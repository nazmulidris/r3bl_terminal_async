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

use std::{
    io::{self},
    ops::DerefMut,
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::{pin_mut, ready, AsyncWrite, FutureExt};
use thingbuf::mpsc::{errors::TrySendError, Sender};

// 01: add tests

/// Cloneable object that implements [`Write`][std::io::Write] and
/// [`AsyncWrite`][futures_util::AsyncWrite] and allows for sending data to the terminal
/// without messing up the readline.
///
/// A `SharedWriter` instance is obtained by calling [`crate::Readline::new()`], which
/// also returns a [crate::`Readline`] instance associated with the writer.
///
/// Data written to a `SharedWriter` is only output when a line feed (`'\n'`) has been
/// written and either [`crate::Readline::readline()`] or [`crate::Readline::flush()`] is
/// executing on the associated `Readline` instance.
#[pin_project::pin_project]
pub struct SharedWriter {
    #[pin]
    pub(crate) buffer: Vec<u8>,
    pub(crate) sender: Sender<Vec<u8>>,
}

impl Clone for SharedWriter {
    fn clone(&self) -> Self {
        Self {
            buffer: Vec::new(),
            sender: self.sender.clone(),
        }
    }
}

impl AsyncWrite for SharedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut this = self.project();
        this.buffer.extend_from_slice(buf);
        if this.buffer.ends_with(b"\n") {
            let fut = this.sender.send_ref();
            pin_mut!(fut);
            let mut send_buf = ready!(fut.poll_unpin(cx)).map_err(|_| {
                io::Error::new(io::ErrorKind::Other, "thingbuf receiver has closed")
            })?;
            // Swap buffers
            std::mem::swap(send_buf.deref_mut(), &mut this.buffer);
            this.buffer.clear();
            Poll::Ready(Ok(buf.len()))
        } else {
            Poll::Ready(Ok(buf.len()))
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut this = self.project();
        let fut = this.sender.send_ref();
        pin_mut!(fut);
        let mut send_buf = ready!(fut.poll_unpin(cx))
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "thingbuf receiver has closed"))?;
        // Swap buffers
        std::mem::swap(send_buf.deref_mut(), &mut this.buffer);
        this.buffer.clear();
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        if self.buffer.ends_with(b"\n") {
            match self.sender.try_send_ref() {
                Ok(mut send_buf) => {
                    std::mem::swap(send_buf.deref_mut(), &mut self.buffer);
                    self.buffer.clear();
                }
                Err(TrySendError::Full(_)) => return Err(io::ErrorKind::WouldBlock.into()),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "thingbuf receiver has closed",
                    ));
                }
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
