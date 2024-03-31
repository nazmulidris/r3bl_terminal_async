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

use r3bl_terminal_async::{Readline, ReadlineEvent};
use std::io::Write;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut readline, mut stdout) = Readline::new("prompt> ".into())?;

    readline.should_print_line_on(false, false);

    loop {
        tokio::select! {
            _ = sleep(Duration::from_secs(1)) => {
                writeln!(stdout, "Message received!")?;
            }
            cmd = readline.readline() => match cmd {
                Ok(ReadlineEvent::Line(line)) => {
                    writeln!(stdout, "You entered: {line:?}")?;
                    readline.add_history_entry(line.clone());
                    if line == "quit" {
                        break;
                    }
                }
                Ok(ReadlineEvent::Eof) => {
                    writeln!(stdout, "<EOF>")?;
                    break;
                }
                Ok(ReadlineEvent::Interrupted) => {
                    // writeln!(stdout, "^C")?;
                    continue;
                }
                Err(e) => {
                    writeln!(stdout, "Error: {e:?}")?;
                    break;
                }
            }
        }
    }

    readline.flush().await?;

    Ok(())
}
