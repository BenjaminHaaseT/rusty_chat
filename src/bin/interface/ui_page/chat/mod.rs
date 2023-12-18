use std::io::{Write, stdin, stdout};
use std::thread;
use async_std::{
    channel::{Sender, Receiver, unbounded},

};
use futures::{Future, FutureExt, Stream, StreamExt, select, stream};
use termion::{clear, cursor, style, color, input::TermRead, event::Key, raw::IntoRawMode, async_stdin};
use crate::UserError;
pub mod prelude {
    pub use super::*;
}

pub enum Null {}

pub async fn keyboard_input_task(to_window: Sender<String>, shutdown: Sender<Null>) -> Result<(), UserError> {
    // Get asynchronous stream of key events as a stream
    let mut keyboard = stream::iter(async_stdin().keys()).fuse();
    // For writing to the console
    let mut stdout = stdout().into_raw_mode().map_err(|_| UserError::RawOutput("unable stdout handle as raw mode"))?;
    // For collecting the characters of a message, to be sent to the chat-window task
    let mut buf = String::new();
    loop {
        match keyboard.select_next_some().await {
            Ok(Key::Char('\n')) => {
                // Get a lock on stdout, needs to be in a separate scope since stdout blocks,
                // i.e. cannot be held across an .await call.
                {
                    // Reset the users prompt
                    let mut guard = stdout.lock();
                    write!(guard, "{}{}", cursor::Goto(4, 18), clear::AfterCursor)
                        .map_err(|_| UserError::WriteError(String::from("unable to write to standard output")))?;
                    guard.flush().map_err(|_| UserError::WriteError(String::from("unable to flush stdout handle")))?;
                }
                // Send buf that contains new message to chat-window
                to_window.send(buf)
                    .await
                    .map_err(|_| UserError::SendError("keyboard input task unable to send message to chat-window task"))?;
                buf = String::new();
            }
            Ok(Key::Char(c)) => {
                // Write the new character to the prompt, lock stdout again so we get exclusive access
                let mut guard = stdout.lock();
                write!(guard, "{}{}", cursor::Goto(4 + buf.len() as u16, 18), c)
                    .map_err(|_| UserError::WriteError(format!("unable to write {} to standard output", c)))?;
                guard.flush()
                    .map_err(|_| UserError::WriteError(String::from("unable to flush stdout handle")))?;
                // push the new character into the buffer
                buf.push(c);
            }
            Ok(Key::Ctrl('q')) => {
                todo!()
            }
            Ok(Key::Backspace) => {
                todo!()
            }
            _ => {}
        }
    }

    // todo!()
}