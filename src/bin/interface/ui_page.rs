//! Contains the traits and data structures to represent the UI pages of a client facing `Interface`
//!
use std::fmt::Display;
use std::{print, println, panic, todo};
use std::marker::Unpin;
use async_std::{
    io::{Read, ReadExt}
};

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

    pub async fn state_from_response<R: ReadExt + Unpin>(self, server_stream: R) -> Result<UIPage, UserError> {
        let mut server_stream = server_stream;

        // Attempt to parse a response from the given server stream
        let response = Response::try_parse(&mut server_stream)
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
                print!("Please enter your username: ");
                // Return the next state that should be transitioned to
                return Ok(UIPage::UsernamePage);
            },
            UIPage::UsernamePage => {
                println!("{}", "-".repeat(80));
                match response {
                    Response::UsernameAlreadyExists { username} => {
                        println!("Sorry, but '{}' is already taken", username);
                        println!("Please enter your username: ");
                        return Ok(UIPage::UsernamePage);
                    }
                    Response::UsernameOk { username, lobby_state} => {
                        // Inform client chosen username is ok
                        println!("Welcome {}", username);

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
                        let confirmation_response = Response::try_parse(&mut server_stream)
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
                        let confirmation_response = Response::try_parse(&mut server_stream)
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
                        let confirmation_response = Response::try_parse(&mut server_stream)
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


}