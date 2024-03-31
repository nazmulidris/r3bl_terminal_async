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

use crate::HISTORY_SIZE_MAX;
use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures_util::StreamExt;
use std::collections::VecDeque;

// 01: add tests

pub struct History {
    pub entries: VecDeque<String>,
    pub max_size: usize,
    pub sender: UnboundedSender<String>,
    receiver: UnboundedReceiver<String>,

    current_position: Option<usize>,
}

impl Default for History {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            entries: Default::default(),
            max_size: HISTORY_SIZE_MAX,
            sender,
            receiver,
            current_position: Default::default(),
        }
    }
}

impl History {
    // Update history entries
    pub async fn update(&mut self) {
        // Receive a new line
        if let Some(line) = self.receiver.next().await {
            // Don't add entry if last entry was same, or line was empty.
            if self.entries.front() == Some(&line) || line.is_empty() {
                return;
            }
            // Add entry to front of history.
            self.entries.push_front(line);

            // Reset offset to newest entry.
            self.current_position = None;

            // Check if already have enough entries.
            if self.entries.len() > self.max_size {
                // Remove oldest entry
                self.entries.pop_back();
            }
        }
    }

    // Find next history that matches a given string from an index.
    pub fn search_next(&mut self, _current: &str) -> Option<&str> {
        if let Some(index) = &mut self.current_position {
            if *index < self.entries.len() - 1 {
                *index += 1;
            }
            Some(&self.entries[*index])
        } else if !self.entries.is_empty() {
            self.current_position = Some(0);
            Some(&self.entries[0])
        } else {
            None
        }
    }

    // Find previous history item that matches a given string from an index.
    pub fn search_previous(&mut self, _current: &str) -> Option<&str> {
        if let Some(index) = &mut self.current_position {
            if *index == 0 {
                self.current_position = None;
                return Some("");
            }
            *index -= 1;
            Some(&self.entries[*index])
        } else {
            None
        }
    }
}
