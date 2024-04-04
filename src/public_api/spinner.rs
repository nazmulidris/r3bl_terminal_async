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

use crate::{SpinnerRender, SpinnerStyle, TerminalAsync};
use miette::miette;
use r3bl_tuify::{
    is_fully_uninteractive_terminal, is_stdout_piped, StdoutIsPipedResult, TTYResult,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::AbortHandle, time::interval};

// 01: This needs a TerminalAsync instance to work. So this is not the main entry point for the crate.
pub struct Spinner {
    pub tick_delay: Duration,

    pub message: String,

    /// This is populated when the task is started. And when the task is stopped, it is
    /// set to [None].
    pub abort_handle: Arc<Mutex<Option<AbortHandle>>>,

    pub terminal_async: TerminalAsync,

    pub style: SpinnerStyle,
}

// 01: needs examples and docs once moved to the other crate
// 01: needs tests
// 01: check isTTY and disable progress bar if not

mod abort_handle {
    use super::*;

    pub async fn is_unset(abort_handle: &Arc<Mutex<Option<AbortHandle>>>) -> bool {
        abort_handle.lock().await.is_none()
    }

    pub async fn is_set(abort_handle: &Arc<Mutex<Option<AbortHandle>>>) -> bool {
        abort_handle.lock().await.is_some()
    }

    pub async fn try_unset(abort_handle: &Arc<Mutex<Option<AbortHandle>>>) -> Option<AbortHandle> {
        abort_handle.lock().await.take()
    }

    pub async fn set(abort_handle: &Arc<Mutex<Option<AbortHandle>>>, handle: AbortHandle) {
        *abort_handle.lock().await = Some(handle);
    }
}

impl Spinner {
    /// Create a new instance of [Spinner].
    ///
    /// ### Returns
    /// 1. This will return an error if the task is already running.
    /// 2. If the terminal is not fully interactive then it will return [None], and won't
    ///    start the task. This is when the terminal is not considered fully interactive:
    ///    - `stdout` is piped, eg: `echo "foo" | cargo run --example spinner`.
    ///    - or all three `stdin`, `stdout`, `stderr` are not `is_tty`, eg when running in
    ///      `cargo test`.
    /// 3. Otherwise, it will start the task and return a [Spinner] instance.
    ///
    /// More info on terminal piping:
    /// - <https://unix.stackexchange.com/questions/597083/how-does-piping-affect-stdin>
    pub async fn try_start(
        spinner_message: String,
        tick_delay: Duration,
        terminal_async: TerminalAsync,
        style: SpinnerStyle,
    ) -> miette::Result<Option<Spinner>> {
        if let StdoutIsPipedResult::StdoutIsPiped = is_stdout_piped() {
            return Ok(None);
        }
        if let TTYResult::IsNotInteractive = is_fully_uninteractive_terminal() {
            return Ok(None);
        }

        // Only start the task if the terminal is fully interactive.
        let mut spinner = Spinner {
            message: spinner_message,
            tick_delay,
            terminal_async,
            abort_handle: Arc::new(Mutex::new(None)),
            style,
        };

        // Start task and get the abort_handle.
        let abort_handle = spinner.try_start_task().await?;

        // Save the abort_handle.
        abort_handle::set(&spinner.abort_handle, abort_handle).await;

        // Suspend terminal_async output while spinner is running.
        spinner.terminal_async.suspend().await;

        Ok(Some(spinner))
    }

    fn get_stdout(&self) -> impl std::io::Write {
        std::io::stderr()
    }

    async fn try_start_task(&mut self) -> miette::Result<AbortHandle> {
        if abort_handle::is_set(&self.abort_handle).await {
            return Err(miette!("Task is already running"));
        }

        let message = self.message.clone();
        let tick_delay = self.tick_delay;
        let abort_handle = self.abort_handle.clone();
        let mut stdout = self.get_stdout();
        let mut style = self.style.clone();

        let join_handle = tokio::spawn(async move {
            let mut interval = interval(tick_delay);
            // Count is used to determine the output.
            let mut count = 0;
            let message_clone = message.clone();

            loop {
                // Wait for the interval duration.
                interval.tick().await;

                // If abort_handle is None (ie, `stop()` has been called), then break the
                // loop.
                if abort_handle::is_unset(&abort_handle).await {
                    break;
                }

                // Render and paint the output, based on style.
                let output = style.render_tick(&message_clone, count, 0);
                style.paint_tick(&output, &mut stdout);

                // Increment count to affect the output in the next iteration of this loop.
                count += 1;
            }
        });

        Ok(join_handle.abort_handle())
    }

    pub async fn stop(&mut self, final_message: &str) {
        // If task is running, abort it. And print "\n".
        if let Some(abort_handle) = abort_handle::try_unset(&self.abort_handle).await {
            // Abort the task.
            abort_handle.abort();

            // Print the final message.
            let final_output = self.style.render_final_tick(final_message, 0);
            self.style
                .paint_final_tick(&final_output, &mut self.get_stdout());

            // Resume terminal_async output.
            self.terminal_async.resume().await;
        }
    }
}
