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

use crate::SafeVecText;
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
use miette::IntoDiagnostic;
use std::{
    io::{self, Error, Write},
    sync::Arc,
};
use thiserror::Error;
use tokio::sync::mpsc::Receiver;
use tokio::{
    sync::mpsc::{channel, Sender, UnboundedSender},
    task::JoinHandle,
};

// 01: add tests

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
///
/// When you call [Self::new()] it kicks off a Tokio task via
/// [monitor_signal_task::start_monitor_control_signal_task()]. This task is responsible
/// for monitoring the control signal channel and the line receiver channel.
/// 1. The control signal channel is where messages or signals (from [Sender]s) are sent
///    to the task to perform some action. For example, when you call [Readline::flush()],
///    it sends a signal to the task to flush the terminal.
/// 2. The line receiver [Sender], connected to the line channel, is where the
///    [SharedWriter]s send their output to be printed to the terminal. This task is
///    responsible for printing the output to the terminal, and also for flushing the
///    terminal.
pub struct Readline {
    /// Used to write output to the terminal. This can be anything that complies with `dyn
    /// std::io::Write + Send` trait bounds.
    pub raw_terminal: SafeRawTerminal,

    /// Stream of events. This comes in from the OS, via crossterm crate, and is used to
    /// process user input (keystrokes) when the terminal is put into raw mode.
    pub event_stream: EventStream,

    /// Current line.
    pub safe_line_state: SafeLineState,

    /// Use this to send control signals (for suspend, resume, flush, etc). Here are all
    /// the signals: [ReadlineControlSignal].
    pub control_signal_sender: Sender<ReadlineControlSignal>,

    /// Task that monitors the control signal channel and the line receiver channel.
    pub monitor_flush_signal_receiver_task: JoinHandle<()>,

    /// Use this to send history entries.
    pub history_sender: UnboundedSender<String>,
    /// Use this to receive history entries. In the [Self::readline()] method, we poll
    /// this channel for history entries.
    pub history_receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
    /// Use struct is manages the history.
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

/// Signals that can be sent to a [`Readline`] instance.
#[derive(Debug, PartialEq, Clone)]
pub enum ReadlineControlSignal {
    /// Flush the buffer.
    Flush,
    /// Suspend the `Readline` instance.
    Suspend,
    /// Resume the `Readline` instance.
    Resume,
    /// Close the `Readline` instance.
    Close,
}

impl Readline {
    /// Create a new instance with an associated [`SharedWriter`]. To customize the
    /// behavior of this instance, use the following methods:
    /// - [Self::should_print_line_on]
    /// - [Self::set_max_history]
    pub async fn new(
        prompt: String,
        raw_term: SafeRawTerminal,
    ) -> Result<(Self, SharedWriter), ReadlineError> {
        // Line channel.
        let line_channel = channel::<Text>(CHANNEL_CAPACITY);
        let (line_sender, line_receiver) = line_channel;

        // Control signal channel.
        let control_signal_channel = channel::<ReadlineControlSignal>(CHANNEL_CAPACITY);
        let (control_signal_sender, control_signal_receiver) = control_signal_channel;

        // Set up the history.
        let (history, history_receiver) = History::new();
        let history_sender = history.sender.clone();
        let safe_history = Arc::new(FuturesMutex::new(history));

        // Set up the line state.
        let line_state = LineState::new(prompt, terminal::size()?);
        let safe_line_state = Arc::new(FuturesMutex::new(line_state));

        // Enable raw mode. The Drop trait implementation will disable raw mode.
        terminal::enable_raw_mode()?;

        // Spawn a task to monitor the flush signal channel. Some variables will be moved
        // there permanently for the lifecycle of this struct.
        let monitor_flush_signal_receiver_task =
            monitor_signal_task::start_monitor_control_signal_task(
                raw_term.clone(),
                safe_line_state.clone(),
                /* this gets moved */ control_signal_receiver,
                /* this gets moved */ line_receiver,
            );

        // Assemble the `Readline` struct.
        let readline = Readline {
            raw_terminal: raw_term,
            event_stream: EventStream::new(),
            safe_line_state,
            control_signal_sender,
            monitor_flush_signal_receiver_task,
            history_sender,
            history_receiver,
            safe_history,
        };

        // Render the prompt.
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

        // Create a `SharedWriter` instance.
        let shared_writer = SharedWriter {
            line_sender,
            buffer: Vec::new(),
        };

        // Return the `Readline` instance and the `SharedWriter`.
        Ok((readline, shared_writer))
    }
}

pub mod monitor_signal_task {
    use super::*;

    /// Spawn a long running Tokio task, which runs for the lifetime of the [Readline]
    /// instance created using [Readline::new()].
    ///
    /// 1. This task monitors the control signal channel, and handles flushing the
    ///    terminal when a signal is received, along with pausing and resuming the
    ///    terminal.
    /// 2. It also monitors the line channel, and prints the output to the terminal.
    ///
    /// It works hand in hand with [LineState]. Some of the variables are moved into this
    /// task for the lifecycle of the struct.
    pub fn start_monitor_control_signal_task(
        safe_raw_terminal: SafeRawTerminal,
        safe_line_state: SafeLineState,
        mut control_signal_receiver: Receiver<ReadlineControlSignal>, /* This is moved */
        mut line_receiver: Receiver<Text>,                            /* This is moved */
    ) -> JoinHandle<()> {
        // Task specific state, used to pause and resume the terminal. And queue the
        // output that is generated (by other SharedWriters) while the terminal is paused.
        let safe_queue_paused_text = Arc::new(FuturesMutex::new(Vec::<Text>::new()));
        let safe_is_terminal_paused = Arc::new(FuturesMutex::new(false));

        tokio::spawn(async move {
            loop {
                select! {
                    // Poll for output from `SharedWriter`s (cloned `stdout`s).
                    maybe_text = line_receiver.recv().fuse() => {
                        match maybe_text {
                            // Got some data, print it.
                            Some(text) => {
                                handle_text(
                                    text,
                                    safe_line_state.clone(),
                                    safe_raw_terminal.clone(),
                                    safe_is_terminal_paused.clone(),
                                    safe_queue_paused_text.clone()
                                ).await;
                            }
                            // line_receiver is closed.
                            None => break,
                        }
                    }

                    // Poll for signals.
                    maybe_signal = control_signal_receiver.recv().fuse() => {
                        match maybe_signal {
                            Some(signal) => {
                                handle_signal(
                                    signal,
                                    safe_raw_terminal.clone(),
                                    safe_line_state.clone(),
                                    safe_is_terminal_paused.clone(),
                                    safe_queue_paused_text.clone(),
                                    &mut line_receiver,
                                )
                                .await
                            }
                            None => break,
                        }
                    }
                }
            }
        })
    }

    /// This function is sensitive to suspension. It is used to handle control signals
    /// sent to the [Readline] instance.
    async fn handle_signal(
        signal: ReadlineControlSignal,
        safe_raw_term: SafeRawTerminal,
        safe_line_state: SafeLineState,
        safe_is_suspended: SafeBool,
        safe_queue_paused_text: SafeVecText,
        line_receiver: &mut Receiver<Text>,
    ) {
        match signal {
            ReadlineControlSignal::Flush => {
                flush_internal(
                    safe_raw_term.clone(),
                    safe_line_state.clone(),
                    safe_queue_paused_text.clone(),
                )
                .await;
            }

            ReadlineControlSignal::Suspend => {
                *safe_is_suspended.lock().await = true;
            }

            ReadlineControlSignal::Resume => {
                *safe_is_suspended.lock().await = false;
                flush_internal(
                    safe_raw_term.clone(),
                    safe_line_state.clone(),
                    safe_queue_paused_text.clone(),
                )
                .await;
            }

            ReadlineControlSignal::Close => {
                line_receiver.close();
            }
        }
    }

    /// This function is sensitive to suspension. It is used to handle text output from
    /// the [SharedWriter]s.
    async fn handle_text(
        text: Text,
        safe_line_state: SafeLineState,
        safe_raw_term: SafeRawTerminal,
        safe_is_suspended: SafeBool,
        safe_queue_paused_text: SafeVecText,
    ) {
        // If the terminal is suspended, queue the output. This will be printed later when
        // the terminal is resumed.
        if *safe_is_suspended.lock().await {
            safe_queue_paused_text.lock().await.push(text);
            return;
        }

        // Print the output.
        let _ = safe_line_state
            .lock()
            .await
            .print_data(&text, &mut *safe_raw_term.lock().await);

        // Clear the line and print the prompt.
        let _ = safe_line_state
            .lock()
            .await
            .clear_and_render(&mut *safe_raw_term.lock().await);

        // Flush the terminal.
        let _ = safe_raw_term.lock().await.flush();
    }

    /// Drain and print the `safe_queue_paused_text`, if there is any content in it. When
    /// the terminal is paused, the output generated by the [SharedWriter]s is queued up
    /// in the `safe_queue_paused_text`.
    async fn drain_queue_paused_text_and_print(
        safe_line_state: SafeLineState,
        safe_raw_term: SafeRawTerminal,
        safe_queue_paused_text: SafeVecText,
    ) {
        if safe_queue_paused_text.lock().await.is_empty() {
            return;
        }
        for text in safe_queue_paused_text.lock().await.iter() {
            let _ = safe_line_state
                .lock()
                .await
                .print_data(text, &mut *safe_raw_term.lock().await);
        }
    }

    /// If there are items in the `safe_queue_paused_text`, print them. Then flush the
    /// terminal.
    async fn flush_internal(
        safe_raw_term: SafeRawTerminal,
        safe_line_state: SafeLineState,
        safe_queue_paused_text: SafeVecText,
    ) {
        // Print any [Text]s in the queue.
        drain_queue_paused_text_and_print(
            safe_line_state.clone(),
            safe_raw_term.clone(),
            safe_queue_paused_text.clone(),
        )
        .await;

        // Clear the prompt line.
        let _ = safe_line_state
            .lock()
            .await
            .clear(&mut *safe_raw_term.lock().await);

        // Flush the terminal.
        let _ = safe_raw_term.lock().await.flush();
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
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

impl Readline {
    /// Call this to shutdown the [tokio::sync::mpsc::Receiver] and thus the channel
    /// [tokio::sync::mpsc::channel]. Typically this happens when your CLI wants to exit,
    /// due to some user input requesting this. This will result in any awaiting tasks in
    /// various places to error out, which is the desired behavior, rather than just
    /// hanging, waiting on events that will never happen.
    pub async fn close(&mut self) {
        let _ = self
            .control_signal_sender
            .send(ReadlineControlSignal::Close)
            .await;
    }
}

impl Readline {
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
    pub async fn flush(&mut self) -> miette::Result<()> {
        self.control_signal_sender
            .send(ReadlineControlSignal::Flush)
            .await
            .into_diagnostic()?;

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
                        ControlFlow::ReturnOk(ok_value) => {return Ok(ok_value);},
                        ControlFlow::ReturnError(err_value) => {return Err(err_value);},
                        ControlFlow::Continue => {}
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

#[cfg(test)]
mod test_streams {
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

        assert!(matches!(control_flow, ControlFlow::Continue));
        assert_eq!(readline.safe_line_state.lock().await.line, "a");

        let output_buffer_data = stdout_mock.buffer.lock().unwrap();
        let output_buffer_data = strip(output_buffer_data.to_vec());
        let output_buffer_data = String::from_utf8(output_buffer_data).expect("utf8");
        // println!("\n`{}`\n", output_buffer_data);

        assert!(output_buffer_data.contains("> a"));
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

// 00: clean this up
#[cfg(test)]
mod test_replace_stdout {
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
