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
    FuturesMutex, History, LineState, SafeBool, SafeHistory, SafeLineState, SafeRawTerminal,
    SharedWriter, Text, CHANNEL_CAPACITY,
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
use tokio::sync::mpsc::{channel, Receiver, UnboundedReceiver, UnboundedSender};

// 01: add tests

/// ### Mental model and overview
///
/// This is a replacement for a [std::io::BufRead::read_line] function. It is async. It
/// supports other tasks concurrently writing to the terminal output (via
/// [SharedWriter]s). It also supports being paused so that [crate::Spinner] can display
/// an indeterminate progress spinner. Then it can be resumed so that the user can type in
/// the terminal.
///
/// When you call [Self::readline()] it enters an infinite loop. During which you can type
/// things into the multiline editor, which also displays the prompt. While in this loop
/// other tasks can send messages to the `Readline` task via the `line` channel, using the
/// [`SharedWriter::line_sender`].
///
/// ### Pause and resume
///
/// When the terminal is paused, then any output from the [`SharedWriter`]s will not be
/// printed to the terminal. This is useful when you want to display a spinner, or some
/// other indeterminate progress indicator. When the terminal is resumed, then the output
/// from the [`SharedWriter`]s will be printed to the terminal by the
/// [`Readline::flush()`] method, which drains the channel. This is possible, because
/// while paused, the [`Readline::poll_for_shared_writer_output()`] method doesn't
/// actually do anything. When resumed, the [`Readline::flush()`] method is called, which
/// drains the channel (if there are any messages in it, and prints them out) so nothing
/// is lost!
///
/// The [`Readline::line_receiver`] [tokio::sync::mpsc::Sender], connected to the line
/// channel, is where the [SharedWriter]s send their output to be printed to the terminal.
/// You can send [`LineControlSignal::Line`] to print a line.
///
/// You can send [`LineControlSignal::Flush`] to flush the output. You can also send
/// [`LineControlSignal::Pause`] to pause the `Readline` task, and
/// [`LineControlSignal::Resume`] to resume it. However, for any of these signals to work,
/// the [`Readline::readline()`] method be running. If you want to pause, resume, or flush
/// the output, outside of while [`Readline::readline()`] is running, you can directly
/// call the methods: [`Readline::pause()`], [`Readline::resume()`], and
/// [`Readline::flush()`].
///
/// ### Usage details
///
/// Struct for reading lines of input from a terminal while lines are output to the
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
///
/// You can provide your own implementation of [SafeRawTerminal], via [dependency
/// injection](https://developerlife.com/category/DI/), so that you can mock terminal
/// output for testing. You can also extend this struct to adapt your own terminal output
/// using this mechanism. Essentially anything that complies with `dyn std::io::Write +
/// Send` trait bounds can be used.
pub struct Readline {
    /// Raw terminal implementation, you can supply this via dependency injection.
    pub raw_terminal: SafeRawTerminal,

    /// Stream of events.
    pub event_stream: EventStream,

    /// Receiver end of the channel, the sender end is in [`SharedWriter`], which does the
    /// actual writing to the terminal. This is only monitored actively while
    /// [`Readline::readline()`] is active. It is also drained passively everytime
    /// [`Readline::flush()`] is called.
    pub line_receiver: Receiver<LineControlSignal>,

    /// Current line.
    pub safe_line_state: SafeLineState,

    /// Use to send history updates.
    pub history_sender: UnboundedSender<String>,
    /// Use to recieve history updates.
    pub history_receiver: UnboundedReceiver<String>,
    /// Manages the history.
    pub safe_history: SafeHistory,

    /// Determines whether terminal is paused or not. When paused, concurrent output
    /// via [`SharedWriter`]s is not printed to the terminal.
    ///
    /// - Affects
    ///   - [`Readline::readline()`],
    ///   - [`Readline::flush()`], and
    ///   - [`Readline::poll_for_shared_writer_output()`].
    /// - Also see
    ///   - [`Self::is_paused()`],
    ///   - [`Self::pause()`], and
    ///   - [`Self::resume()`].
    pub safe_is_paused: SafeBool,
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

/// For these to work, the [Readline::readline()] method must be running. Since the
/// [Readline::line_receiver] is used to send these signals. And it is only monitored when
/// the [Readline::readline()] method is running.
#[derive(Debug, PartialEq, Clone)]
pub enum LineControlSignal {
    Line(Text),
    Flush,
    Pause,
    Resume,
}

/// Internal control flow for the `readline` method. This is used primarly to make testing
/// easier.
#[derive(Debug, PartialEq, Clone)]
pub enum InternalControlFlow<T, E> {
    ReturnOk(T),
    ReturnError(E),
    Continue,
}

impl Readline {
    /// Create a new instance with an associated [`SharedWriter`]. To customize the
    /// behavior of this instance, you can use the following methods:
    /// - [Self::should_print_line_on]
    /// - [Self::set_max_history]
    pub async fn new(
        prompt: String,
        raw_terminal: SafeRawTerminal,
    ) -> Result<(Self, SharedWriter), ReadlineError> {
        // Line channel.
        let line_channel = channel::<LineControlSignal>(CHANNEL_CAPACITY);
        let (line_sender, line_receiver) = line_channel;

        // Paused state.
        let safe_is_paused = Arc::new(FuturesMutex::new(false));

        // Enable raw mode. Drop will disable raw mode.
        terminal::enable_raw_mode()?;

        // History setup.
        let (history, history_receiver) = History::new();
        let history_sender = history.sender.clone();
        let safe_history = Arc::new(FuturesMutex::new(history));

        // Line state.
        let line_state = LineState::new(prompt, terminal::size()?);

        // Create the instance with all the supplied components.
        let readline = Readline {
            raw_terminal,
            event_stream: EventStream::new(),
            line_receiver,
            safe_line_state: Arc::new(FuturesMutex::new(line_state)),
            history_sender,
            safe_is_paused,
            history_receiver,
            safe_history,
        };

        // Print the prompt.
        readline
            .safe_line_state
            .lock()
            .await
            .render(&mut *readline.raw_terminal.lock().await)?;
        readline
            .raw_terminal
            .lock()
            .await
            .queue(terminal::EnableLineWrap)?;
        readline.raw_terminal.lock().await.flush()?;

        // Create the shared writer.
        let shared_writer = SharedWriter {
            line_sender,
            buffer: Vec::new(),
        };

        // Return the instance and the shared writer.
        Ok((readline, shared_writer))
    }

    /// Change the prompt.
    pub async fn update_prompt(&mut self, prompt: &str) -> Result<(), ReadlineError> {
        self.safe_line_state
            .lock()
            .await
            .update_prompt(prompt, &mut *self.raw_terminal.lock().await)?;
        Ok(())
    }

    /// Clear the screen.
    pub async fn clear(&mut self) -> Result<(), ReadlineError> {
        self.raw_terminal
            .lock()
            .await
            .queue(Clear(terminal::ClearType::All))?;
        self.safe_line_state
            .lock()
            .await
            .clear_and_render(&mut *self.raw_terminal.lock().await)?;
        self.raw_terminal.lock().await.flush()?;
        Ok(())
    }

    /// Set maximum history length. The default length is [crate::HISTORY_SIZE_MAX].
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
        if self.is_paused().await {
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
                    if let LineControlSignal::Line(buf) = buf {
                        self.safe_line_state
                            .lock()
                            .await
                            .print_data(&buf, &mut *self.raw_terminal.lock().await)?;
                    }
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
            .clear(&mut *self.raw_terminal.lock().await)?;
        self.raw_terminal.lock().await.flush()?;

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
                        &mut *self.raw_terminal.lock().await,
                        self.safe_history.clone()
                    ).await {
                        InternalControlFlow::ReturnOk(ok_value) => {return Ok(ok_value);},
                        InternalControlFlow::ReturnError(err_value) => {return Err(err_value);},
                        InternalControlFlow::Continue => {}
                    }
                },

                // Poll for output from `SharedWriter`s (cloned `stdout`s).
                maybe_line_control_signal = self.line_receiver.recv().fuse() => {
                    match self.poll_for_shared_writer_output(
                        maybe_line_control_signal,
                    ).await {
                        InternalControlFlow::ReturnError(err_value) => { return Err(err_value); },
                        InternalControlFlow::Continue => {}
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

impl Readline {
    /// This method doesn't do anything when the instance is paused. This is what allows
    /// [`Self::flush()`] to work correctly, since it drains any items that are in this
    /// channel when the instance is resumed, and `flush()` is called.
    pub async fn poll_for_shared_writer_output(
        &mut self,
        maybe_line_control_signal: Option<LineControlSignal>,
    ) -> InternalControlFlow<(), ReadlineError> {
        let self_line_state = self.safe_line_state.clone();
        let self_raw_term = self.raw_terminal.clone();

        // If paused, then return!
        if self.is_paused().await {
            return InternalControlFlow::Continue;
        }

        match maybe_line_control_signal {
            Some(line_control_signal) => match line_control_signal {
                LineControlSignal::Line(buf) => {
                    if let Err(err) = self_line_state
                        .lock()
                        .await
                        .print_data(&buf, &mut *self_raw_term.lock().await)
                    {
                        return InternalControlFlow::ReturnError(err);
                    }
                    if let Err(err) = self_raw_term.lock().await.flush() {
                        return InternalControlFlow::ReturnError(err.into());
                    }
                }
                LineControlSignal::Flush => {
                    let _ = self.flush().await;
                }
                LineControlSignal::Pause => {
                    self.pause().await;
                }
                LineControlSignal::Resume => {
                    self.resume().await;
                }
            },
            None => return InternalControlFlow::ReturnError(ReadlineError::Closed),
        }

        InternalControlFlow::Continue
    }
}

pub mod readline_internal {
    use super::*;

    pub async fn process_event(
        maybe_result_crossterm_event: Option<Result<Event, Error>>,
        self_line_state: SafeLineState,
        self_raw_term: &mut dyn Write,
        self_safe_history: SafeHistory,
    ) -> InternalControlFlow<ReadlineEvent, ReadlineError> {
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
                                return InternalControlFlow::ReturnError(e.into());
                            }
                            if let Some(readline_event) = maybe_readline_event {
                                return InternalControlFlow::ReturnOk(readline_event);
                            }
                        }
                        Err(e) => return InternalControlFlow::ReturnError(e),
                    }
                }
                Err(e) => return InternalControlFlow::ReturnError(e.into()),
            }
        }
        InternalControlFlow::Continue
    }
}

/// Exit raw mode when the instance is dropped.
impl Drop for Readline {
    /// There is no need to call [Readline::close()] since as soon as the
    /// [`Readline::line_receiver`] is dropped, it will shutdown its channel.
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Pauses and resumes the readline instance.
impl Readline {
    pub async fn is_paused(&self) -> bool {
        *self.safe_is_paused.lock().await
    }

    /// There are 2 ways to pause the readline instance:
    /// 1. By calling this method.
    /// 2. By sending a `LineControlSignal::Pause` to the `line_sender`. However, this
    ///    requires that the [Readline::readline()] method is called, and that the
    ///    `line_sender` is still alive. Alternatively the
    ///    [crate::TerminalAsync::get_readline_event()] method can be called, which will
    ///    call [Readline::readline()].
    pub async fn pause(&mut self) {
        *self.safe_is_paused.lock().await = true;
    }

    /// There are 2 ways to resume the readline instance:
    /// 1. By calling this method.
    /// 2. By sending a `LineControlSignal::Resume` to the `line_sender`. However, this
    ///   requires that the [Readline::readline()] method is called, and that the
    ///  `line_sender` is still alive. Alternatively the
    /// [crate::TerminalAsync::get_readline_event()] method can be called, which will call
    /// [Readline::readline()].
    pub async fn resume(&mut self) {
        *self.safe_is_paused.lock().await = false;
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
    use super::*;
    use crate::StdMutex;
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
    async fn test_readline_process_event_and_terminal_output() {
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
            &mut *readline.raw_terminal.lock().await,
            safe_history.clone(),
        )
        .await;

        assert!(matches!(control_flow, InternalControlFlow::Continue));
        assert_eq!(readline.safe_line_state.lock().await.line, "a");

        let output_buffer_data = stdout_mock.buffer.lock().unwrap();
        let output_buffer_data = strip(output_buffer_data.to_vec());
        let output_buffer_data = String::from_utf8(output_buffer_data).expect("utf8");
        // println!("\n`{}`\n", output_buffer_data);
        assert!(output_buffer_data.contains("> a"));
    }

    pub(super) fn get_input_vec() -> Vec<Event> {
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

#[cfg(test)]
mod test_streams {
    use super::tests::get_input_vec;
    use super::*;

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
}

// 00: clean this up
#[cfg(test)]
mod tests_terminal_writer_exp {
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
