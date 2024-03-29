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

#![allow(dead_code)]

use std::io::Write;

use miette::IntoDiagnostic;
use r3bl_terminal_async::{Readline, ReadlineEvent};
use tracing::info;

mod helpers;
use helpers::*;

#[derive(Debug)]
struct BigStruct {
    bytes: Vec<u8>,
    name: String,
    number: usize,
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    let (mut rl, mut stdout) = Readline::new("> ".to_owned()).unwrap();

    let thingy = BigStruct {
        bytes: vec![1; 20],
        name: "Baloney Shmalony".to_owned(),
        number: 60,
    };

    tracing_setup::init(stdout.clone())?;

    loop {
        match rl.readline().await {
            Ok(ReadlineEvent::Line(_)) => {
                writeln!(stdout, "{:?}", thingy).into_diagnostic()?;
                info!("{:?}", thingy);
            }
            Ok(ReadlineEvent::Eof) => {
                writeln!(stdout, "Exiting...").into_diagnostic()?;
                break;
            }
            Ok(ReadlineEvent::Interrupted) => writeln!(stdout, "^C").into_diagnostic()?,
            Err(err) => {
                writeln!(stdout, "Received err: {:?}", err).into_diagnostic()?;
                break;
            }
        }
    }
    rl.flush().into_diagnostic()?;

    Ok(())
}
