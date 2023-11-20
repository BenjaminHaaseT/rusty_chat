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
    chatroom_shutdown: AsyncStdSender<NullEnum>,
    channel_buf_size: usize
) -> Result<(), ServerError> {
    // Ensure `client_receiver` is fused
    let mut client_receiver = client_receiver.fuse();

    // Channel for receiving new messages from client writer tasks
    let (client_msg_sender, client_msg_receiver) = bounded::<Message>(channel_buf_size);

    // Channel for sending received messages from client readers and broadcasting to client writers
    // `broadcast_msg_receiver` will be cloned and passed to the writing task for each client
    let (broadcast_msg_sender, broadcast_msg_receiver) = broadcast::channel::<Message>(channel_buf_size);

    // Channel for receiving harvesting disconnected clients
    let (client_disconnect_sender, client_disconnect_receiver) = unbounded::<(Client, TokioBroadcastReceiver<Message>)>();
    let mut client_disconnect_receiver = client_disconnect_receiver.fuse();

    // Hashmap for keeping track of all clients
    // let mut clients: HashMap<Uuid, Client> = HashMap::new();
    let mut clients: HashSet<Uuid> = HashSet::new();

    loop {
        // We first need to check for disconnecting clients, joining clients or received messages
        let msg = select! {
            new_client = client_receiver.next().fuse() => {
                match new_client {
                    Some(mut new_client) => {
                        // add client to set of clients
                        clients.insert(new_client.id);
                        // for sending messages from a client to the chatroom broker
                        let client_msg_sender_clone = client_msg_sender.clone();
                        // for subscribing to the messages broadcast by the chatroom broker
                        let mut broadcast_msg_subscriber = broadcast_msg_sender.subscribe();
                        // for harvesting disconnected clients
                        let mut client_disconnect_sender_clone = client_disconnect_sender.clone();

                        // Spawn a task for handling a new client
                        task::spawn(async move {
                            // TODO: Refactor for handling clients stream
                            let res = handle_new_client(new_client.stream.take().unwrap(), client_msg_sender_clone, &mut broadcast_msg_subscriber).await;
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

async fn handle_new_client(
    client_stream: net::TcpStream,
    mut msg_sender: AsyncStdSender<Message>,
    msg_subscriber: &mut TokioBroadcastReceiver<Message>
) -> Result<(), ServerError> {
    todo!()
}