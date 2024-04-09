/*
 *   Copyright (c) 2024 R3BL LLC
 *   All rights reserved.
 *
 *   Licensed under the Apache License, Version 2.0 (the "License");
 *   you may not use this file except in compliance with the License.
 *   You may obtain a copy of the License at
 *
 *   http://www.apache.org/licenses/LICENSE-2.0
 *
 *   Unless required by applicable law or agreed to in writing, software
 *   distributed under the License is distributed on an "AS IS" BASIS,
 *   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *   See the License for the specific language governing permissions and
 *   limitations under the License.
 */

use crate::{
    FuturesMutex, History, LineState, SafeBool, SafeHistory, SafeLineState, SafeRawTerm,
    SharedWriter,
};
use crossterm::{
    event::{Event, EventStream},
    terminal::{self, disable_raw_mode, Clear},
    QueueableCommand,
};
use futures_util::{select, stream::StreamExt, FutureExt};
use std::{
    io::{self, Error, Write},
    sync::Arc,
};
use thiserror::Error;
use tokio::task::JoinHandle;

// 01: add tests

pub type Text = Vec<u8>;
pub const CHANNEL_CAPACITY: usize = 500;
pub const HISTORY_SIZE_MAX: usize = 1000;

/// Structure for reading lines of input from a terminal while lines are output to the
/// terminal concurrently.
///
/// Terminal input is retrieved by calling [`Readline::readline()`], which returns each
/// complete line of input once the user presses Enter.
///
/// Each `Readline` instance is associated with one or more [`SharedWriter`] instances.
///
/// Lines written to an associated `SharedWriter` are output:
/// 1. While retrieving input with [`readline()`][Readline::readline].
/// 2. By calling [`flush()`][Readline::flush].
pub struct Readline {
    pub raw_term: SafeRawTerm,

    /// Stream of events.
    pub event_stream: EventStream,

    pub line_receiver: tokio::sync::mpsc::Receiver<Text>,

    /// Current line.
    // 00: rename to safe_line_state
    pub safe_line_state: SafeLineState,

    pub history_sender: tokio::sync::mpsc::UnboundedSender<String>,

    /// - Affects
    ///   - [`Readline::readline()`],
    ///   - [`Readline::flush()`], and
    ///   - [`readline_internal::poll_for_shared_writer_output()`].
    /// - Also see
    ///   - [`Self::is_suspended()`],
    ///   - [`Self::suspend()`], and
    ///   - [`Self::resume()`].
    // 00: rename to safe_is_suspended
    pub safe_is_suspended: SafeBool,
    pub flush_signal_sender: tokio::sync::mpsc::Sender<ReadlineFlushSignal>,
    pub monitor_flush_signal_receiver_task: tokio::task::JoinHandle<()>,

    pub history_receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
    pub safe_history: SafeHistory,
}

/// Error returned from [`readline()`][Readline::readline]. Such errors generally require
/// specific procedures to recover from.
#[derive(Debug, Error)]
pub enum ReadlineError {
    /// An internal I/O error occurred.
    #[error(transparent)]
    IO(#[from] io::Error),

    /// `readline()` was called after the [`SharedWriter`] was dropped and everything
    /// written to the `SharedWriter` was already output.
    #[error("line writers closed")]
    Closed,
}

/// Events emitted by [`Readline::readline()`].
#[derive(Debug, PartialEq, Clone)]
pub enum ReadlineEvent {
    /// The user entered a line of text.
    Line(String),

    /// The user pressed Ctrl-D.
    Eof,

    /// The user pressed Ctrl-C.
    Interrupted,
}

#[derive(Debug, PartialEq, Clone)]
pub enum ReadlineFlushSignal {
    /// Flush the buffer.
    Flush,
    /// Suspend the `Readline` instance.
    Suspend,
    /// Resume the `Readline` instance.
    Resume,
}

// 00: clean this up
#[cfg(test)]
mod stdout_exp {
    use crossterm::{queue, terminal};
    use miette::IntoDiagnostic;
    use std::io::{stdout, Write};

    #[test]
    fn test_stand_ins_for_stdout() -> miette::Result<()> {
        let stdout = stdout();
        do_with_stdout(stdout)?;
        Ok(())
    }

    fn do_with_stdout(mut write: impl Write) -> miette::Result<()> {
        queue! {
            write,
            terminal::EnableLineWrap
        }
        .into_diagnostic()?;
        Ok(())
    }
}

impl Readline {
    /// Take care of flushing the terminal when a signal is received. This works hand in
    /// hand with [LineState]. Some of the variables are moved into this task for the
    /// lifecycle of the struct.
    pub fn start_monitor_flush_signal_task(
        raw_term_clone: SafeRawTerm,
        is_suspended_clone: SafeBool,
        mut flush_signal_receiver: tokio::sync::mpsc::Receiver<ReadlineFlushSignal>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            // 00: do something with the flush_signal_receiver
            loop {
                let maybe_signal = flush_signal_receiver.recv().await;
                match maybe_signal {
                    Some(signal) => match signal {
                        ReadlineFlushSignal::Flush => {
                            eprintln!("\nflushing\n");
                            let _ = raw_term_clone.lock().await.flush();
                        }
                        ReadlineFlushSignal::Suspend => {
                            eprintln!("\nsuspending\n");
                            *is_suspended_clone.lock().await = true;
                        }
                        ReadlineFlushSignal::Resume => {
                            eprintln!("\nresuming\n");
                            *is_suspended_clone.lock().await = false;
                            let _ = raw_term_clone.lock().await.flush();
                        }
                    },
                    None => break,
                }
            }
        })
    }

    /// Create a new instance with an associated [`SharedWriter`]. You can try out some of
    /// the following configuration options:
    /// - [Self::should_print_line_on]
    /// - [Self::set_max_history]
    pub async fn new(
        prompt: String,
        raw_term: SafeRawTerm,
    ) -> Result<(Self, SharedWriter), ReadlineError> {
        let (line_sender, line_receiver) = tokio::sync::mpsc::channel::<Text>(CHANNEL_CAPACITY);
        let (flush_signal_sender, flush_signal_receiver) =
            tokio::sync::mpsc::channel::<ReadlineFlushSignal>(CHANNEL_CAPACITY);

        let is_suspended = Arc::new(FuturesMutex::new(false));

        terminal::enable_raw_mode()?;

        let (history, history_receiver) = History::new();
        let history_sender = history.sender.clone();
        let safe_history = Arc::new(FuturesMutex::new(history));

        let line_state = LineState::new(prompt, terminal::size()?);

        // 00: move this into its oww method below
        // Spawn a task to monitor the flush signal channel. Some variables will be moved
        // there permanently for the lifecycle of this struct.
        let monitor_flush_signal_receiver_task = Self::start_monitor_flush_signal_task(
            raw_term.clone(),
            is_suspended.clone(),
            /* this gets moved*/ flush_signal_receiver,
        );

        let readline = Readline {
            raw_term,
            event_stream: EventStream::new(),
            line_receiver,
            safe_line_state: Arc::new(FuturesMutex::new(line_state)),
            history_sender,
            safe_is_suspended: is_suspended,
            flush_signal_sender,
            monitor_flush_signal_receiver_task,
            history_receiver,
            safe_history,
        };

        readline
            .safe_line_state
            .lock()
            .await
            .render(&mut *readline.raw_term.lock().await)?;
        readline
            .raw_term
            .lock()
            .await
            .queue(terminal::EnableLineWrap)?;
        readline.raw_term.lock().await.flush()?;

        let shared_writer = SharedWriter {
            line_sender,
            buffer: Vec::new(),
        };

        Ok((readline, shared_writer))
    }

    /// Change the prompt.
    pub async fn update_prompt(&mut self, prompt: &str) -> Result<(), ReadlineError> {
        self.safe_line_state
            .lock()
            .await
            .update_prompt(prompt, &mut *self.raw_term.lock().await)?;
        Ok(())
    }

    /// Clear the screen.
    pub async fn clear(&mut self) -> Result<(), ReadlineError> {
        self.raw_term
            .lock()
            .await
            .queue(Clear(terminal::ClearType::All))?;
        self.safe_line_state
            .lock()
            .await
            .clear_and_render(&mut *self.raw_term.lock().await)?;
        self.raw_term.lock().await.flush()?;
        Ok(())
    }

    /// Set maximum history length. The default length is [HISTORY_SIZE_MAX].
    pub async fn set_max_history(&mut self, max_size: usize) {
        let mut history = self.safe_history.lock().await;
        history.max_size = max_size;
        history.entries.truncate(max_size);
    }

    /// Set whether the input line should remain on the screen after events.
    ///
    /// If `enter` is true, then when the user presses "Enter", the prompt and the text
    /// they entered will remain on the screen, and the cursor will move to the next line.
    /// If `enter` is false, the prompt & input will be erased instead.
    ///
    /// `control_c` similarly controls the behavior for when the user presses Ctrl-C.
    ///
    /// The default value for both settings is `true`.
    pub async fn should_print_line_on(&mut self, enter: bool, control_c: bool) {
        let mut line_state = self.safe_line_state.lock().await;
        line_state.should_print_line_on_enter = enter;
        line_state.should_print_line_on_control_c = control_c;
    }

    /// Flush all writers to terminal and erase the prompt string.
    pub async fn flush(&mut self) -> Result<(), ReadlineError> {
        if self.is_suspended().await {
            return Ok(());
        }

        // Break out of the loop if the channel is:
        // 1. closed - This loop does not block when the channel is empty and there's
        //    nothing to flush.
        // 2. empty - This is why we use `try_recv()` here, and not `recv()` which would
        //    block this loop, when the channel is empty.
        loop {
            // `try_recv()` will produce an error when the channel is empty or closed.
            let result = self.line_receiver.try_recv();
            match result {
                // Got some data, print it.
                Ok(buf) => {
                    self.safe_line_state
                        .lock()
                        .await
                        .print_data(&buf, &mut *self.raw_term.lock().await)?;
                }
                // Closed or empty.
                Err(_) => {
                    break;
                }
            }
        }

        self.safe_line_state
            .lock()
            .await
            .clear(&mut *self.raw_term.lock().await)?;
        self.raw_term.lock().await.flush()?;

        Ok(())
    }

    /// Polling function for `readline`, manages all input and output. Returns either an
    /// [ReadlineEvent] or an [ReadlineError].
    pub async fn readline(&mut self) -> miette::Result<ReadlineEvent, ReadlineError> {
        loop {
            select! {
                // Poll for events.
                maybe_result_crossterm_event = self.event_stream.next().fuse() => {
                    match readline_internal::process_event(
                        maybe_result_crossterm_event,
                        self.safe_line_state.clone(),
                        &mut *self.raw_term.lock().await,
                        self.safe_history.clone()
                    ).await {
                        ControlFlow::ReturnOk(ok_value) => {return Ok(ok_value);},
                        ControlFlow::ReturnError(err_value) => {return Err(err_value);},
                        ControlFlow::Continue => {}
                    }
                },

                // Poll for output from `SharedWriter`s (cloned `stdout`s).
                result = self.line_receiver.recv().fuse() => {
                    match poll_for_shared_writer_output(
                        self.is_suspended().await,
                        result,
                        self.safe_line_state.clone(),
                        &mut *self.raw_term.lock().await
                    ).await {
                        ControlFlow::ReturnError(err_value) => { return Err(err_value); },
                        ControlFlow::Continue => {}
                        _ => { unreachable!(); }
                    }
                },

                // Poll for history updates.
                maybe_line = self.history_receiver.recv().fuse() => {
                    self.safe_history.lock().await.update(maybe_line).await;
                }
            }
        }
    }

    /// Add a line to the input history.
    pub fn add_history_entry(&mut self, entry: String) -> Option<()> {
        self.history_sender.send(entry).ok()
    }
}

pub mod readline_internal {
    use super::*;

    #[derive(Debug, PartialEq, Clone)]
    pub enum ControlFlow<T, E> {
        ReturnOk(T),
        ReturnError(E),
        Continue,
    }

    pub async fn poll_for_shared_writer_output(
        is_suspended: bool,
        result: Option<Text>,
        self_line_state: SafeLineState,
        self_raw_term: &mut dyn Write,
    ) -> ControlFlow<(), ReadlineError> {
        if is_suspended {
            return ControlFlow::Continue;
        }

        match result {
            Some(buf) => {
                if let Err(err) = self_line_state.lock().await.print_data(&buf, self_raw_term) {
                    return ControlFlow::ReturnError(err);
                }
                if let Err(err) = self_raw_term.flush() {
                    return ControlFlow::ReturnError(err.into());
                }
            }
            None => return ControlFlow::ReturnError(ReadlineError::Closed),
        }

        ControlFlow::Continue
    }

    pub async fn process_event(
        maybe_result_crossterm_event: Option<Result<Event, Error>>,
        self_line_state: SafeLineState,
        self_raw_term: &mut dyn Write,
        self_safe_history: SafeHistory,
    ) -> ControlFlow<ReadlineEvent, ReadlineError> {
        if let Some(result_crossterm_event) = maybe_result_crossterm_event {
            match result_crossterm_event {
                Ok(crossterm_event) => {
                    let mut it = self_line_state.lock().await;
                    let result_maybe_readline_event = it
                        .handle_event(crossterm_event, self_raw_term, self_safe_history)
                        .await;
                    match result_maybe_readline_event {
                        Ok(maybe_readline_event) => {
                            if let Err(e) = self_raw_term.flush() {
                                return ControlFlow::ReturnError(e.into());
                            }
                            if let Some(readline_event) = maybe_readline_event {
                                return ControlFlow::ReturnOk(readline_event);
                            }
                        }
                        Err(e) => return ControlFlow::ReturnError(e),
                    }
                }
                Err(e) => return ControlFlow::ReturnError(e.into()),
            }
        }
        ControlFlow::Continue
    }
}
use readline_internal::*;

/// Exit raw mode when the instance is dropped.
impl Drop for Readline {
    /// There is no need to call [Readline::close()] since as soon as the
    /// [`Readline::line_receiver`] is dropped, it will shutdown its channel.
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Suspends and resumes the readline instance.
impl Readline {
    pub async fn is_suspended(&self) -> bool {
        *self.safe_is_suspended.lock().await
    }

    pub async fn suspend(&mut self) {
        *self.safe_is_suspended.lock().await = true;
    }

    pub async fn resume(&mut self) {
        *self.safe_is_suspended.lock().await = false;
        // 00: clean this up
        let _ = self.flush().await;
    }
}

impl Readline {
    /// Call this to shutdown the [tokio::sync::mpsc::Receiver] and thus the channel
    /// [tokio::sync::mpsc::channel]. Typically this happens when your CLI wants to exit,
    /// due to some user input requesting this. This will result in any awaiting tasks in
    /// various places to error out, which is the desired behavior, rather than just
    /// hanging, waiting on events that will never happen.
    pub fn close(&mut self) {
        self.line_receiver.close();
    }
}

#[cfg(test)]
mod tests {
    use crate::StdMutex;

    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use strip_ansi_escapes::strip;

    #[derive(Clone)]
    pub struct StdoutMock {
        pub buffer: Arc<StdMutex<Vec<u8>>>,
    }

    impl Write for StdoutMock {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_readline_process_event() {
        let vec = get_input_vec();
        let mut iter = vec.iter();

        let prompt_str = "> ";

        let output_buffer = Vec::new();
        let stdout_mock = StdoutMock {
            buffer: Arc::new(std::sync::Mutex::new(output_buffer)),
        };

        // We will get the `line_state` out of this to test.
        let (readline, _) = Readline::new(
            prompt_str.into(),
            Arc::new(FuturesMutex::new(stdout_mock.clone())),
        )
        .await
        .unwrap();

        let history = History::new();
        let safe_history = Arc::new(FuturesMutex::new(history.0));

        // Simulate 'a'.
        let event = iter.next().unwrap();
        let control_flow = readline_internal::process_event(
            Some(Ok(event.clone())),
            readline.safe_line_state.clone(),
            &mut *readline.raw_term.lock().await,
            safe_history.clone(),
        )
        .await;

        assert!(matches!(control_flow, ControlFlow::Continue));
        assert_eq!(readline.safe_line_state.lock().await.line, "a");

        let output_buffer_data = stdout_mock.buffer.lock().unwrap();
        let output_buffer_data = strip(output_buffer_data.to_vec());
        let output_buffer_data = String::from_utf8(output_buffer_data).expect("utf8");
        println!("\n`{}`\n", output_buffer_data);

        assert_eq!(output_buffer_data, "> > a");
    }

    // 00: use this as inspiration to change readline, so that it can accept a param of type Stream<Item = T>
    #[tokio::test]
    async fn test_generate_event_stream() {
        use async_stream::stream;
        use futures_core::stream::Stream;
        use futures_util::pin_mut;
        use futures_util::stream::StreamExt;

        fn gen_stream() -> impl Stream<Item = Event> {
            stream! {
                for event in get_input_vec() {
                    yield event;
                }
            }
        }

        let stream = gen_stream();
        pin_mut!(stream);

        let mut count = 0;
        while let Some(event) = stream.next().await {
            assert_eq!(event, get_input_vec()[count]);
            count += 1;
        }
    }

    fn get_input_vec() -> Vec<Event> {
        vec![
            // a
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            // b
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)),
            // c
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)),
            // enter
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ]
    }
}
