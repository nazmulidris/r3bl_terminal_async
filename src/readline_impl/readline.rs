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

use std::io::{self, stdout, Stdout, Write};

use crossterm::{
    event::EventStream,
    terminal::{self, disable_raw_mode, Clear},
    QueueableCommand,
};
use futures_channel::mpsc;
use futures_util::{select, FutureExt, StreamExt};
use thingbuf::mpsc::Receiver;
use thiserror::Error;

use crate::{LineState, SharedWriter};

// 01: add tests

/// Error returned from [`readline()`][Readline::readline].  Such errors
/// generally require specific procedures to recover from.
#[derive(Debug, Error)]
pub enum ReadlineError {
    /// An internal I/O error occurred
    #[error(transparent)]
    IO(#[from] io::Error),

    /// `readline()` was called after the [`SharedWriter`] was dropped and
    /// everything written to the `SharedWriter` was already output
    #[error("line writers closed")]
    Closed,
}

/// Events emitted by [`Readline::readline()`]
#[derive(Debug)]
pub enum ReadlineEvent {
    /// The user entered a line of text
    Line(String),
    /// The user pressed Ctrl-D
    Eof,
    /// The user pressed Ctrl-C
    Interrupted,
}

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

    line_receiver: Receiver<Vec<u8>>,

    /// Current line.
    line: LineState,

    history_sender: mpsc::UnboundedSender<String>,
}

impl Readline {
    /// Create a new instance with an associated [`SharedWriter`]
    pub fn new(prompt: String) -> Result<(Self, SharedWriter), ReadlineError> {
        let (sender, line_receiver) = thingbuf::mpsc::channel(500);

        terminal::enable_raw_mode()?;

        let line = LineState::new(prompt, terminal::size()?);
        let history_sender = line.history.sender.clone();

        let mut readline = Readline {
            raw_term: stdout(),
            event_stream: EventStream::new(),
            line_receiver,
            line,
            history_sender,
        };

        readline.line.render(&mut readline.raw_term)?;
        readline.raw_term.queue(terminal::EnableLineWrap)?;
        readline.raw_term.flush()?;

        Ok((
            readline,
            SharedWriter {
                sender,
                buffer: Vec::new(),
            },
        ))
    }

    /// Change the prompt
    pub fn update_prompt(&mut self, prompt: &str) -> Result<(), ReadlineError> {
        self.line.update_prompt(prompt, &mut self.raw_term)?;
        Ok(())
    }

    /// Clear the screen
    pub fn clear(&mut self) -> Result<(), ReadlineError> {
        self.raw_term.queue(Clear(terminal::ClearType::All))?;
        self.line.clear_and_render(&mut self.raw_term)?;
        self.raw_term.flush()?;
        Ok(())
    }

    /// Set maximum history length.  The default length is 1000.
    pub fn set_max_history(&mut self, max_size: usize) {
        self.line.history.max_size = max_size;
        self.line.history.entries.truncate(max_size);
    }

    /// Set whether the input line should remain on the screen after
    /// events.
    ///
    /// If `enter` is true, then when the user presses "Enter", the prompt
    /// and the text they entered will remain on the screen, and the cursor
    /// will move to the next line.  If `enter` is false, the prompt &
    /// input will be erased instead.
    ///
    /// `control_c` similarly controls the behavior for when the user
    /// presses Ctrl-C.
    ///
    /// The default value for both settings is `true`.
    pub fn should_print_line_on(&mut self, enter: bool, control_c: bool) {
        self.line.should_print_line_on_enter = enter;
        self.line.should_print_line_on_control_c = control_c;
    }

    /// Flush all writers to terminal and erase the prompt string
    pub fn flush(&mut self) -> Result<(), ReadlineError> {
        while let Ok(buf) = self.line_receiver.try_recv_ref() {
            self.line.print_data(&buf, &mut self.raw_term)?;
        }
        self.line.clear(&mut self.raw_term)?;
        self.raw_term.flush()?;
        Ok(())
    }

    /// Polling function for readline, manages all input and output.
    /// Returns either an Readline Event or an Error
    pub async fn readline(&mut self) -> Result<ReadlineEvent, ReadlineError> {
        loop {
            select! {
                event = self.event_stream.next().fuse() => match event {
                    Some(Ok(event)) => {
                        match self.line.handle_event(event, &mut self.raw_term) {
                            Ok(Some(event)) => {
                                self.raw_term.flush()?;
                                return Result::<_, ReadlineError>::Ok(event)
                            },
                            Err(e) => return Err(e),
                            Ok(None) => self.raw_term.flush()?,
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => {},
                },
                result = self.line_receiver.recv_ref().fuse() => match result {
                    Some(buf) => {
                        self.line.print_data(&buf, &mut self.raw_term)?;
                        self.raw_term.flush()?;
                    },
                    None => return Err(ReadlineError::Closed),
                },
                _ = self.line.history.update().fuse() => {}
            }
        }
    }

    /// Add a line to the input history
    pub fn add_history_entry(&mut self, entry: String) -> Option<()> {
        self.history_sender.unbounded_send(entry).ok()
    }
}

impl Drop for Readline {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}
