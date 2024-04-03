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

use r3bl_terminal_async::{
    Spinner, SpinnerStyle, SpinnerTemplate, TerminalAsync, ARTIFICIAL_UI_DELAY, DELAY_MS,
    DELAY_UNIT,
};
use std::time::Duration;
use tokio::{time::Instant, try_join};

#[tokio::main]
pub async fn main() -> miette::Result<()> {
    let terminal_async = TerminalAsync::try_new("$ ")?;

    if let Some(terminal_async) = terminal_async {
        println!("-------------> Example with concurrent output: Dots <-------------");
        example_with_concurrent_output(terminal_async.clone(), SpinnerTemplate::Dots).await?;

        println!("-------------> Example with concurrent output: Braille <-------------");
        example_with_concurrent_output(terminal_async.clone(), SpinnerTemplate::Braille).await?;

        println!("-------------> Example with concurrent output: Block <-------------");
        example_with_concurrent_output(terminal_async.clone(), SpinnerTemplate::Block).await?;
    }

    Ok(())
}

#[allow(unused_assignments)]
async fn example_with_concurrent_output(
    mut terminal_async: TerminalAsync,
    template: SpinnerTemplate,
) -> miette::Result<()> {
    let address = "127.0.0.1:8000";
    let message_trying_to_connect = format!("Trying to connect to server on {}", &address);

    let mut maybe_spinner = Spinner::try_start(
        message_trying_to_connect.clone(),
        DELAY_UNIT,
        terminal_async.clone(),
        SpinnerStyle {
            template,
            ..Default::default()
        },
    )
    .await?;

    // Start another task, to simulate some async work, that uses a interval to display
    // output, for a fixed amount of time, using `terminal_async.println_prefixed()`.
    let _ = try_join!(tokio::spawn(async move {
        // To calculate the delay.
        let duration = ARTIFICIAL_UI_DELAY;
        let start = Instant::now();

        // Dropping the interval cancels it.
        let mut interval = tokio::time::interval(Duration::from_millis(DELAY_MS * 5));

        loop {
            interval.tick().await;
            let elapsed = start.elapsed();
            if elapsed >= duration {
                break;
            }
            terminal_async.println_prefixed("foo").await;
        }
    }));

    // Stop progress bar.
    if let Some(spinner) = maybe_spinner.as_mut() {
        spinner.stop("Connected to server").await;
    }

    Ok(())
}
