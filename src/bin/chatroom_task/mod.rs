//! Module that encapsulates everything needed to create a new chatroom task by the main broker task
//!

use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use async_std::net;
use async_std::channel::{bounded, unbounded};
use async_std::task;
use futures::{select, Stream, StreamExt, FutureExt};
use tokio::sync::broadcast;

use rusty_chat::prelude::*;
use crate::server_error::prelude::*;

pub async fn chatroom_broker(
    client_receiver: AsyncStdReceiver<Client>,
    chatroom_shutdown: AsyncStdSender<Null>,
    channel_buf_size: usize
) -> Result<(), ServerError> {
    // Ensure `client_receiver` is fused
    let mut client_receiver = client_receiver.fuse();

    // Channel for receiving new MessageEvents from client writer tasks
    let (client_msg_sender, client_msg_receiver) = bounded::<MessageEvent>(channel_buf_size);

    // Channel for sending received MessageEvents from client readers and broadcasting to client writers
    // `broadcast_msg_receiver` will be cloned and passed to the writing task for each client
    let (broadcast_msg_sender, broadcast_msg_receiver) = broadcast::channel::<MessageEvent>(channel_buf_size);

    // Channel for receiving harvesting disconnected clients
    let (client_disconnect_sender, client_disconnect_receiver) = unbounded::<(Client, TokioBroadcastReceiver<MessageEvent>)>();
    let mut client_disconnect_receiver = client_disconnect_receiver.fuse();

    // Hashmap for keeping track of all clients
    // let mut clients: HashMap<Uuid, Client> = HashMap::new();
    let mut clients: HashSet<Uuid> = HashSet::new();

    loop {
        // We first need to check for new clients joining, disconnecting clients, or new messages received
        let msg = select! {
            new_client = client_receiver.next().fuse() => {
                match new_client {
                    Some(mut new_client) => {
                        // add client to set of clients
                        clients.insert(new_client.id);
                        // for sending NewMessageEvents from a client to the chatroom broker
                        let client_msg_sender_clone = client_msg_sender.clone();
                        // for subscribing to the NewMessageEvents broadcast by the chatroom broker
                        let mut broadcast_msg_subscriber = broadcast_msg_sender.subscribe();
                        // for harvesting disconnected clients
                        let mut client_disconnect_sender_clone = client_disconnect_sender.clone();

                        // Spawn a task for handling a new client
                        task::spawn(async move {
                            // TODO: Refactor for handling clients stream
                            let res = handle_client(new_client, client_msg_sender_clone, &mut broadcast_msg_subscriber).await;
                            client_disconnect_sender_clone.send((new_client, broadcast_msg_subscriber))
                                .await
                                .expect("chatroom broker should be connected");
                            if let Err(e) = res {
                                // TODO: Log/Trace errors
                                eprintln!("{e}");
                                Err(e)
                            } else {
                                Ok(())
                            }
                        });
                    }
                    None => break,
                }
            },
            default => unreachable!()
        };
        todo!()
    }

    todo!()
}

async fn handle_client(
    mut client: Client,
    mut msg_sender: AsyncStdSender<MessageEvent>,
    msg_subscriber: &mut TokioBroadcastReceiver<MessageEvent>
) -> Result<(), ServerError> {
    let client_stream = client.stream.take().expect("client stream should not be None");
    // split the client stream into read/write tasks
    let (client_reader, client_writer) = (&client_stream, &client_stream);

    // For synchronizing with the writer task
    let (_shutdown_sender, shutdown_receiver) = unbounded::<Null>();

    // spawn a new writing task for this client
    // TODO: log errors
    let _ = task::spawn(client_writer_task(client.id, client_writer, msg_subscriber, shutdown_receiver));

    // Attempt to read new messages from the client
    // TODO: implement parsing of message events from client reader
    let mut client_reader = client_reader;

    todo!()
}

async fn client_writer_task(
    client_id: Uuid,
    client_writer: &net::TcpStream,
    msg_subscriber: &mut TokioBroadcastReceiver<MessageEvent>,
    shutdown_receiver: AsyncStdReceiver<Null>
) -> Result<(), ServerError> {
    todo!()
}