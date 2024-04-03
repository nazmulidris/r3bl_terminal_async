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

use crate::{SpinnerStyle, BLOCK_DOTS, BRAILLE_DOTS};
use crossterm::{
    cursor::{MoveToColumn, MoveUp},
    execute,
    style::Print,
    terminal::{Clear, ClearType},
};
use std::io::Write;

pub trait SpinnerRender {
    fn render_tick(&self, message: &str, count: usize, display_width: usize) -> String;
    fn paint_tick(&self, output: &str, writer: &mut impl Write);
    fn render_final_tick(&self, message: &str, display_width: usize) -> String;
    fn paint_final_tick(&self, output: &str, writer: &mut impl Write);
}

impl SpinnerRender for SpinnerStyle {
    // 00: do something w/ display_width
    fn render_tick(&self, message: &str, count: usize, _display_width: usize) -> String {
        match self.template {
            crate::SpinnerTemplate::Dots => {
                format!("{}{}", message, ".".repeat(count))
            }
            crate::SpinnerTemplate::Braille => {
                // Translate count into the index of the BRAILLE_DOTS array.
                let index_to_use = count % BRAILLE_DOTS.len();
                format!("{} {}", BRAILLE_DOTS[index_to_use], message)
            }
            crate::SpinnerTemplate::Block => {
                // Translate count into the index of the BLOCK_DOTS array.
                let index_to_use = count % BLOCK_DOTS.len();
                format!("{} {}", BLOCK_DOTS[index_to_use], message)
            }
        }

        // 00: Determine the output based on the style color.
        // match style_color {
        //     SpinnerColor::None => {}
        //     SpinnerColor::ColorWheel => todo!(),
        // }
    }

    fn paint_tick(&self, output: &str, writer: &mut impl Write) {
        match self.template {
            crate::SpinnerTemplate::Dots => {
                // Print the output. And make sure to terminate w/ a newline, so that the
                // output is printed.
                let _ = execute!(
                    writer,
                    MoveToColumn(0),
                    Print(format!("{}\n", output)),
                    MoveUp(1),
                );
            }
            crate::SpinnerTemplate::Braille => {
                // Print the output. And make sure to terminate w/ a newline, so that the
                // output is printed.
                let _ = execute!(
                    writer,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{}\n", output)),
                    MoveUp(1),
                );
            }
            crate::SpinnerTemplate::Block => {
                // Print the output. And make sure to terminate w/ a newline, so that the
                // output is printed.
                let _ = execute!(
                    writer,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{}\n", output)),
                    MoveUp(1),
                );
            }
        }
        let _ = writer.flush();
    }

    // 00: do something w/ display_width
    fn render_final_tick(&self, final_message: &str, _display_width: usize) -> String {
        match self.template {
            crate::SpinnerTemplate::Dots => final_message.to_string(),
            crate::SpinnerTemplate::Braille => final_message.to_string(),
            crate::SpinnerTemplate::Block => final_message.to_string(),
        }
    }

    fn paint_final_tick(&self, output: &str, writer: &mut impl Write) {
        match self.template {
            crate::SpinnerTemplate::Dots => {
                let _ = execute!(
                    writer,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{}\n", output)),
                );
            }
            crate::SpinnerTemplate::Braille => {
                let _ = execute!(
                    writer,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{}\n", output)),
                );
            }
            crate::SpinnerTemplate::Block => {
                let _ = execute!(
                    writer,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{}\n", output)),
                );
            }
        }
        let _ = writer.flush();
    }
}
