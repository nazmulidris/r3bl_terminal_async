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

//! The `r3bl_terminal_async` library lets your CLI program:
//! 1. Read user input from the terminal line by line, while your program concurrently
//!    writes lines to the same terminal. One [`Readline`] instance can spawn many async
//!    `stdout` writers ([SharedWriter]) that can write to the terminal concurrently. You
//!    use the [`TerminalAsync`] struct to use this functionality. You rarely have to access
//!    the underlying [`Readline`] or [`SharedWriter`] directly.
//! 2. Generate a spinner (indeterminate). This spinner works concurrently with the rest
//!    of your program. It suspends the output from all the [`SharedWriter`] instances
//!    that are associated with one [`Readline`] instance. This is useful when you want to
//!    show a spinner while waiting for a long-running task to complete.
//! 3. Use tokio tracing with support for concurrent `stout` writes. If you choose to log
//!    to `stdout` then the concurrent version ([`SharedWriter`]) from this crate will be
//!    used. This ensures that the concurrent output is supported even for your tracing
//!    logs to `stdout`.
//!
//! This crate can detect when your terminal is not in interactive mode. Eg: when you pipe
//! the output of your program to another program. In this case, the `readline` feature is
//! disabled. Both the [`TerminalAsync`] and [`Spinner`] support this functionality. So if you
//! run the examples in this crate, and pipe something into them, they won't do anything. Here's
//! an example:
//!
//! ```bash
//! # This will work.
//! cargo run --examples terminal_async
//!
//! # This won't do anything. Just exits with no error.
//! echo "hello" | cargo run --examples terminal_async
//! ```
//!
//! Here is a video of the `terminal_async` example in action:
//! ![terminal_async_video](https://github.com/nazmulidris/r3bl_terminal_async/blob/main/docs/clip-terminal-async.gif?raw=true)
//!
//! Here is a video of the `spinner` example in action:
//! ![spinner_video](https://github.com/nazmulidris/r3bl_terminal_async/blob/main/docs/clip-spinner.gif?raw=true)
//!
//! ## Why use this crate
//!
//! 1. Because
//!    [`read_line()`](https://doc.rust-lang.org/std/io/struct.Stdin.html#method.read_line)
//!    is blocking. And there is no way to terminate an OS thread that is blocking in Rust.
//!    To do this you have to exit the process (who's thread is blocked in `read_line()`).
//!
//!     - There is no way to get `read_line()` unblocked once it is blocked.
//!     - You can use [`process::exit()`](https://doc.rust-lang.org/std/process/fn.exit.html)
//!       or [`panic!()`](https://doc.rust-lang.org/std/panic/index.html) to kill the entire
//!       process. This is not appealing.
//!     - Even if that task is wrapped in a [`thread::spawn()` or
//!       `thread::spawn_blocking()`](https://tokio.rs/tokio/tutorial/spawning), it isn't
//!       possible to cancel or abort that thread, without cooperatively asking it to exit. To
//!       see what this type of code looks like, take a look at
//!       [this](https://github.com/nazmulidris/rust-scratch/blob/fcd730c4b17ed0b09ff2c1a7ac4dd5b4a0c66e49/tcp-api-server/src/client_task.rs#L275).
//!
//! 2. Another annoyance is that when a thread is blocked in `read_line()`, and you have
//!    to display output to `stdout` concurrently, this poses some challenges.
//!
//!     - This is because the caret is moved by `read_line()` and it blocks.
//!     - When another thread / task writes to `stdout` concurrently, it assumes that the
//!       caret is at row 0 of a new line.
//!     - This results in output that doesn't look good.
//!
//! ### More info on blocking and thread cancellation in Rust
//!
//! - [Docs: tokio's `stdin`](https://docs.rs/tokio/latest/tokio/io/struct.Stdin.html)
//! - [Discussion: Stopping a thread in
//!   Rust](https://users.rust-lang.org/t/stopping-a-thread/6328/7)
//! - [Discussion: Support for
//!   `Thread::cancel()`](https://internals.rust-lang.org/t/thread-cancel-support/3056/16)
//! - [Discussion: stdin, stdout redirection for spawned
//!   processes](https://stackoverflow.com/questions/34611742/how-do-i-read-the-output-of-a-child-process-without-blocking-in-rust)
//!
//! # Examples
//!
//! ```bash
//! cargo run --example terminal_async
//! cargo run --example spinner
//! ```
//!
//! # How to use this crate
//!
//! ## [`TerminalAsync::try_new()`]
//!
//! This is the main entry point for this library.
//! 1. You can clone the `TerminalAsync` struct that you get from this method and use it
//!    in multiple tasks. You can also call [`TerminalAsync::clone_shared_writer()`] to get a
//!    [`SharedWriter`] instance that you can use to write to `stdout` concurrently, using
//!    [`std::write!`] or [`std::writeln!`].
//! 2. To read user input, call [`TerminalAsync::get_readline_event()`].
//! 3. If you use `std::writeln!` then there's no need to [`TerminalAsync::flush()`]
//!    because the `\n` will flush the buffer. When there's no `\n` in the buffer, or you
//!    are using `std::write!` then you might need to call [`TerminalAsync::flush()`].
//! 4. You can use the [`TerminalAsync::println`] and [`TerminalAsync::println_prefixed`]
//!    methods to easily write concurrent output to the `stdout` ([`SharedWriter`]).
//! 5. You can also get access to the underlying [`Readline`] via the `readline` field.
//!    Details on this struct are listed below. For most use cases you won't need to do
//!    this.
//!
//! ### [`Readline`] details
//!
//! - Structure for reading lines of input from a terminal while lines are output to the
//!   terminal concurrently.
//! - Terminal input is retrieved by calling `Readline::readline()`, which returns each
//!   complete line of input once the user presses Enter.
//! - Each `Readline` instance is associated with one or more `SharedWriter` instances.
//!   Lines written to an associated `SharedWriter` are output while retrieving input with
//!   `readline()` or by calling `flush()`.
//!
//! - Call [`Readline::new()`] to create a [`Readline`] instance and associated
//!   [`SharedWriter`].
//!
//! - Call [`Readline::readline()`] (most likely in a loop) to receive a line
//!   of input from the terminal.  The user entering the line can edit their
//!   input using the key bindings listed under "Input Editing" below.
//!
//! - After receiving a line from the user, if you wish to add it to the
//!   history (so that the user can retrieve it while editing a later line),
//!   call [`Readline::add_history_entry()`].
//!
//! - Lines written to the associated `SharedWriter` while `readline()` is in
//!   progress will be output to the screen above the input line.
//!
//! - When done, call [`Readline::flush()`] to ensure that all lines written to
//!   the `SharedWriter` are output.
//!
//! ## [`Spinner::try_start()`]
//!
//! This displays an indeterminate spinner while waiting for a long-running task to
//! complete. The intention with displaying this spinner is to give the user an
//! indication that the program is still running and hasn't hung. When other tasks produce
//! output concurrently, this spinner will not be clobbered. It suspends the output
//! from all the [`SharedWriter`] instances that are associated with one [`Readline`]
//! instance. The `spinner.rs` example shows this (`cargo run --example spinner`).
//!
//! ## [`tracing_setup::init()`]
//!
//! This is a convenience method to setup Tokio [`tracing_subscriber`] with `stdout` as the output
//! destination. This method also ensures that the [`SharedWriter`] is used for concurrent
//! writes to `stdout`.
//!
//! # Input Editing Behavior
//!
//! While entering text, the user can edit and navigate through the current
//! input line with the following key bindings:
//!
//! - Works on all platforms supported by `crossterm`.
//! - Full Unicode Support (Including Grapheme Clusters).
//! - Multiline Editing.
//! - In-memory History.
//! - Left, Right: Move cursor left/right.
//! - Up, Down: Scroll through input history.
//! - Ctrl-W: Erase the input from the cursor to the previous whitespace.
//! - Ctrl-U: Erase the input before the cursor.
//! - Ctrl-L: Clear the screen.
//! - Ctrl-Left / Ctrl-Right: Move to previous/next whitespace.
//! - Home: Jump to the start of the line.
//!     - When the "emacs" feature (on by default) is enabled, Ctrl-A has the
//!       same effect.
//! - End: Jump to the end of the line.
//!     - When the "emacs" feature (on by default) is enabled, Ctrl-E has the
//!       same effect.
//! - Ctrl-C, Ctrl-D: Send an `Eof` event.
//! - Ctrl-C: Send an `Interrupt` event.
//! - Extensible design based on `crossterm`'s `event-stream` feature.
//!
//! # Why another async readline crate?
//!
//! This crate & repo is forked from
//! [rustyline-async](https://github.com/zyansheep/rustyline-async). However it has mostly
//! been rewritten and re-architected. Here are some changes made to the code:
//! - Drop support for all async runtimes other than `tokio`. Rewrite all the code for
//!   this.
//! - Drop crates like `pin-project`, `thingbuf` in favor of `tokio`. Rewrite all the code
//!   for this.
//! - Drop `simplelog` and `log` dependencies. Add support for `tokio-tracing`. Rewrite
//!   all the code for this, and add `tracing_setup.rs`.
//! - Rewrite main example `examples/readline.rs` to mimic a real world CLI application.
//!   Add more examples.
//! - Add `spinner_impl`, `readline_impl`, and `public_api` modules.
//! - Add tests.
//!
//! # Video series on [developerlife.com](https://developerlife.com) [YT channel](https://www.youtube.com/@developerlifecom) on building this crate with Naz
//!
//! - [Part 1: Why?](https://youtu.be/6LhVx0xM86c)
//! - [Part 2: What?](https://youtu.be/3vQJguti02I)
//! - [Part 3: Do the refactor and rename the crate](https://youtu.be/uxgyZzOmVIw)
//! - [Part 4: Build the spinner](https://www.youtube.com/watch?v=fcb6rstRniI)
//! - [Part 5: Add color gradient animation to
//!   spinner](https://www.youtube.com/watch?v=_QjsGDds270)
//! - [Testing](https://www.youtube.com/watch?v=Xt495QLrFFk&list=PLofhE49PEwmwLR_4Noa0dFOSPmSpIg_l8)
//! - Playlists
//!   - [Build with Naz, async readline and spinner for CLI in Rust](https://www.youtube.com/watch?v=3vQJguti02I&list=PLofhE49PEwmwelPkhfiqdFQ9IXnmGdnSE)
//!   - [Build with Naz, testing in Rust](https://www.youtube.com/watch?v=Xt495QLrFFk&list=PLofhE49PEwmwLR_4Noa0dFOSPmSpIg_l8)

pub mod public_api;
pub mod readline_impl;
pub mod spinner_impl;

pub use public_api::*;
pub use readline_impl::*;
pub use spinner_impl::*;

pub type StdMutex<T> = std::sync::Mutex<T>;
pub type FuturesMutex<T> = futures_util::lock::Mutex<T>;
pub type SafeRawTerminal = std::sync::Arc<FuturesMutex<dyn std::io::Write + Send>>;
pub type SafeLineState = std::sync::Arc<FuturesMutex<LineState>>;
pub type SafeHistory = std::sync::Arc<FuturesMutex<History>>;
pub type SafeBool = std::sync::Arc<FuturesMutex<bool>>;
pub type SafeVecText = std::sync::Arc<FuturesMutex<Vec<Text>>>;

pub type Text = Vec<u8>;
pub const CHANNEL_CAPACITY: usize = 500;
pub const HISTORY_SIZE_MAX: usize = 1000;
