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

use r3bl_terminal_async::{ProgressBarAsync, TerminalAsync};
use std::time::Duration;

const DELAY_MS: u64 = 33;
pub const DELAY_UNIT: Duration = Duration::from_millis(DELAY_MS);
const ARTIFICIAL_UI_DELAY: Duration = Duration::from_millis(DELAY_MS * 15);

#[tokio::main]
pub async fn main() -> miette::Result<()> {
    let terminal_async = TerminalAsync::try_new("$ ")?;

    println!("------------- 1) Example with no other output");
    example_no_other_output(terminal_async.clone()).await?;

    println!("------------- 2) Example with other output");
    example_with_other_output(terminal_async.clone()).await?;

    Ok(())
}

async fn example_with_other_output(terminal_async: TerminalAsync) -> miette::Result<()> {
    let address = "127.0.0.1:8000";
    let message_trying_to_connect = format!("Trying to connect to server on {}", &address);

    let mut bar = ProgressBarAsync::try_new_and_start(
        message_trying_to_connect.clone(),
        DELAY_UNIT,
        terminal_async.clone(),
    )
    .await?;

    // Start an interval to display output using terminal_async.println_prefixed().
    let mut terminal_async_clone = terminal_async.clone();
    let interval_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(30));
        loop {
            interval.tick().await;
            terminal_async_clone.println_prefixed("foo").await;
        }
    });

    // Artificial delay to see the spinner spin.
    tokio::time::sleep(ARTIFICIAL_UI_DELAY).await;

    // Stop the interval.
    interval_handle.abort();

    // Stop progress bar.
    bar.finish_with_message("Connected to server").await;
    Ok(())
}

/// This example demonstrates how to use the [ProgressBarAsync] struct to show a
/// indeterminate progress bar while waiting for a long-running task to complete.
async fn example_no_other_output(terminal_async: TerminalAsync) -> miette::Result<()> {
    let address = "127.0.0.1:8000";
    let message_trying_to_connect = format!("Trying to connect to server on {}", &address);

    let mut bar = ProgressBarAsync::try_new_and_start(
        message_trying_to_connect.clone(),
        DELAY_UNIT,
        terminal_async.clone(),
    )
    .await?;

    // Artificial delay to see the spinner spin.
    tokio::time::sleep(ARTIFICIAL_UI_DELAY).await;

    // Stop progress bar.
    bar.finish_with_message("Connected to server").await;
    Ok(())
}
