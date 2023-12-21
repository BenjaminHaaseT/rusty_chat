//! Contains the traits and data structures to represent the UI pages of a client facing `Interface`
//!
use std::fmt::Display;
use std::{print, println, panic, todo};
use std::marker::Unpin;
use async_std::{
    io::{Read, ReadExt, Write, WriteExt, BufRead}
};
use async_std::io::prelude::BufReadExt;

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
    pub async fn state_from_response<R: ReadExt + Unpin>(self, from_server: R) -> Result<UIPage, UserError> {
        // So it can be read from
        let mut from_server = from_server;

        // Attempt to parse a response from the given connection
        let response = Response::try_parse(&mut from_server)
            .await
            .map_err(|e| UserError::ParseResponse(e))?;

        // Match on self, displaying an appropriate message to the client,
        // execute the logic to transition to the next state, and finally return the next UIPage in the progression
        match self {
            UIPage::WelcomePage => {
                if !response.is_connection_ok() {
                    // Something wrong happened on the server's side, this should be kept hidden from
                    // client so display an InternalServerError instead
                    return Err(UserError::InternalServerError("an internal server error occurred"));
                }
                // Display the state to the client
                println!("{:-^80}", "Welcome to Rusty Chat!");
                println!("A command line chatroom");
                println!();
                // Return the next state that should be transitioned to
                Ok(UIPage::UsernamePage)
            },
            UIPage::UsernamePage => {
                println!("{}", "-".repeat(80));
                match response {
                    Response::UsernameAlreadyExists { username} => {
                        println!("Sorry, but '{}' is already taken", username);
                        Ok(UIPage::UsernamePage)
                    }
                    Response::UsernameOk { username, lobby_state} => {
                        // Inform client chosen username is ok
                        println!("Welcome {}", username);

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
            },
            UIPage::LobbyPage {username, lobby_state} => {
                println!("{}", "-".repeat(80));
                match response {
                    Response::ExitLobby => {
                        println!("Goodbye {}", username);
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
                        println!("You have successfully connected to {}", chatroom_name);
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
                        // Display message to client they have successfully connected to the chatroom
                        println!("You have successfully created the chatroom {}", chatroom_name);
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
                                // Display message to client they have successfully connected to the chatroom
                                println!("You have successfully created the chatroom {}", chatroom_name);
                                // Chatroom page signals that chatroom procedure needs to be started
                                Ok(UIPage::Chatroom { username, chatroom_name})
                            },
                            Response::Subscribed {chatroom_name} => {
                                // Display message to client they have successfully connected to the chatroom
                                println!("You have successfully connected to {}", chatroom_name);
                                // Chatroom page signals that chatroom procedure needs to be started
                                Ok(UIPage::Chatroom { username, chatroom_name})
                            },
                            _ => Err(UserError::InternalServerError("an internal server error occurred")),
                        }
                    }
                    Response::ChatroomDoesNotExist {chatroom_name, lobby_state} => {
                        println!("Unable to join {}, chatroom: {} does not exist. Please select another option.", chatroom_name, chatroom_name);
                        // Parse the lobby state
                        let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                            .map_err(|e| UserError::ParseLobby(e))?;
                        chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));
                        Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames})
                    }
                    Response::ChatroomAlreadyExists {chatroom_name, lobby_state} => {
                        println!("Unable to create chatroom {}, chatroom: {} already exists. Please select another option.", chatroom_name, chatroom_name);
                        // Parse the lobby state
                        let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                            .map_err(|e| UserError::ParseLobby(e))?;

                        chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));
                        Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames})
                    }
                    _ => Err(UserError::InternalServerError("an internal server error occurred"))
                }
            }
            UIPage::Chatroom {username, chatroom_name} => {
                // We just need to ensure the client receives a ExitChatroom response
                // and return a QuitChatroom variant,
                print!("...");
                match response {
                    Response::ExitChatroom {chatroom_name} => {
                        println!("Left chatroom {}", chatroom_name);
                        Ok(UIPage::QuitChatroom {username, chatroom_name})
                    }
                    _ => Err(UserError::InternalServerError("an internal server error occurred")),
                }
            }
            UIPage::QuitChatroom {username, chatroom_name} => {
                println!("Entering lobby...");
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
    ///
    pub async fn process_request<B, R, W>(&self, from_client: &mut B, to_server: W, from_server: R) -> Result<(), UserError>
    where
        B: BufRead + BufReadExt + Unpin,
        R: ReadExt + Unpin,
        W: WriteExt + Unpin,
    {
        // General idea: Display request prompt to client on stdout based on the current state of self
        // Read client's input from 'from_client', parse accordingly with error handling.
        // Then send request to server. Note that the state of self should only be 'UsernamePage', 'ChatroomPage', 'Lobby' or 'QuitChatroom'
        // let mut from_client = from_client;
        let mut to_server = to_server;
        let mut from_server = from_server;
        match self {
            // If this matches, we know client has successfully connected to the server.
            // So client needs to be prompted to enter a username.
            UIPage::UsernamePage => {
                // Prompt the user for a valid username. Use a loop to validate user has entered a
                // valid username
                let username = loop {
                    println!("Please enter your username: ");
                    let mut selected_username = String::new();
                    from_client.read_line(&mut selected_username)
                        .await
                        .map_err(|e| UserError::ReadError(e))?;
                    // Ensure we remove leading/trailing white space
                    let selected_username = selected_username.trim().to_owned();
                    if selected_username.is_empty() {
                        println!("selected username cannot be empty");
                    } else if selected_username.as_bytes().len() > 255 {
                        println!("selected username length cannot exceed 255 bytes");
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

            // If this matches, the client has successfully set their username and is now in
            // the lobby of the chatroom server. We need to display the state of the lobby and
            // prompt for users input.
            UIPage::LobbyPage {username, lobby_state} => {
                // Get users input
                let users_request = loop {
                    for frame in &lobby_state.frames {
                        println!("{}: {}/{}", frame.name, frame.num_clients, frame.capacity);
                    }
                    println!();
                    println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                    let mut buf = String::new();
                    from_client.read_line(&mut buf)
                        .await
                        .map_err(|e| UserError::ReadError(e))?;
                    let buf = buf.trim().to_owned();
                    // Ensure buf is not empty
                    if buf.is_empty() {
                        println!("please enter valid input, input cannot be empty");
                        println!();
                    } else if buf.as_bytes().len() > 255 {
                        // Validate user has entered a valid chatroom name if the buffers length is greater than 1
                        // chatroom names cannot have a length greater than 255
                        println!("the chatroom name you entered is too long, length cannot exceed 255 bytes");
                        println!("please enter valid input");
                        println!();
                    } else if buf.len() > 1 && lobby_state.frames.binary_search_by(|frame| frame.name.cmp(&buf)).is_err() {
                        // Ensure the selected chatroom name exists in the current lobby state
                        println!("chatroom {} does not exist", buf);
                        println!("please enter valid input");
                        println!();
                    } else if buf.len() == 1 && buf != "q" && buf != "c" {
                        // Ensure a valid command was entered
                        println!("please enter valid input");
                        println!();
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
                        to_server.write_all(&Frame::Quit.as_bytes())
                            .await
                            .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
                    },
                    "c" => {
                        // Validation loop pattern for getting users selected chatroom name
                        let chatroom_name = loop {
                            println!("enter chatroom name:");
                            let mut buf = String::new();
                            from_client.read_line(&mut buf)
                                .await
                                .map_err(|e| UserError::ReadError(e))?;
                            let buf = buf.trim().to_owned();
                            if buf.is_empty() {
                                println!("chatroom name cannot be empty, please enter valid input.");
                            } else if buf.as_bytes().len() > 255 {
                                println!("chatroom name length cannot exceed 255 bytes, please enter valid input.");
                            } else {
                                break buf;
                            }
                        };
                        // We can be sure the client has entered a valid chatroom name from here
                        // Send the frame to the server
                        to_server.write_all(&Frame::Create { chatroom_name }.as_bytes())
                            .await
                            .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
                    },
                    chatroom_name=> {
                        to_server.write_all(&Frame::Join { chatroom_name: chatroom_name.to_string() }.as_bytes())
                            .await
                            .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
                    }
                }
                // We have successfully sent a frame to the server
                Ok(())
            }

            // If this matches, the client has successfully created/joined a new chatroom
            // the chatroom procedure needs to be executed from this branch.
            UIPage::Chatroom {username, chatroom_name} => {
                // Start the chatroom procedure
                chat_window_task(username, chatroom_name, &mut from_server, &mut to_server).await?;
                // client has left
                println!("Leaving chatroom {}...", chatroom_name);
                Ok(())
            }

            // If this matches, the client has successfully quit from a chatroom.
            UIPage::QuitChatroom {username, chatroom_name} => {
                // Client just needs to send a request to server to refresh their lobby state
                println!("Re-entering lobby from {}...", chatroom_name);
                to_server.write_all(&Frame::Lobby.as_bytes())
                    .await
                    .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
                Ok(())
            }
            _ => unreachable!()
        }
    }

    /// Returns true if the 'UIPage' is the 'QuitLobbyPage' variant, false otherwise.
    pub fn is_quit_lobby(&self) -> bool {
        match self {
            UIPage::QuitLobbyPage => true,
            _ => false
        }
    }


}