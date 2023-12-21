// use std::net::ToSocketAddrs;
use std::fmt::Debug;
use async_std::{
    net::{TcpStream, ToSocketAddrs},
    io::{Read, ReadExt, Write, WriteExt, BufRead, BufReader, prelude::BufReadExt, stdin}
};

use crate::interface::ui_page::UIPage;
use crate::UserError;

mod ui_page;

pub mod prelude {
    pub use super::*;
}


/// Encapsulates the client UI functionality
pub struct Interface;

impl Interface {
    /// Starts a new client connection to the chatroom server and runs the UI for the connecting client.
    pub async fn run<A: ToSocketAddrs + Debug + Clone>(addrs: A) -> Result<(), UserError> {
        // Establish connection to server
        println!("Connecting to {:?}...", addrs);
        let mut stream = TcpStream::connect(addrs).await.map_err(|e| UserError::ConnectionError(e))?;

        // Instantiate necessary state
        // For managing the UI progression to and from requests/responses
        let mut ui = UIPage::new();
        // For getting users input from standard in
        let mut from_client = BufReader::new(stdin());
        // For reading and writing to the server
        let (mut from_server, mut to_server) = (&stream, &stream);

        // main loop
        loop {
            // Attempt to parse response from server, and transition state of ui
            ui = ui.state_from_response(from_server).await?;
            if ui.is_quit_lobby() {
                break;
            }
            // Attempt to read client input and send a request to the server
            ui.process_request(&mut from_client, to_server, from_server).await?;
        }
        Ok(())
    }
}


