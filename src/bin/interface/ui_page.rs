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

/// A state machine that represents all pages of the UI.
///
/// `UIPage` is a state machine that keeps track of the logic and state for each stage
/// of a clients interaction with the chatroom server.
#[derive(Debug)]
pub enum UIPage {
    WelcomePage,
    UsernamePage,
    LobbyPage { username: String, lobby_state: ChatroomFrames },
    Chatroom { username: String, chatroom_name: String},
    QuitChatroom { username: String, chatroom_name: String },
    QuitLobbyPage,
}

impl UIPage {
    pub fn new() -> UIPage {
        UIPage::WelcomePage
    }

    pub async fn state_from_response<R: ReadExt + Unpin>(self, from_server: R) -> Result<UIPage, UserError> {
        let mut from_server = from_server;

        // Attempt to parse a response from the given server stream
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
                println!("A chatroom built with rust using a command line interface");
                println!();
                // Return the next state that should be transitioned to
                return Ok(UIPage::UsernamePage);
            },
            UIPage::UsernamePage => {
                println!("{}", "-".repeat(80));
                match response {
                    Response::UsernameAlreadyExists { username} => {
                        println!("Sorry, but '{}' is already taken", username);
                        return Ok(UIPage::UsernamePage);
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
                        return Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames });
                    }
                    _ => return Err(UserError::InternalServerError("an internal server error occurred")),
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
                        if !confirmation_response.is_read_sync() {
                            return Err(UserError::InternalServerError("an internal server error occurred"));
                        }
                        // Display message to client they have successfully connected to the chatroom
                        println!("You have successfully connected to {}", chatroom_name);

                        // Chatroom page signals that chatroom procedure needs to be started
                        return Ok(UIPage::Chatroom { username, chatroom_name})
                    }
                    Response::ChatroomCreated {chatroom_name} => {
                        // Needs to start chatroom_prompt routine. First await for confirmation that
                        // client's read/write tasks are both connected to chatroom broker.
                        let confirmation_response = Response::try_parse(&mut from_server)
                            .await
                            .map_err(|_| UserError::InternalServerError("connection to chatroom failed"))?;
                        if !confirmation_response.is_read_sync() {
                            return Err(UserError::InternalServerError("an internal server error occurred"));
                        }

                        // Display message to client they have successfully connected to the chatroom
                        println!("You have successfully created the chatroom {}", chatroom_name);

                        // Chatroom page signals that chatroom procedure needs to be started
                        return Ok(UIPage::Chatroom { username, chatroom_name})
                    }
                    Response::ReadSync => {
                        // Needs to start chatroom_prompt routine. First await for confirmation that
                        // client's read/write tasks are both connected to chatroom broker.
                        let confirmation_response = Response::try_parse(&mut from_server)
                            .await
                            .map_err(|_| UserError::InternalServerError("connection to chatroom failed"))?;

                        let chatroom_name = match confirmation_response {
                            Response::ChatroomCreated {chatroom_name} => chatroom_name,
                            Response::Subscribed {chatroom_name} => chatroom_name,
                            _ => return Err(UserError::InternalServerError("an internal server error occurred")),
                        };

                        // Display message to client they have successfully connected to the chatroom
                        println!("You have successfully connected to {}", chatroom_name);

                        // Chatroom page signals that chatroom procedure needs to be started
                        return Ok(UIPage::Chatroom { username, chatroom_name})
                    }
                    Response::ChatroomDoesNotExist {chatroom_name, lobby_state} => {
                        println!("{}", "-".repeat(80));
                        println!("Unable to join {}, chatroom: {} does not exist. Please select another option.", chatroom_name, chatroom_name);
                        // Parse the lobby state
                        let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                            .map_err(|e| UserError::ParseLobby(e))?;

                        chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));

                        for frame in &chatroom_frames.frames {
                            println!("{}: {}/{}", frame.name, frame.num_clients, frame.capacity);
                        }

                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                        return Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames});
                    }
                    Response::ChatroomAlreadyExists {chatroom_name, lobby_state} => {
                        println!("{}", "-".repeat(80));
                        println!("Unable to create chatroom {}, chatroom: {} already exists. Please select another option.", chatroom_name, chatroom_name);
                        // Parse the lobby state
                        let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                            .map_err(|e| UserError::ParseLobby(e))?;

                        chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));

                        for frame in &chatroom_frames.frames {
                            println!("{}: {}/{}", frame.name, frame.num_clients, frame.capacity);
                        }

                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                        return Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames});
                    }
                    _ => return Err(UserError::InternalServerError("an internal server error occurred"))
                }
            }
            UIPage::Chatroom {username, chatroom_name} => {
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
                match response {
                    Response::Lobby {lobby_state} => {
                        // Parse the lobby state
                        let mut chatroom_frames = ChatroomFrames::try_from(lobby_state)
                            .map_err(|e| UserError::ParseLobby(e))?;

                        chatroom_frames.frames.sort_by(|frame1, frame2| frame1.name.cmp(&frame2.name));

                        for frame in &chatroom_frames.frames {
                            println!("{}: {}/{}", frame.name, frame.num_clients, frame.capacity);
                        }

                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");

                        // Create new Lobby page and return
                        Ok(UIPage::LobbyPage {username, lobby_state: chatroom_frames })
                    }
                    _ => Err(UserError::InternalServerError("an internal server error occurred")),
                }
            }
            _ => return Err(UserError::InternalServerError("an internal server error occurred")),
        }
    }

    pub async fn process_request<B: BufReadExt + Unpin, R: ReadExt + Unpin, W: WriteExt + Unpin>(&self, from_client: B, mut to_server: W, mut from_server: R) -> Result<(), UserError> {
        // General idea: Display request prompt to client on stdout based on the current state of self
        // Read client's input from 'from_client', parse accordingly with error handling.
        // Then send request to server. Note that the state of self should only be 'UsernamePage', 'ChatroomPage', 'Lobby' or 'QuitChatroom'
        let mut from_client = from_client.lines();
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
                        .map_err(|_| UserError::ReadInput("error reading input from standard in"))?;

                    if selected_username.is_empty() {
                        println!("A valid username cannot be empty");
                        continue;
                    }
                    // Trim new line character
                    // let selected_username = selected_username.trim_end_matches('\n').to_string();
                    // Ensure selected username's length can fit into a single byte
                    if selected_username.len() > 255 {
                        println!("Username: {} is too long. Chosen username must be between 1 and 255 characters in length", selected_username);
                        continue;
                    }
                    break selected_username
                };

                // Create frame to send to the server
                let frame = Frame::Username { new_username: username };

                // Send frame to the server
                to_server.write_all(frame.as_bytes().as_slice())
                    .await
                    .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;

                Ok(())
            }

            // If this matches, the client has successfully set their username and is now in
            // the lobby of the chatroom server. We need to display the state of the lobby and
            // prompt for users input.
            UIPage::LobbyPage {username, lobby_state} => {
                for frame in &lobby_state.frames {
                    println!("{}: {}/{}", frame.name, frame.num_clients, frame.capacity);
                }

                println!();
                println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");

                // Get users input
                // let mut users_request = String::new();
                let users_request = loop {
                    let mut buf = String::new();
                    from_client.read_line(&mut buf)
                        .await
                        .map_err(|_| UserError::ReadInput("error reading input from standard in"))?;
                    if buf.is_empty() {
                        println!("please enter valid input");
                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                        println!();
                        continue;
                    }
                    // Remove line break
                    // let buf = buf.trim_end_matches('\n').to_string();

                    // Validate user has entered a valid chatroom name if the buffers length is greater than 1
                    // chatroom names cannot have a length greater than 255
                    if buf.len() > 255 {
                        println!("the chatroom name you entered is too long");
                        println!("please enter valid input");
                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                        println!();
                    } else if buf.len() > 1 && lobby_state.frames.binary_search_by(|frame| frame.name.cmp(&buf)).is_err() {
                        println!("chatroom {} does not exist", buf);
                        println!("please enter valid input");
                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                        println!();
                    } else if buf.len() == 1 && buf != "q" && buf != "c" {
                        println!("please enter valid input");
                        println!();
                        println!("[q] quit | [c] create new chatroom | [:name:] join existing chatroom");
                        println!();
                    } else {
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
                                .map_err(|_| UserError::ReadInput("error reading input from standard in"))?;
                            // let buf = buf.trim_end_matches('\n').to_string();
                            if buf.is_empty() {
                                println!("chatroom name cannot be empty, please enter valid input.");
                            } else if buf.len() > 255 {
                                println!("chatroom name length cannot exceed 255 characters, please enter valid input.");
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
                    },
                    _ => unreachable!()
                }

                // We have successfully sent a frame to the server
                Ok(())
            }

            // If this matches, the client has successfully created/joined a new chatroom
            // the chatroom procedure needs to be executed from this branch.
            UIPage::Chatroom {username, chatroom_name} => {
                todo!()
            }

            // If this matches, the client has successfully quit from a chatroom.
            UIPage::QuitChatroom {username, chatroom_name} => {
                // Client just needs to send a request to server to refresh their lobby state
                println!("Re-entering lobby...");
                to_server.write_all(&Frame::Lobby.as_bytes())
                    .await
                    .map_err(|_| UserError::InternalServerError("an internal server error occurred"))?;
            }
            _ => unreachable!()
        }

    }


}