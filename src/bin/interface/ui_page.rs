//! Contains the traits and data structures to represent the UI pages of a client facing `Interface`
//!
use std::fmt::Display;
use std::{print, println, panic, todo};
use std::marker::Unpin;
use async_std::{
    io::{Read, ReadExt}
};

use super::*;
use rusty_chat::prelude::*;

/// A state machine that represents all pages of the UI.
///
/// `UIPage` is a state machine that keeps track of the logic and state for each stage
/// of a clients interaction with the chatroom server.
///
#[derive(Debug)]
pub enum UIPage {
    WelcomePage,
    UsernamePage,
    LobbyPage { username: String, lobby_state: Vec<u8> },
    Chatroom { username: String, chatroom_name: String },
}

impl UIPage {
    pub fn new() -> UIPage {
        UIPage::WelcomePage
    }

    pub async fn state_from_response<R: ReadExt + Unpin>(self, server_stream: R) -> Result<(), UserError> {
        let mut server_stream = server_stream;

        // Attempt to parse a response from the given server stream
        let response = Response::try_parse(&mut server_stream)
            .await
            .map_err(|e| UserError::ParseResponse(e))?;

        // Match on self, executing appropriate logic
        match self {

            UIPage::WelcomePage => {
                if !response.is_connection_ok() {
                    // Something wrong happened on the server's side, this should be kept hidden from
                    // client so display an InternalServerError instead
                    return Err(UserError::InternalServerError("an internal server error occurred"));
                }
                println!("{:-^80}", "Welcome to Rusty Chat!");
                println!("A chatroom built with rust using a command line interface");
                println!();
                print!("Please enter your username: ");
                UIPage::UsernamePage
            }

            UIPage::UsernamePage => {
                println!("{}", "-".repeat(80));
                match response {
                    Response::UsernameAlreadyExists { username} => {
                        println!("Sorry, but '{}' is already taken", username);
                        println!("Please enter your username: ");
                        UIPage::UsernamePage
                    }
                    Response::UsernameOk { username, lobby_state} => {
                        // Inform client chosen username is ok
                        println!("Welcome {}", username);

                        // Display lobby state to user

                        // Create new Lobby page and return
                        todo!()
                    }
                    _ => todo!()
                }
            }
            _ => panic!()
        }

        Ok(())
    }
}