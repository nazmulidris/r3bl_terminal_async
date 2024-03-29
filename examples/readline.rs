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

use crossterm::style::Stylize;
use futures_util::FutureExt;
use miette::IntoDiagnostic;
use r3bl_terminal_async::{Readline, ReadlineEvent, SharedWriter};
use std::{io::Write, ops::ControlFlow, time::Duration};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter, EnumString};
use tokio::time::interval;
use tracing::info;

mod helpers;
use helpers::tracing_setup::{self};

/// More info:
/// - <https://docs.rs/strum_macros/latest/strum_macros/derive.EnumString.html>
/// - <https://docs.rs/strum_macros/latest/strum_macros/derive.Display.html>
/// - <https://docs.rs/strum_macros/latest/strum_macros/derive.EnumIter.html>
#[derive(Debug, PartialEq, EnumString, EnumIter, Display)]
enum Command {
    #[strum(ascii_case_insensitive)]
    StartTask1,

    #[strum(ascii_case_insensitive)]
    StopTask1,

    #[strum(ascii_case_insensitive)]
    StartTask2,

    #[strum(ascii_case_insensitive)]
    StopTask2,

    #[strum(ascii_case_insensitive)]
    StartPrintouts,

    #[strum(ascii_case_insensitive)]
    StopPrintouts,

    #[strum(ascii_case_insensitive)]
    Info,

    #[strum(ascii_case_insensitive)]
    Exit,
}

fn get_info_message() -> String {
    let available_commands = {
        let commands = Command::iter()
            .map(|it| it.to_string())
            .collect::<Vec<String>>();
        format!("{:?}", commands).blue()
    };
    let info_message = format!(
        "try Ctrl+D, Up, Down, `{}`, `{}`, and `{}`",
        Command::StartTask1,
        Command::StartTask2,
        Command::StopPrintouts
    );
    format!(
        "{}: \n{}\n{}",
        format!("{}", "Available commands".bold())
            .magenta()
            .bold()
            .underlined(),
        available_commands,
        info_message.to_string().white().bold().on_dark_grey()
    )
}

#[derive(Debug, Clone, Copy)]
struct State {
    pub task_1_state: TaskState,
    pub task_2_state: TaskState,
}

#[derive(Debug, Clone, Copy)]
struct TaskState {
    pub interval_delay: Duration,
    pub counter: u64,
    pub is_running: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            task_1_state: TaskState {
                interval_delay: Duration::from_secs(1),
                counter: 0,
                is_running: false,
            },
            task_2_state: TaskState {
                interval_delay: Duration::from_secs(4),
                counter: 0,
                is_running: false,
            },
        }
    }
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    // Available options for configuration:
    // readline.should_print_line_on(false, false);
    // readline.set_max_history(10);
    let (mut readline, mut stdout) = Readline::new("> ".to_owned()).into_diagnostic()?;

    // Pre-populate the readline's history with some entries.
    Command::iter().for_each(|command| {
        readline.add_history_entry(command.to_string());
    });

    tracing_setup::init(stdout.clone())?;

    // Start tasks.
    let mut state = State::default();
    let mut interval_1_task = interval(state.task_1_state.interval_delay);
    let mut interval_2_task = interval(state.task_2_state.interval_delay);

    writeln!(stdout, "{}", get_info_message()).into_diagnostic()?;

    loop {
        tokio::select! {
            _ = interval_1_task.tick() => {
                task_1::tick(&mut state, &mut stdout)?;
            },
            _ = interval_2_task.tick() => {
                task_2::tick(&mut state, &mut stdout)?;
            },
            user_input = readline.readline().fuse() => match user_input {
                Ok(readline_event) => {
                    let continuation = process_input_event::process_readline_event(
                        readline_event, &mut state, &mut stdout, &mut readline
                    )?;
                    if let ControlFlow::Break(_) = continuation { break }
                },
                Err(err) => {
                    writeln!(stdout,"Received err: {}", format!("{:?}",err).red()).into_diagnostic()?;
                    writeln!(stdout,"{}", "Exiting...".red()).into_diagnostic()?;
                    break;
                },
            }
        }
    }

    // Flush all writers to stdout
    readline.flush().into_diagnostic()?;

    Ok(())
}

/// This task simply uses [writeln] and [SharedWriter] to print to stdout.
mod task_1 {
    use super::*;

    pub fn tick(state: &mut State, stdout: &mut SharedWriter) -> miette::Result<()> {
        if !state.task_1_state.is_running {
            return Ok(());
        };

        let counter_1 = state.task_1_state.counter;
        writeln!(stdout, "[{counter_1}] First interval went off!").into_diagnostic()?;
        state.task_1_state.counter += 1;

        Ok(())
    }
}

/// This task uses [tracing] to log to stdout (via [SharedWriter]).
mod task_2 {
    use super::*;

    pub fn tick(state: &mut State, _stdout: &mut SharedWriter) -> miette::Result<()> {
        if !state.task_2_state.is_running {
            return Ok(());
        };

        let counter_2 = state.task_2_state.counter;
        info!("[{counter_2}] Second interval went off!");
        state.task_2_state.counter += 1;

        Ok(())
    }
}

mod process_input_event {
    use std::str::FromStr;

    use super::*;

    pub fn process_readline_event(
        readline_event: ReadlineEvent,
        state: &mut State,
        stdout: &mut SharedWriter,
        readline: &mut Readline,
    ) -> miette::Result<ControlFlow<()>> {
        match readline_event {
            ReadlineEvent::Line(user_input) => {
                process_user_input(user_input, state, stdout, readline)
            }
            ReadlineEvent::Eof => {
                writeln!(stdout, "{}", "Exiting due to Eof...".red().bold()).into_diagnostic()?;
                Ok(ControlFlow::Break(()))
            }
            ReadlineEvent::Interrupted => {
                writeln!(stdout, "{}", "^C pressed...".red().bold()).into_diagnostic()?;
                Ok(ControlFlow::Break(()))
            }
        }
    }

    fn process_user_input(
        user_input: String,
        state: &mut State,
        stdout: &mut SharedWriter,
        readline: &mut Readline,
    ) -> miette::Result<ControlFlow<()>> {
        // Add to history.
        let line = user_input.trim();
        readline.add_history_entry(line.to_string());

        // Convert line to command. And process it.
        let result_command = Command::from_str(&line.trim().to_lowercase());
        match result_command {
            Err(_) => {
                writeln!(stdout, "Unknown command!").into_diagnostic()?;
                return Ok(ControlFlow::Continue(()));
            }
            Ok(command) => match command {
                Command::Exit => {
                    writeln!(stdout, "{}", "Exiting...".red()).into_diagnostic()?;
                    return Ok(ControlFlow::Break(()));
                }
                Command::StartTask1 => {
                    state.task_1_state.is_running = true;
                    writeln!(stdout, "First task started! This prints to stdout.")
                        .into_diagnostic()?;
                }
                Command::StopTask1 => {
                    state.task_1_state.is_running = false;
                    writeln!(stdout, "First task stopped!").into_diagnostic()?;
                }
                Command::StartTask2 => {
                    state.task_2_state.is_running = true;
                    writeln!(
                        stdout,
                        "Second task started! This generates logs which print to stdout"
                    )
                    .into_diagnostic()?;
                }
                Command::StopTask2 => {
                    state.task_2_state.is_running = false;
                    writeln!(stdout, "Second task stopped!").into_diagnostic()?;
                }
                Command::StartPrintouts => {
                    writeln!(stdout, "Printouts started!").into_diagnostic()?;
                    readline.should_print_line_on(true, true);
                }
                Command::StopPrintouts => {
                    writeln!(stdout, "Printouts stopped!").into_diagnostic()?;
                    readline.should_print_line_on(false, false);
                }
                Command::Info => {
                    writeln!(stdout, "{}", get_info_message()).into_diagnostic()?;
                }
            },
        }

        Ok(ControlFlow::Continue(()))
    }
}
