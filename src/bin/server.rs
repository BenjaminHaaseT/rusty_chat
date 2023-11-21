use std::fmt::Debug;
use async_std::channel::{self, Sender, Receiver};
use async_std::net::ToSocketAddrs;
use async_std::net::{TcpListener, TcpStream};
use async_std::task;
use futures::{FutureExt, StreamExt};
use uuid::Uuid;

use crate::server_error::ServerError;
use rusty_chat::Event;

mod chatroom_task;
mod server_error;

async fn accept_loop(server_addrs: impl ToSocketAddrs + Clone + Debug, channel_buf_size: usize) -> Result<(), ServerError> {
    println!("listening at {:?}...", server_addrs);

    let mut listener = TcpListener::bind(server_addrs.clone())
        .await
        .map_err(|_| ServerError::ConnectionFailed)?;

    let (broker_sender, broker_receiver) = channel::bounded::<Event>(channel_buf_size);

    // spawn broker task
    task::spawn(broker(broker_receiver));

    while let Some(stream) = listener.incoming().next().await {
        let stream = stream.map_err(|_| ServerError::ConnectionFailed)?;
        println!("accepting client connection: {:?}", stream.peer_addr());
        task::spawn(handle_connection(broker_sender.clone(), stream));
    }

    Ok(())
}

async fn handle_connection(broker_sender: Sender<Event>, client_stream: TcpStream) -> Result<(), ServerError> {
    let client_id = Uuid::new_v4();

    todo!();
}

async fn broker(event_receiver: Receiver<Event>) -> Result<(), ServerError> {
    todo!()
}



fn main() {todo!()}