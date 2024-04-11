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
use miette::IntoDiagnostic;
use r3bl_terminal_async::{LineControlSignal, Spinner, SpinnerStyle};
use r3bl_terminal_async::{Readline, ReadlineEvent, SharedWriter, TerminalAsync};
use std::{io::Write, ops::ControlFlow, time::Duration};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter, EnumString};
use tokio::select;
use tokio::time::interval;
use tracing::info;

/// Load dependencies for this examples file.
mod helpers;
use helpers::tracing_setup::{self};

/// More info:
/// - <https://docs.rs/strum_macros/latest/strum_macros/derive.EnumString.html>
/// - <https://docs.rs/strum_macros/latest/strum_macros/derive.Display.html>
/// - <https://docs.rs/strum_macros/latest/strum_macros/derive.EnumIter.html>
#[derive(Debug, PartialEq, EnumString, EnumIter, Display)]
enum Command {
    #[strum(ascii_case_insensitive)]
    Spinner,

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
        "try Ctrl+D, Up, Down, `{}`, `{}`, `{}`, and `{}`",
        Command::StartTask1,
        Command::Spinner,
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
    let maybe_terminal_async = TerminalAsync::try_new("> ").await?;

    // If the terminal is not fully interactive, then return early.
    let mut terminal_async = match maybe_terminal_async {
        None => return Ok(()),
        _ => maybe_terminal_async.unwrap(),
    };

    // Pre-populate the readline's history with some entries.

    for command in Command::iter() {
        terminal_async
            .readline
            .add_history_entry(command.to_string());
    }

    // Initialize tracing w/ the "async stdout".
    tracing_setup::init(terminal_async.clone_shared_writer())?;

    // Start tasks.
    let mut state = State::default();
    let mut interval_1_task = interval(state.task_1_state.interval_delay);
    let mut interval_2_task = interval(state.task_2_state.interval_delay);

    terminal_async.println(get_info_message().to_string()).await;

    loop {
        select! {
            _ = interval_1_task.tick() => {
                task_1::tick(&mut state, &mut terminal_async.clone_shared_writer())?;
            },
            _ = interval_2_task.tick() => {
                task_2::tick(&mut state, &mut terminal_async.clone_shared_writer())?;
            },
            user_input = terminal_async.get_readline_event() => match user_input {
                Ok(readline_event) => {
                    let continuation = process_input_event::process_readline_event(
                        readline_event, &mut state, &mut terminal_async.clone_shared_writer(), &mut terminal_async.readline
                    ).await?;
                    if let ControlFlow::Break(_) = continuation { break }
                },
                Err(err) => {
                    let msg_1 = format!("Received err: {}", format!("{:?}",err).red());
                    let msg_2 = format!("{}", "Exiting...".red());
                    terminal_async.println(msg_1).await;
                    terminal_async.println(msg_2).await;
                    break;
                },
            }
        }
    }

    // Flush all writers to stdout
    let _ = terminal_async.flush().await;

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

    pub async fn process_readline_event(
        readline_event: ReadlineEvent,
        state: &mut State,
        shared_writer: &mut SharedWriter,
        readline: &mut Readline,
    ) -> miette::Result<ControlFlow<()>> {
        match readline_event {
            ReadlineEvent::Line(user_input) => {
                process_user_input(user_input, state, shared_writer, readline).await
            }
            ReadlineEvent::Eof => {
                writeln!(shared_writer, "{}", "Exiting due to Eof...".red().bold())
                    .into_diagnostic()?;
                Ok(ControlFlow::Break(()))
            }
            ReadlineEvent::Interrupted => {
                writeln!(
                    shared_writer,
                    "{}",
                    "Exiting due to ^C pressed...".red().bold()
                )
                .into_diagnostic()?;
                Ok(ControlFlow::Break(()))
            }
        }
    }

    async fn process_user_input(
        user_input: String,
        state: &mut State,
        shared_writer: &mut SharedWriter,
        readline: &mut Readline,
    ) -> miette::Result<ControlFlow<()>> {
        // Add to history.
        let line = user_input.trim();
        readline.add_history_entry(line.to_string());

        // Convert line to command. And process it.
        let result_command = Command::from_str(&line.trim().to_lowercase());
        match result_command {
            Err(_) => {
                writeln!(shared_writer, "Unknown command!").into_diagnostic()?;
                return Ok(ControlFlow::Continue(()));
            }
            Ok(command) => match command {
                Command::Exit => {
                    writeln!(shared_writer, "{}", "Exiting due to exit command...".red())
                        .into_diagnostic()?;
                    readline.close().await;
                    return Ok(ControlFlow::Break(()));
                }
                Command::StartTask1 => {
                    state.task_1_state.is_running = true;
                    writeln!(shared_writer, "First task started! This prints to stdout.")
                        .into_diagnostic()?;
                }
                Command::StopTask1 => {
                    state.task_1_state.is_running = false;
                    writeln!(shared_writer, "First task stopped!").into_diagnostic()?;
                }
                Command::StartTask2 => {
                    state.task_2_state.is_running = true;
                    writeln!(
                        shared_writer,
                        "Second task started! This generates logs which print to stdout"
                    )
                    .into_diagnostic()?;
                }
                Command::StopTask2 => {
                    state.task_2_state.is_running = false;
                    writeln!(shared_writer, "Second task stopped!").into_diagnostic()?;
                }
                Command::StartPrintouts => {
                    writeln!(shared_writer, "Printouts started!").into_diagnostic()?;
                    readline.should_print_line_on(true, true).await;
                }
                Command::StopPrintouts => {
                    writeln!(shared_writer, "Printouts stopped!").into_diagnostic()?;
                    readline.should_print_line_on(false, false).await;
                }
                Command::Info => {
                    writeln!(shared_writer, "{}", get_info_message()).into_diagnostic()?;
                }
                Command::Spinner => {
                    writeln!(shared_writer, "Spinner started! Pausing terminal...")
                        .into_diagnostic()?;
                    long_running_task::spawn_task_that_shows_spinner(
                        shared_writer,
                        "Spinner task",
                        Duration::from_secs(1),
                    );
                }
            },
        }

        Ok(ControlFlow::Continue(()))
    }
}

mod long_running_task {
    use std::{io::stderr, sync::Arc};

    use r3bl_terminal_async::TokioMutex;

    use super::*;

    // Spawn a task that uses the shared writer to print to stdout, and pauses the spinner
    // at the start, and resumes it when it ends.
    pub fn spawn_task_that_shows_spinner(
        shared_writer: &mut SharedWriter,
        task_name: &str,
        delay: Duration,
    ) {
        let mut interval = interval(delay);
        let mut tick_counter = 0;
        let max_tick_count = 3;

        let line_sender = shared_writer.line_sender.clone();
        let task_name = task_name.to_string();

        let shared_writer_clone = shared_writer.clone();

        tokio::spawn(async move {
            // Create a spinner.
            let maybe_spinner = Spinner::try_start(
                format!(
                    "{} - This is a sample indeterminate progress message",
                    task_name
                ),
                Duration::from_millis(100),
                SpinnerStyle::default(),
                Arc::new(TokioMutex::new(stderr())),
                shared_writer_clone,
            )
            .await;

            loop {
                // Wait for the interval duration (one tick).
                interval.tick().await;

                // Don't print more than `max_tick_count` times.
                tick_counter += 1;
                if tick_counter >= max_tick_count {
                    break;
                }

                // Display a message at every tick.
                let msg = format!("[{task_name}] - [{tick_counter}] interval went off while spinner was spinning!\n");
                let _ = line_sender
                    .send(LineControlSignal::Line(msg.into_bytes()))
                    .await;
            }

            if let Ok(Some(mut spinner)) = maybe_spinner {
                let msg = format!("{} - Task ended. Resuming terminal and showing any output that was generated while spinner was active.", task_name);
                let _ = spinner.stop(msg.as_str()).await;
            }
        });
    }
}
