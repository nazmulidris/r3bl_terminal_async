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

use crate::TerminalAsync;
use crossterm::{cursor::*, style::*, terminal::*, *};
use miette::miette;
use std::io::Write;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::AbortHandle, time::interval};

// 01: This needs a TerminalAsync instance to work. So this is not the main entry point for the crate.
pub struct Spinner {
    pub tick_delay: Duration,
    pub message: String,
    pub abort_handle: Arc<Mutex<Option<AbortHandle>>>,
    pub terminal_async: TerminalAsync,
}

// 01: needs examples and docs once moved to the other crate
// 01: needs tests
// 01: check isTTY and disable progress bar if not

impl Spinner {
    pub async fn start(
        message: String,
        tick_delay: Duration,
        terminal_async: TerminalAsync,
    ) -> miette::Result<Spinner> {
        let mut bar = Spinner {
            message,
            tick_delay,
            terminal_async,
            abort_handle: Arc::new(Mutex::new(None)),
        };

        // Start task and get the abort_handle.
        let abort_handle = bar.try_start_task().await?;

        // Save the abort_handle.
        *bar.abort_handle.lock().await = Some(abort_handle);

        // Suspend terminal_async output while spinner is running.
        bar.terminal_async.suspend().await;

        Ok(bar)
    }

    fn get_stdout(&self) -> impl std::io::Write {
        std::io::stderr()
    }

    async fn try_start_task(&mut self) -> miette::Result<AbortHandle> {
        if self.abort_handle.lock().await.is_some() {
            return Err(miette!("Task is already running"));
        }

        let message = self.message.clone();
        let tick_delay = self.tick_delay;
        let abort_handle = self.abort_handle.clone();
        let mut stdout = self.get_stdout();

        let join_handle = tokio::spawn(async move {
            let mut interval = interval(tick_delay);
            let mut count = 0;
            let message_clone = message.clone();

            loop {
                // If abort_handle is None (`finish_with_message()` has been called), then
                // break the loop.
                if abort_handle.lock().await.is_none() {
                    break;
                }

                interval.tick().await;

                let output = format!("{}{}", message_clone, ".".repeat(count));

                // Print the output. And make sure to terminate w/ a newline, so that the
                // output is printed.
                let _ = execute!(
                    stdout,
                    MoveToColumn(0),
                    Print(format!("{}\n", output)),
                    MoveUp(1),
                );
                let _ = stdout.flush();

                count += 1;
            }
        });

        Ok(join_handle.abort_handle())
    }

    pub async fn stop(&mut self, message: &str) {
        // If task is running, abort it. And print "\n".
        if let Some(abort_handle) = self.abort_handle.lock().await.take() {
            // Abort the task.
            abort_handle.abort();

            // Print the final message.
            let mut stdout = self.get_stdout();
            let _ = execute!(
                stdout,
                MoveToColumn(0),
                Clear(ClearType::CurrentLine),
                Print(format!("{}\n", message)),
            );
            let _ = stdout.flush();

            // Resume terminal_async output.
            self.terminal_async.resume().await;
        }
    }
}
