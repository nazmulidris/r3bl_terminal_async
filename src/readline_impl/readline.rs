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

use crate::{LineState, SharedWriter};
use crossterm::{
    event::EventStream,
    terminal::{self, disable_raw_mode, Clear},
    QueueableCommand,
};
use futures_util::{lock::Mutex, select, FutureExt, StreamExt};
use std::{
    io::{self, stdout, Stdout, Write},
    sync::Arc,
};
use thiserror::Error;

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
    raw_term: Stdout,

    /// Stream of events.
    event_stream: EventStream,

    line_receiver: tokio::sync::mpsc::Receiver<Text>,

    /// Current line.
    line: LineState,

    history_sender: tokio::sync::mpsc::UnboundedSender<String>,

    pub(crate) is_suspended: Arc<Mutex<bool>>,
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
#[derive(Debug)]
pub enum ReadlineEvent {
    /// The user entered a line of text.
    Line(String),

    /// The user pressed Ctrl-D.
    Eof,

    /// The user pressed Ctrl-C.
    Interrupted,
}

impl Readline {
    /// Create a new instance with an associated [`SharedWriter`]. You can try out some of
    /// the following configuration options:
    /// - [Self::should_print_line_on]
    /// - [Self::set_max_history]
    pub fn new(prompt: String) -> Result<(Self, SharedWriter), ReadlineError> {
        let (line_sender, line_receiver) = tokio::sync::mpsc::channel::<Text>(CHANNEL_CAPACITY);

        terminal::enable_raw_mode()?;

        let line = LineState::new(prompt, terminal::size()?);
        let history_sender = line.history.sender.clone();

        let mut readline = Readline {
            raw_term: stdout(),
            event_stream: EventStream::new(),
            line_receiver,
            line,
            history_sender,
            is_suspended: Arc::new(Mutex::new(false)),
        };

        readline.line.render(&mut readline.raw_term)?;
        readline.raw_term.queue(terminal::EnableLineWrap)?;
        readline.raw_term.flush()?;

        let shared_writer = SharedWriter {
            line_sender,
            buffer: Vec::new(),
        };

        Ok((readline, shared_writer))
    }

    /// Change the prompt.
    pub fn update_prompt(&mut self, prompt: &str) -> Result<(), ReadlineError> {
        self.line.update_prompt(prompt, &mut self.raw_term)?;
        Ok(())
    }

    /// Clear the screen.
    pub fn clear(&mut self) -> Result<(), ReadlineError> {
        self.raw_term.queue(Clear(terminal::ClearType::All))?;
        self.line.clear_and_render(&mut self.raw_term)?;
        self.raw_term.flush()?;
        Ok(())
    }

    /// Set maximum history length. The default length is [HISTORY_SIZE_MAX].
    pub fn set_max_history(&mut self, max_size: usize) {
        self.line.history.max_size = max_size;
        self.line.history.entries.truncate(max_size);
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
    pub fn should_print_line_on(&mut self, enter: bool, control_c: bool) {
        self.line.should_print_line_on_enter = enter;
        self.line.should_print_line_on_control_c = control_c;
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
                    self.line.print_data(&buf, &mut self.raw_term)?;
                }
                // Closed or empty.
                Err(_) => {
                    break;
                }
            }
        }

        self.line.clear(&mut self.raw_term)?;
        self.raw_term.flush()?;

        Ok(())
    }

    /// Polling function for readline, manages all input and output. Returns either an
    /// Readline Event or an Error.
    pub async fn readline(&mut self) -> Result<ReadlineEvent, ReadlineError> {
        loop {
            select! {
                // Poll for events.
                event = self.event_stream.next().fuse() => match event {
                    Some(Ok(event)) => {
                        let result_maybe_readline_event: Result<Option<ReadlineEvent>, ReadlineError> =
                            self.line.handle_event(event, &mut self.raw_term);
                        match result_maybe_readline_event {
                            Ok(Some(readline_event)) => {
                                self.raw_term.flush()?;
                                return Ok(readline_event)
                            },
                            Ok(None) => self.raw_term.flush()?,
                            Err(e) => return Err(e),
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => {},
                },

                // Poll for input.
                result = self.line_receiver.recv().fuse() => {
                    if self.is_suspended().await {
                        continue;
                    }
                    match result {
                        Some(buf) => {
                                self.line.print_data(&buf, &mut self.raw_term)?;
                                self.raw_term.flush()?;
                        },
                        None => return Err(ReadlineError::Closed),
                    }
                },

                // Poll for history updates.
                _ = self.line.history.update().fuse() => {
                    // Do nothing, just wait for the history to update.
                }
            }
        }
    }

    /// Add a line to the input history.
    pub fn add_history_entry(&mut self, entry: String) -> Option<()> {
        self.history_sender.send(entry).ok()
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

/// Suspends and resumes the readline instance.
impl Readline {
    pub async fn is_suspended(&self) -> bool {
        *self.is_suspended.lock().await
    }

    pub async fn suspend(&mut self) {
        let mut is_suspended = self.is_suspended.lock().await;
        *is_suspended = true;
    }

    pub async fn resume(&mut self) {
        let mut is_suspended = self.is_suspended.lock().await;
        *is_suspended = false;
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
