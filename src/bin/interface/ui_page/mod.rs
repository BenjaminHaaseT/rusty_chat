//! Contains the data structure to represent the UI pages of a client facing `Interface`,
//! namely `UIPage`.

use std::fmt::Display;
use std::thread;
use std::time::Duration;
use std::marker::Unpin;
use std::io::{Stdout, stdout, stdin, Read as StdRead, Write as StdWrite};
use async_std::{
    io::{Read, ReadExt, Write, WriteExt, BufRead},
    task,
};

use async_std::io::prelude::BufReadExt;
use termion::{clear, cursor, style, color, raw::IntoRawMode, raw::RawTerminal, input::TermRead, event::Key, screen::{IntoAlternateScreen, AlternateScreen}};
use tracing::{instrument, info, error, debug, warn};

use crate::UserError;
use rusty_chat::prelude::*;

mod chat;
use chat::prelude::*;

/// A state machine that represents all pages of the UI.
///
/// `UIPage` is a state machine that keeps track of the logic and state for each stage
/// of a clients interaction with the chatroom server.
#[derive(Debug)]
pub enum UIPage {
    /// Initial page the client will see first in the progression
    WelcomePage,

    /// Page that represents the username prompt
    UsernamePage,

    /// Page that represents the state when the client is inside the lobby
    LobbyPage { username: String, lobby_state: ChatroomFrames },

    /// Page that represents the state when the client is inside a chatroom
    Chatroom { username: String, chatroom_name: String},

    /// Represents the state when the client has quit a chatroom and is rejoining the lobby
    QuitChatroom { username: String, chatroom_name: String },

    /// Represents the state that signals the client has quit the chatroom program altogether
    QuitLobbyPage,
}

impl UIPage {
    /// Returns a new 'UIPage'. Always returns the 'WelcomePage' variant since this is the
    /// first page that every client should see.
    pub fn new() -> UIPage {
        UIPage::WelcomePage
    }

    /// The state transition function.
    ///
    /// Takes ownership of 'self' and a connection to a server `from_server`, attempts to parse a response received and
    /// executes the associated logic for transitioning the the UI to the next state based on the response
    /// received. This method consumes 'self' and returns the next state of the UI after the appropriate logic
    /// is executed.
    #[instrument(ret, err, skip_all)]
    pub async fn state_from_response<R: ReadExt + Unpin>(self, out: &mut RawTerminal<Stdout>, from_server: R) -> Result<UIPage, UserError> {
        // So it can be read from
        let mut from_server = from_server;

        // Attempt to parse a response from the given connection
        debug!("Attempting to parse response from server...");
        let response = Response::try_parse(&mut from_server)
            .await
            .map_err(|e| UserError::ParseResponse(e))?;

        // Match on self, displaying an appropriate message to the client,
        // execute the logic to transition to the next state, and finally return the next UIPage in the progression
        match self {
            UIPage::WelcomePage => UIPage::welcome_page_state_from_response_helper(response, out),
            UIPage::UsernamePage => UIPage::username_page_state_from_response_helper(response, out),
            UIPage::LobbyPage {username, lobby_state} => UIPage::lobby_page_state_from_response_helper(username, from_server, response, out).await,
            UIPage::Chatroom {username, chatroom_name} => UIPage::chatroom_state_from_response_helper(username, response),
            UIPage::QuitChatroom {username, chatroom_name} => UIPage::quit_chatroom_state_from_response_helper(username, response),
            _ => return Err(UserError::InternalServerError("an internal server error occurred")),
        }
    }

    /// Processes a request from the client based on the current state of 'self'.
    ///
    /// Gets input from the client, validates the input based on the current state of 'self' and then
    /// writes the appropriate 'Frame' to 'to_server'.
    /// Takes three input streams, 'from_client' for client input, 'to_server' for sending 'Frames' to
    /// the server, and 'from_server' for receiving responses from the server. Note 'from_server' is
    /// used whenever the 'UIPage' is in the 'Chatroom' state, so the chatroom procedure can be run.
    #[instrument(ret, err, skip_all)]
    pub async fn process_request<R, W>(&self, out: &mut RawTerminal<Stdout>, to_server: W, from_server: R) -> Result<(), UserError>
    where
        R: ReadExt + Unpin,
        W: WriteExt + Unpin,
    {
        // For writing to out in with consistent style
        // let mut out = stdout().into_raw_mode().map_err(|e| UserError::RawOutput(e))?;
        // General idea: Display request prompt to client on out based on the current state of self
        // Read client's input from 'from_client', parse accordingly with error handling.
        // Then send request to server. Note that the state of self should only be 'UsernamePage', 'ChatroomPage', 'Lobby' or 'QuitChatroom'
        // let mut from_client = from_client;
        let mut to_server = to_server;
        let mut from_server = from_server;

        match self {
            // If this matches, we know client has successfully connected to the server.
            // So client needs to be prompted to enter a username.
            UIPage::UsernamePage => UIPage::username_process_request_helper(out, to_server).await,
            // If this matches, the client has successfully set their username and is now in
            // the lobby of the chatroom server. We need to display the state of the lobby and
            // prompt for users input.
            UIPage::LobbyPage {username, lobby_state} => UIPage::lobby_process_request_helper(username, lobby_state, out, to_server).await,
            // If this matches, the client has successfully created/joined a new chatroom
            // the chatroom procedure needs to be executed from this branch.
            UIPage::Chatroom {username, chatroom_name} => UIPage::chatroom_process_request_helper(username, chatroom_name, out, to_server, from_server).await,
            // If this matches, the client has successfully quit from a chatroom.
            UIPage::QuitChatroom {username, chatroom_name} => UIPage::quit_chatroom_process_request_helper(to_server).await,
            _ => unreachable!()
        }
    }

    /// Helper method for `state_from_response`, renders the `WelcomePage` and returns the next
    /// state in the success case.
    #[instrument(ret, err, skip(out))]
    fn welcome_page_state_from_response_helper(response: Response, out: &mut RawTerminal<Stdout>) -> Result<UIPage, UserError> {
        if !response.is_connection_ok() {
            // Something wrong happened on the server's side, this should be kept hidden from
            // client so display an InternalServerError instead
            return Err(UserError::InternalServerError("an internal server error occurred"));
        }
        // Display the state to the client
        write!(
            out, "{}{}{}{}{}{:-^80}{}{}",
            clear::BeforeCursor, clear::AfterCursor, cursor::Goto(1, 1),
            color::Fg(color::Rgb(116, 179, 252)),
            style::Bold, "Welcome To Rusty Chat!",
            style::Reset, color::Fg(color::Reset)
        ).map_err(|e| UserError::WriteError(e))?;
        out.flush().map_err(|e| UserError::WriteError(e))?;

        // Return the next state that should be transitioned to
        Ok(UIPage::UsernamePage)
    }

    /// Helper method for `state_from_response`, renders the `UsernamePage` and returns the next
    /// state in the success case.
    #[instrument(ret, err, skip(out))]
    fn username_page_state_from_response_helper(response: Response, out: &mut RawTerminal<Stdout>) -> Result<UIPage, UserError> {
        match response {
            Response::UsernameAlreadyExists { username} => {
                // Write feedback to client
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, 2), clear::CurrentLine,
                    color::Fg(color::Rgb(202, 227, 113)),
                    format!("Sorry, but '{}' is already taken", username),
                    color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;

                Ok(UIPage::UsernamePage)
            }
            Response::UsernameOk { username, lobby_state} => {
                // Parse the lobby state
                let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                    .map_err(|e| UserError::ParseLobby(e))?;

                // Ensure frames are sorted by name
                chatroom_frames.frames.sort_by(|f1, f2| f1.name.cmp(&f2.name));

                // Create new Lobby page and return
                Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames })
            }
            _ => Err(UserError::InternalServerError("an internal server error occurred")),
        }
    }

    /// Helper method for `state_from_response`, renders the `LobbyPage` and returns the next
    /// state in the success case.
    #[instrument(ret, err, skip(from_server, out))]
    async fn lobby_page_state_from_response_helper<R: ReadExt + Unpin>(
        username: String,
        mut from_server: R,
        response: Response,
        out: &mut RawTerminal<Stdout>
    ) -> Result<UIPage, UserError> {
        match response {
            Response::ExitLobby => {
                // Goodbye message
                write!(out, "{}{}", clear::BeforeCursor, clear::AfterCursor).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, 1), clear::All,
                    color::Fg(color::Cyan), format!("Goodbye {}", username),
                    color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;

                // Reset cursor
                write!(out, "{}", cursor::Show).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;

                // Signals to interface that main loop should exit
                Ok(UIPage::QuitLobbyPage)
            }
            Response::Subscribed {chatroom_name} => {
                // Needs to start chatroom_prompt routine. First await for confirmation that
                // client's read/write tasks are both connected to chatroom broker.
                let confirmation_response = Response::try_parse(&mut from_server)
                    .await
                    .map_err(|_| UserError::InternalServerError("connection to chatroom failed"))?;

                // We need to ensure that we receive Response::ReadSync variant, this should be the only
                // response that the client receives during this state
                if !confirmation_response.is_read_sync() {
                    return Err(UserError::InternalServerError("an internal server error occurred"));
                }
                // Display message to client they have successfully connected to the chatroom

                // Chatroom page signals that chatroom procedure needs to be started
                Ok(UIPage::Chatroom { username, chatroom_name})
            }
            Response::ChatroomCreated {chatroom_name} => {
                // Needs to start chatroom_prompt routine. First await for confirmation that
                // client's read/write tasks are both connected to chatroom broker.
                let confirmation_response = Response::try_parse(&mut from_server)
                    .await
                    .map_err(|_| UserError::InternalServerError("connection to chatroom failed"))?;

                // We need to ensure that we receive Response::ReadSync variant, this should be the only
                // response that the client receives during this state
                if !confirmation_response.is_read_sync() {
                    return Err(UserError::InternalServerError("an internal server error occurred"));
                }

                // Chatroom page signals that chatroom procedure needs to be started
                Ok(UIPage::Chatroom { username, chatroom_name})
            }
            Response::ReadSync => {
                // Needs to start chatroom_prompt routine. First await for confirmation that
                // client's read/write tasks are both connected to chatroom broker.
                let confirmation_response = Response::try_parse(&mut from_server)
                    .await
                    .map_err(|_| UserError::InternalServerError("connection to chatroom failed"))?;
                match confirmation_response {
                    Response::ChatroomCreated {chatroom_name} => {
                        // Chatroom page signals that chatroom procedure needs to be started
                        Ok(UIPage::Chatroom { username, chatroom_name})
                    },
                    Response::Subscribed {chatroom_name} => {
                        // Chatroom page signals that chatroom procedure needs to be started
                        Ok(UIPage::Chatroom { username, chatroom_name})
                    },
                    _ => Err(UserError::InternalServerError("an internal server error occurred")),
                }
            }
            Response::ChatroomDoesNotExist {chatroom_name, lobby_state} => {
                // Display feedback to client
                write!(
                    out, "{}{}{}{}{}{}",
                    cursor::Goto(1, 1), clear::BeforeCursor, clear::AfterCursor,
                    color::Fg(color::Rgb(202, 227, 113)), format!("Chatroom: {} no longer exists. Refreshing lobby...", chatroom_name),
                    color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
                // Ensure thread sleeps so client can read feedback before UI updates
                task::spawn_blocking(|| {
                    thread::sleep(Duration::from_millis(1750));
                }).await;
                // println!("Unable to join {}, chatroom: {} does not exist. Please select another option.", chatroom_name, chatroom_name);
                // Parse the lobby state
                let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                    .map_err(|e| UserError::ParseLobby(e))?;
                chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));
                Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames})
            }
            Response::ChatroomAlreadyExists {chatroom_name, lobby_state} => {
                // Display feedback to client
                write!(
                    out, "{}{}{}{}{}{}",
                    cursor::Goto(1, 1), clear::BeforeCursor, clear::AfterCursor,
                    color::Fg(color::Rgb(202, 227, 113)), format!("Chatroom: {} already created. Refreshing lobby...", chatroom_name),
                    color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;

                task::spawn_blocking(|| {
                    thread::sleep(Duration::from_millis(1750));
                }).await;

                // Parse the lobby state
                let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                    .map_err(|e| UserError::ParseLobby(e))?;

                chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));
                Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames})
            }
            _ => Err(UserError::InternalServerError("an internal server error occurred"))
        }
    }

    /// Helper method for `state_from_response`. Returns the next state in the success case.
    #[instrument(ret, err)]
    fn chatroom_state_from_response_helper(username: String, response: Response) -> Result<UIPage, UserError> {
        // We just need to ensure the client receives a ExitChatroom response
        // and return a QuitChatroom variant
        match response {
            Response::ExitChatroom {chatroom_name} => {
                Ok(UIPage::QuitChatroom {username, chatroom_name})
            }
            _ => Err(UserError::InternalServerError("an internal server error occurred")),
        }
    }

    /// Helper method for `state_from_response`, parses the response and returns the next state in the success case.
    #[instrument(ret, err)]
    fn quit_chatroom_state_from_response_helper(username: String, response: Response) -> Result<UIPage, UserError> {
        // We just need to ensure the client receives a Lobby response
        match response {
            Response::Lobby {lobby_state} => {
                // Parse the lobby state
                let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                    .map_err(|e| UserError::ParseLobby(e))?;

                chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));
                // Create new Lobby page and return
                Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames })
            }
            _ => Err(UserError::InternalServerError("an internal server error occurred")),
        }
    }

    /// Helper method for 'process_request', renders the prompt associated with the 'UsernamePage',
    /// reads input from the user, validates the input and sends the request to the server
    /// via `to_server`.
    #[instrument(ret, err, skip_all)]
    async fn username_process_request_helper<W>(out: &mut RawTerminal<Stdout>, mut to_server: W) -> Result<(), UserError>
    where
        W: WriteExt + Unpin,
    {
        // Prompt the user for a valid username. Use a loop to validate user has entered a
        // valid username
        let username = loop {
            write!(
                out, "{}{}{}{}{}{}{}",
                cursor::Goto(1, 3), clear::CurrentLine,
                color::Fg(color::Rgb(215, 247, 241)),
                style::Underline,
                "enter desired username:",
                color::Fg(color::Reset),
                style::Reset
            ).map_err(|e| UserError::WriteError(e))?;
            out.flush().map_err(|e| UserError::WriteError(e))?;

            // Read line from client input
            debug!("Attempting to read input from client input stream");
            let selected_username = read_line_from_client_input(out, 3, 25)?;

            // Ensure we remove leading/trailing white space
            let selected_username = selected_username.trim().to_owned();
            if selected_username.is_empty() {
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, 2), clear::CurrentLine,
                    color::Fg(color::Rgb(202, 227, 113)),
                    "Username cannot be empty.",
                    color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;

            } else if selected_username.as_bytes().len() > 255 {
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, 2), clear::CurrentLine,
                    color::Fg(color::Rgb(202, 227, 113)),
                    "Username length cannot exceed 255 bytes.",
                    color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
            } else {
                break selected_username;
            }
        };

        // Create frame to send to the server
        let frame = Frame::Username { new_username: username };
        // Send frame to the server
        to_server.write_all(&frame.as_bytes())
            .await
            .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
        Ok(())
    }

    /// Helper method for 'process_request', renders the prompt associated with the 'LobbyPage',
    /// reads input from the user, validates the input and sends the request to the server
    /// via `to_server`.
    #[instrument(ret, err, skip(out, to_server))]
    async fn lobby_process_request_helper<W>(username: &String, lobby_state: &ChatroomFrames, out: &mut RawTerminal<Stdout>, mut to_server: W) -> Result<(), UserError>
    where
        W: WriteExt + Unpin
    {
        // First update the page header for the lobby
        let fmt_width = usize::max(80, username.as_bytes().len() + 31);

        write!(
            out, "{}{}{}{}{}{:-^w$}{}{}",
            clear::BeforeCursor, clear::AfterCursor, cursor::Goto(1, 1),
            color::Fg(color::Rgb(116, 179, 252)),
            style::Bold, format!("{} ~ Welcome to Rusty Chat lobby", username),
            style::Reset, color::Fg(color::Reset),
            w=fmt_width
        ).map_err(|e| UserError::WriteError(e))?;
        out.flush().map_err(|e| UserError::WriteError(e))?;

        if !lobby_state.frames.is_empty() {
            // Get max length for formatting
            let max_chatroom_name_length = usize::max(
                lobby_state.frames.iter().map(|f| f.name.len()).max().unwrap(),
                8
            );
            // Write header for chatroom/capacity table
            let capacity_width = usize::max(9, 2 * lobby_state.frames[0].capacity.to_string().len() + 1);
            write!(
                out, "{}{}{}{}{}\n",
                cursor::Goto(1, 2), clear::CurrentLine,
                color::Fg(color::Rgb(116, 179, 252)),
                format!("{:<w$} ~ {:>cap_w$}", "Chatroom", "Capacity", w=max_chatroom_name_length, cap_w=capacity_width),
                color::Fg(color::Reset)
            ).map_err(|e| UserError::WriteError(e))?;
            out.flush().map_err(|e| UserError::WriteError(e))?;
            // Write chatroom/capacity table from lobby_state
            for (i, frame) in lobby_state.frames.iter().enumerate() {
                write!(
                    out, "{}{}{}{}{}\n",
                    cursor::Goto(1, (i + 3) as u16), clear::CurrentLine,
                    color::Fg(color::Rgb(215, 247, 241)),
                    format!("{:<w$} ~ {:>cap_w$}/{:>cap_w$}", frame.name, frame.num_clients, frame.capacity, w=max_chatroom_name_length, cap_w=(capacity_width/2)),
                    color::Fg(color::Reset),
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
            }
        }

        let prompt_offset = (lobby_state.frames.len() + 4) as u16;

        // Validate users input using validation loop pattern
        let users_request = loop {

            // Write prompt
            write!(
                out, "{}{}{}{}{}",
                cursor::Goto(1, prompt_offset + 1), clear::CurrentLine,
                color::Fg(color::Rgb(215, 247, 241)),
                "[q] quit | [c] create new chatroom | [:name:] join existing chatroom",
                color::Fg(color::Reset)
            ).map_err(|e| UserError::WriteError(e))?;
            out.flush().map_err(|e| UserError::WriteError(e))?;

            // Read buffer from client input
            debug!("Attempting to read line from client input stream");
            let buf = read_line_from_client_input(out, prompt_offset + 2, 1)?;
            let buf = buf.trim().to_owned();

            // Ensure buf is not empty
            if buf.is_empty() {
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, prompt_offset), clear::AfterCursor,
                    color::Fg(color::Rgb(202, 227, 113)),
                    "Selection cannot be empty, please enter valid input.",
                    color::Fg(color::Reset),
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
            } else if buf.as_bytes().len() > 255 {
                // Validate user has entered a valid chatroom name if the buffers length is greater than 1
                // chatroom names cannot have a length greater than 255
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, prompt_offset), clear::AfterCursor,
                    color::Fg(color::Rgb(202, 227, 113)),
                    "The chatroom name you entered is too long, length cannot exceed 255 bytes.",
                    color::Fg(color::Reset),
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
            } else if buf.len() == 1 && buf != "q" && buf != "c" && lobby_state.frames.binary_search_by(|frame| frame.name.cmp(&buf)).is_err() {
                // Ensure a valid command was entered
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, prompt_offset), clear::AfterCursor,
                    color::Fg(color::Rgb(202, 227, 113)),
                    "Please select a valid option.",
                    color::Fg(color::Reset),
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
            } else if buf != "q" && buf != "c" && lobby_state.frames.binary_search_by(|frame| frame.name.cmp(&buf)).is_err() {
                // Ensure the selected chatroom name exists in the current lobby state
                write!(
                    out, "{}{}{}{}{}",
                    cursor::Goto(1, prompt_offset), clear::AfterCursor,
                    color::Fg(color::Rgb(202, 227, 113)),
                    format!("Chatroom {} does not exist.", buf),
                    color::Fg(color::Reset),
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
            } else {
                // Valid input, break
                break buf;
            }
        };

        // We can be sure that the user has entered valid input from here, so we can send the
        // request to the server
        match users_request.as_str() {
            "q" => {
                // Client is electing to quit altogether
                let frame = Frame::Quit;
                info!(frame = ?frame, "Writing request to server");
                to_server.write_all(&frame.as_bytes())
                    .await
                    .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
            },
            "c" => {
                // Validation loop pattern for getting users selected chatroom name
                write!(out, "{}{}", cursor::Goto(1, prompt_offset), clear::AfterCursor)
                    .map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;

                // Validation loop for validating users input
                let chatroom_name = loop {

                    write!(
                        out, "{}{}{}{}{}{}{}",
                        cursor::Goto(1, prompt_offset + 1), clear::AfterCursor,
                        style::Underline,
                        color::Fg(color::Rgb(215, 247, 241)),
                        "chatroom name:", color::Fg(color::Reset),
                        style::Reset,
                    ).map_err(|e| UserError::WriteError(e))?;
                    out.flush().map_err(|e| UserError::WriteError(e))?;

                    // Read buffer from client input
                    let buf = read_line_from_client_input(out, prompt_offset + 1, 16)?;
                    let buf = buf.trim().to_owned();

                    if buf.is_empty() {
                        write!(
                            out, "{}{}{}{}{}",
                            cursor::Goto(1, prompt_offset), clear::AfterCursor,
                            color::Fg(color::Rgb(202, 227, 113)),
                            "chatroom name cannot be empty, please enter a valid chatroom name",
                            color::Fg(color::Reset)
                        ).map_err(|e| UserError::WriteError(e))?;
                        out.flush().map_err(|e| UserError::WriteError(e))?;
                    } else if buf.as_bytes().len() > 255 {
                        write!(
                            out, "{}{}{}{}{}",
                            cursor::Goto(1, prompt_offset), clear::AfterCursor,
                            color::Fg(color::Rgb(202, 227, 113)),
                            "chatroom name length cannot exceed 255 bytes, please enter a valid chatroom name",
                            color::Fg(color::Reset)
                        ).map_err(|e| UserError::WriteError(e))?;
                        out.flush().map_err(|e| UserError::WriteError(e))?;
                    } else if buf == "q" || buf == "c" {
                        // Inform user chatroom name cannot be a reserved command
                        write!(
                            out, "{}{}{}{}{}",
                            cursor::Goto(1, prompt_offset), clear::AfterCursor,
                            color::Fg(color::Rgb(202, 227, 113)),
                            "chatroom name cannot be a reserved command",
                            color::Fg(color::Reset)
                        ).map_err(|e| UserError::WriteError(e))?;
                        out.flush().map_err(|e| UserError::WriteError(e))?;
                    } else {
                        break buf;
                    }
                };
                // We can be sure the client has entered a valid chatroom name from here
                // Send the frame to the server
                let frame = Frame::Create {chatroom_name};
                info!(frame = ?frame, "Writing request to server");
                to_server.write_all(&frame.as_bytes())
                    .await
                    .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
            },
            chatroom_name=> {
                let frame = Frame::Join { chatroom_name: chatroom_name.to_string() };
                info!(frame = ?frame, "Writing request to server");
                to_server.write_all(&frame.as_bytes())
                    .await
                    .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
            }
        }
        // We have successfully sent a frame to the server
        Ok(())
    }

    /// Helper method for 'process_request'. Starts the chatroom procedure.
    #[instrument(ret, err, skip(out, to_server, from_server))]
    async fn chatroom_process_request_helper<R, W>(
        username: &String,
        chatroom_name: &String,
        out: &mut RawTerminal<Stdout>,
        mut to_server: W,
        mut from_server: R
    ) -> Result<(), UserError>
    where
        R: ReadExt + Unpin,
        W: WriteExt + Unpin,
    {
        // Clear the current window
        write!(out, "{}{}", clear::BeforeCursor, clear::AfterCursor).map_err(|e| UserError::WriteError(e))?;
        out.flush().map_err(|e| UserError::WriteError(e))?;

        // Proceed with an alternate screen until chatroom procedure has finished
        let mut alt_out = stdout().into_raw_mode().unwrap().into_alternate_screen().unwrap();
        write!(
            alt_out, "{}{}",
            cursor::Goto(1, 1), clear::All,
        ).map_err(|e| UserError::WriteError(e))?;
        out.flush().map_err(|e| UserError::WriteError(e))?;

        // Start the chatroom procedure
        chat_window_task(username, chatroom_name, &mut from_server, &mut to_server).await?;
        Ok(())
    }

    /// Helper method for 'process_request'. Attempts to send a request to the server
    /// for refreshing the lobby
    #[instrument(ret, err, skip_all)]
    async fn quit_chatroom_process_request_helper<W>(mut to_server: W) -> Result<(), UserError>
    where
        W: WriteExt + Unpin
    {
        // Client just needs to send a request to server to refresh their lobby state
        to_server.write_all(&Frame::Lobby.as_bytes())
            .await
            .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
        Ok(())
    }



    /// Returns true if the 'UIPage' is the 'QuitLobbyPage' variant, false otherwise.
    pub fn is_quit_lobby(&self) -> bool {
        match self {
            UIPage::QuitLobbyPage => true,
            _ => false
        }
    }
}

/// Helper function for reading client input from stdin while using 'IntoRawMode' trait with
/// out.
#[instrument(ret, err, skip(out))]
fn read_line_from_client_input(out: &mut RawTerminal<Stdout>, row: u16, col: u16) -> Result<String, UserError> {
    let mut keys = stdin().keys();
    let mut buf = String::new();
    loop {
        let Some(key_res) = keys.next() else { return Err(UserError::ReadInput("unable to read key event from standard in")) };
        match key_res {
            Ok(Key::Char('\n')) => {
                write!(out, "{}", clear::CurrentLine).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
                break;
            }
            Ok(Key::Char(c)) => {
                write!(
                    out, "{}{}{}{}",
                    cursor::Goto(col + buf.len() as u16, row),
                    color::Fg(color::Rgb(215, 247, 241)),
                    c, color::Fg(color::Reset)
                ).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
                buf.push(c);
            }
            Ok(Key::Backspace) if buf.len() > 0 => {
                write!(out, "{}{}", cursor::Left(1), clear::AfterCursor).map_err(|e| UserError::WriteError(e))?;
                out.flush().map_err(|e| UserError::WriteError(e))?;
                buf.pop();
            }
            Ok(_) => {}
            Err(e) => return Err(UserError::ReadError(e))
        }
    }
    Ok(buf)
}