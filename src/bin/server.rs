use std::fmt::Debug;
use std::sync::Arc;
use async_std::io::{self, Read, ReadExt, Write, WriteExt};
use async_std::channel::{self, Sender, Receiver};
use async_std::net::ToSocketAddrs;
use async_std::net::{TcpListener, TcpStream};
use async_std::task;
use futures::{FutureExt, StreamExt};
use uuid::Uuid;

use crate::server_error::ServerError;
use rusty_chat::{*, Event, Null};

mod chatroom_task;
mod server_error;

async fn accept_loop(server_addrs: impl ToSocketAddrs + Clone + Debug, channel_buf_size: usize) -> Result<(), ServerError> {
    println!("listening at {:?}...", server_addrs);

    let mut listener = TcpListener::bind(server_addrs.clone())
        .await
        .map_err(|_| ServerError::ConnectionFailed)?;

    let (broker_sender, broker_receiver) = channel::bounded::<Event>(channel_buf_size);

    // spawn broker task
    task::spawn(broker(broker_sender.clone(),broker_receiver));

    while let Some(stream) = listener.incoming().next().await {
        let stream = stream.map_err(|_| ServerError::ConnectionFailed)?;
        println!("accepting client connection: {:?}", stream.peer_addr());
        task::spawn(handle_connection(broker_sender.clone(), stream));
    }

    Ok(())
}

async fn handle_connection(main_broker_sender: Sender<Event>, client_stream: TcpStream) -> Result<(), ServerError> {
    let client_stream = Arc::new(client_stream);
    let mut client_reader = &*client_stream;

    // Id for the client
    let peer_id = Uuid::new_v4();

    // channel for synchronizing graceful shutdown with the writer task
    let (_client_shutdown_sender, client_shutdown_receiver) = channel::unbounded::<Null>();

    // channel for orchestrating the connection between main broker, chatroom broker and client
    // read/write tasks
    let (chatroom_broker_sender, chatroom_broker_receiver) = channel::unbounded::<AsyncStdSender<Event>>();

    // new client event
    let event = Event::NewClient {
        peer_id,
        stream: client_stream.clone(),
        shutdown: client_shutdown_receiver,
        chatroom_connection: chatroom_broker_sender
    };

    // for sending message events when the client joins a new chatroom.
    let mut chatroom_sender: Option<AsyncStdSender<Event>> = None;

    // send a new client event to the main broker, should not be disconnected
    // TODO: add logging/tracing
    main_broker_sender.send(event)
        .await
        .expect("broker should be connected");

    let mut frame_tag = [0u8; 5];

    while let Ok(_) = client_reader.read_exact(&mut frame_tag) {
        // Attempt to parse the frame from the client
        todo!()
    }


    todo!()
}

async fn client_write_loop() -> Result<(), ServerError> {todo!()}

async fn broker(_event_sender: Sender<Event>, event_receiver: Receiver<Event>) -> Result<(), ServerError> {

}



fn main() {todo!()}