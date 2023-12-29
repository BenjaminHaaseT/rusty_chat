use std::io::{Write, stdin, stdout};
use std::marker::Unpin;
use std::thread;
use std::collections::VecDeque;
use async_std::{
    channel::{Sender, Receiver, unbounded},
    io::{WriteExt, ReadExt},
    task,
};
use futures::{Future, FutureExt, Stream, StreamExt, select, stream};
use termion::{clear, cursor, style, color, input::TermRead, event::Key, raw::IntoRawMode};
use crate::UserError;
use rusty_chat::prelude::*;
pub mod prelude {
    pub use super::*;
}

pub enum Null {}

async fn keyboard_input_task(to_window: Sender<String>, shutdown: Sender<Null>) -> Result<(), UserError> {
    // Get asynchronous stream of key events
    let mut keyboard = stream::iter(stdin().keys());
    // For writing to the console
    let mut stdout = stdout().into_raw_mode().map_err(|e| UserError::RawOutput(e))?;
    // For collecting the characters of a message, to be sent to the chat-window task
    let mut buf = String::new();
    while let Some(k) = keyboard.next().await {
        match k {
            Ok(Key::Char('\n')) => {
                // Get a lock on stdout, needs to be in a separate scope since stdout blocks,
                // i.e. cannot be held across an .await call.
                {
                    // Reset the users prompt
                    let mut guard = stdout.lock();
                    write!(guard, "{}{}", cursor::Goto(4, 12), clear::AfterCursor)
                        .map_err(|e| UserError::WriteError(e))?;
                    guard.flush().map_err(|e| UserError::WriteError(e))?;
                    Ok::<(), UserError>(())
                }?;
                // Send buf that contains new message to chat-window
                to_window.send(buf)
                    .await
                    .map_err(|_| UserError::SendError("keyboard input task unable to send message to chat-window task"))?;
                buf = String::new();
            }
            Ok(Key::Char(c)) => {
                // Write the new character to the prompt, lock stdout again so we get exclusive access
                let mut guard = stdout.lock();
                write!(guard, "{}{}{}{}", cursor::Goto(4 + buf.len() as u16, 12), color::Fg(color::Rgb(215, 247, 241)), c, color::Fg(color::Reset))
                    .map_err(|e| UserError::WriteError(e))?;
                guard.flush()
                    .map_err(|e| UserError::WriteError(e))?;
                // push the new character into the buffer
                buf.push(c);
            }
            Ok(Key::Ctrl('q')) => {
                let mut guard = stdout.lock();
                write!(guard, "{}{}{}{}{}", cursor::Goto(1, 12), clear::CurrentLine, color::Fg(color::Cyan), "GOODBYE", color::Fg(color::Reset))
                    .map_err(|e| UserError::WriteError(e))?;
                guard.flush()
                    .map_err(|e| UserError::WriteError(e))?;
                break;
            }
            Ok(Key::Backspace) if buf.len() > 0 => {
                // First remove the character from the prompt
                let mut guard = stdout.lock();
                write!(guard, "{}{}", cursor::Goto(4 + buf.len() as u16 - 1, 12), clear::AfterCursor)
                    .map_err(|e| UserError::WriteError(e))?;
                guard.flush().map_err(|e| UserError::WriteError(e))?;
                buf.pop();
            }
            _ => {}
        }
    }
    // Todo: Logging maybe?
    // println!("Keyboard sender channel dropped");
    Ok(())
}

pub async fn chat_window_task<'a, R, W>(username: &'a str, chatroom_name: &'a str, from_server: R, to_server: W) -> Result<(), UserError>
where
    R: ReadExt + Unpin,
    W: WriteExt + Unpin,
{
    // Get handle to stdout for displaying custom window/prompt
    let mut stdout = stdout().into_raw_mode().map_err(|e| UserError::RawOutput(e))?;

    // Display custom window and prompt
    write!(
        stdout,
        "{}{}{}{}{}{:-^80}{}{}",
        cursor::Goto(1, 1),
        clear::AfterCursor,
        cursor::Hide,
        style::Bold,
        color::Fg(color::Rgb(3, 169, 252)),
        chatroom_name,
        color::Fg(color::Reset),
        style::Reset
    )
        .map_err(|e| UserError::WriteError(e))?;
    stdout.flush()
        .map_err(|e| UserError::WriteError(e))?;

    // Write white space
    write!(stdout, "{}{}", cursor::Goto(1, 2), "\n".repeat(11))
        .map_err(|e| UserError::WriteError(e))?;
    stdout.flush()
        .map_err(|e| UserError::WriteError(e))?;

    // Write prompt window border
    write!(stdout, "{}{}{}{}{}{}\n", cursor::Goto(1, 11), style::Bold, color::Fg(color::Rgb(3, 169, 252)), "-".repeat(80), style::Reset, color::Fg(color::Reset))
        .map_err(|e| UserError::WriteError(e))?;
    stdout.flush()
        .map_err(|e| UserError::WriteError(e))?;

    // Write prompt
    write!(stdout, "{}{}{}{}", cursor::Goto(1, 12), color::Fg(color::Rgb(250, 250, 237)), ">>>", color::Fg(color::Reset))
        .map_err(|e| UserError::WriteError(e))?;
    stdout.flush()
        .map_err(|e| UserError::WriteError(e))?;

    // For synchronizing with keyboard input task
    let (keyboard_send, keyboard_recv) = unbounded::<String>();
    let (keyboard_shutdown_send, keyboard_shutdown_recv) = unbounded::<Null>();

    // Spawn the keyboard task
    let handle = thread::spawn(move || {
        task::block_on(keyboard_input_task(keyboard_send, keyboard_shutdown_send))
    });

    // Fuse receivers so they can be used in a select loop
    // println!("{}", keyboard_recv.is_closed());
    let mut keyboard_recv = keyboard_recv.fuse();
    let mut keyboard_shutdown_recv = keyboard_shutdown_recv.fuse();

    // Shadow as mutable so read/writes happen
    let mut from_server = from_server;
    let mut to_server = to_server;

    // For keeping track of messages
    let mut msg_queue = VecDeque::with_capacity(9);

    loop {
        // Select from receiving input sources
        let msg = select! {
            client_msg = keyboard_recv.next().fuse() => {
                let msg = match client_msg {
                    Some(msg) => msg,
                    None if keyboard_recv.is_done() => {
                        // Todo: log instead
                        // println!("shutting down chat-window task from 'keyboard_recv'");
                        to_server.write_all(&Frame::Quit.as_bytes())
                        .await
                        .map_err(|e| UserError::WriteError(e))?;
                        break;
                    }
                    _ => return Err(UserError::ReceiveError("received 'None' from unfinished stream 'keyboard_recv'")),
                };
                let abridged_msg = format!("{}: {}", username, msg);
                // Send message to server
                to_server.write_all(&Frame::Message{message: abridged_msg.clone()}.as_bytes())
                    .await
                    .map_err(|e| UserError::WriteError(e))?;
                abridged_msg
            },
            server_msg = Response::try_parse(&mut from_server).fuse() => {
                let Ok(Response::Message {peer_id, msg}) = server_msg else {
                    return Err(UserError::InternalServerError("an internal server error occurred"));
                };
                msg
            },
            shutdown_sig = keyboard_shutdown_recv.next().fuse() => {
                match shutdown_sig {
                    Some(null) => match null {},
                    None => {
                        //TODO: log shutdown signal
                        // println!("shutting down chat-window task from signal channel");
                        // Send quit frame to server
                        to_server.write_all(&Frame::Quit.as_bytes())
                            .await
                            .map_err(|e| UserError::WriteError(e))?;
                        break;
                    }
                }
            }
        };

        // Check if we have reached message capacity with the queue
        if msg_queue.len() == 9 {
            msg_queue.pop_front();
        }
        msg_queue.push_back(msg);

        // Get lock for standard out
        let mut guard = stdout.lock();
        // For keeping track of the current row
        let mut row = 10;
        for msg in msg_queue.iter().rev() {
            write!(guard, "{}{}{}{}{}", cursor::Goto(1, row), clear::CurrentLine, color::Fg(color::Rgb(215, 247, 241)), msg, color::Fg(color::Reset))
                .map_err(|e| UserError::WriteError(e))?;
            guard.flush()
                .map_err(|e| UserError::WriteError(e))?;
            row -= 1;
        }
    }

    // Reset cursor
    let mut guard = stdout.lock();
    write!(guard, "{}{}{}", cursor::Goto(1, 1), clear::All, color::Fg(color::Reset))
        .map_err(|e| UserError::WriteError(e))?;
    guard.flush()
        .map_err(|e| UserError::WriteError(e))?;

    Ok(())
}