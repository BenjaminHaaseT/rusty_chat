//! Contains the handlers for event handling done by the broker task
//!


use std::collections::HashMap;
use std::sync::Arc;
use async_std::channel::{Sender as AsyncStdSender, Receiver as AsyncStdReceiver};
use async_std::task;
use async_std::channel;
use tokio::sync::broadcast::{self, Sender as TokioBroadcastSender};
use uuid::Uuid;
use tracing::{info, error, debug, instrument};
use rusty_chat::Chatroom;

use crate::{Client, Event, Response, Null};
use crate::ServerError;
use super::{create_lobby_state, chatroom_broker};


pub mod prelude {
    pub use super::*;
}

pub async fn handle_lobby_event(peer_id: Uuid, clients: &mut HashMap<Uuid, Client>, chatroom_brokers: &HashMap<Uuid, Chatroom>) -> Result<(), ServerError> {
    info!(peer_id = ?peer_id, "Client {:?} has requested `Lobby`", peer_id);
    let mut client = clients.get_mut(&peer_id)
        .ok_or(ServerError::StateError(format!("no client with id {} contained in client map", peer_id)))?;
    let response = Response::Lobby {lobby_state: create_lobby_state(&chatroom_brokers)};
    client.main_broker_write_task_sender
        .send(response)
        .await
        .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
    Ok(())
}

pub async fn handle_read_sync_event(peer_id: Uuid, clients: &mut HashMap<Uuid, Client>) -> Result<(), ServerError> {
    info!(peer_id = ?peer_id, "Client {:?} has confirmed read task is synced", peer_id);
    let mut client = clients.get_mut(&peer_id)
        .ok_or(ServerError::StateError(format!("no client with id {} contained in client map", peer_id)))?;
    // Ensure client has username set, sub_broker_id set as well
    if client.username.is_none() {
        return Err(ServerError::IllegalEvent(format!("client with id {} sent 'ReadSync' event without having username set", peer_id)));
    }
    if client.chatroom_broker_id.is_none() {
        return Err(ServerError::IllegalEvent(format!("client with id {} sent 'ReadSync' event without having chatroom_sub_broker_id set", peer_id)));
    }
    let response = Response::ReadSync;
    client.main_broker_write_task_sender
        .send(response)
        .await
        .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
    Ok(())
}

pub async fn handle_create_event(
    peer_id: Uuid,
    chatroom_name: String,
    clients: &mut HashMap<Uuid, Client>,
    chatroom_brokers: &mut HashMap<Uuid, Chatroom>,
    chatroom_name_to_id: &mut HashMap<Arc<String>, Uuid>,
    client_exit_sub_broker_sender: &AsyncStdSender<Event>,
    disconnected_sub_broker_sender: &AsyncStdSender<(Uuid, AsyncStdReceiver<Event>, AsyncStdSender<Event>, TokioBroadcastSender<Response>)>
) -> Result<(), ServerError> {
    info!(peer_id = ?peer_id, chatroom_name, "Client {:?} has requested `Create`", peer_id);

    // Get client reference first, ensure client is a valid connected client
    let mut client = clients.get_mut(&peer_id)
        .ok_or(ServerError::StateError(format!("no client with id {:?} contained in client map", peer_id)))?;

    // Ensure two invariants about state of client, 1) client has username set and 2)
    // client has no existing chatroom broker id set, client should be in neither of these states,
    // therefore if the client is, the program should terminate
    if client.username.is_none() {
        return Err(ServerError::IllegalEvent(format!("client with id {} attempted to create a new chatroom without having a valid username set", client.id)));
    }
    if client.chatroom_broker_id.is_some() {
        return Err(ServerError::IllegalEvent(format!("client with id {} already has field 'chatroom_broker_id' set", client.id)));
    }

    // Check if chatroom_name is already in use, if so new chatroom cannot be created
    let chatroom_name_clone = chatroom_name.clone();
    let chatroom_name = Arc::new(chatroom_name);

    if chatroom_name_to_id.contains_key(&chatroom_name) {
        info!(peer_id = ?peer_id, chatroom_name = ?chatroom_name, "Client {:?} attempted to create chatroom that was already created", peer_id);
        let lobby_state = create_lobby_state(&chatroom_brokers);
        client.main_broker_write_task_sender.send(
            Response::ChatroomAlreadyExists {
                chatroom_name: chatroom_name_clone,
                lobby_state,
            })
            .await
            .map_err(|_e| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
    } else {
        info!(peer_id = ?peer_id, chatroom_name = ?chatroom_name, "Creating new chatroom from client {} `Create` request", peer_id);
        // Otherwise we can create the chatroom
        let id = Uuid::new_v4();

        // Channel for client subscriptions i.e from chatroom sub-broker to client write tasks
        // TODO: Set capacity has a parameter
        let (mut broadcast_sender, broadcast_receiver) = broadcast::channel::<Response>(100);

        // Channel for client sending i.e from client read tasks to chatroom sub-broker
        // Needs to be saved for whenever a new client wishes to join this chatroom,
        // the sender can be cloned and used by the client
        // TODO: add capacity to avoid overflow of messages received
        let (client_sender, mut client_receiver) = channel::unbounded::<Event>();

        // Channel for synchronizing shutdown signal with main broker, chatroom gets receiving end
        // so it can be shutdown by the main broker
        let (shutdown_sender, shutdown_receiver) = channel::unbounded::<Null>();

        // Clone channel for exiting clients from the chatroom
        let mut client_exit_clone = client_exit_sub_broker_sender.clone();

        let chatroom = Chatroom {
            id,
            name: chatroom_name.clone(),
            client_subscriber: broadcast_sender.clone(),
            client_read_sender: client_sender.clone(),
            shutdown: Some(shutdown_sender),
            capacity: 1000,
            num_clients: 1
        };

        // Insert into maps
        chatroom_brokers.insert(id, chatroom);
        chatroom_name_to_id.insert(chatroom_name.clone(), id);

        // Clone channel sender for moving into a new task, for sending back channel connections when chatroom is finished
        let mut disconnection_sender_clone = disconnected_sub_broker_sender.clone();

        // Clone channel for subscriptions so chatroom can send responses to all subscribers
        let mut broadcast_sender_clone = broadcast_sender.clone();

        info!(chatroom_id = ?id, chatroom_name = ?chatroom_name.clone(), "Spawning chatroom: {} task", chatroom_name);
        // Spawn chatroom broker task
        let _chatroom_handle = task::spawn(async move {
            let res = chatroom_broker(id,&mut client_receiver, &mut client_exit_clone, &mut broadcast_sender_clone, shutdown_receiver).await;
            if let Err(ref e) = res {
                error!(error = ?e, chatrooom_id = ?id, "Error from chatroom sub-broker task {}", id);
            }
            disconnection_sender_clone.send((id, client_receiver, client_exit_clone, broadcast_sender_clone))
                .await
                .map_err(|_| ServerError::ChannelSendError(format!("chatroom-sub-broker {} unable to send disconnection event to main broker", id)))?;
            info!(chatroom_id = ?id, "Chatroom {} successfully sent shutdown event to main broker", id);
            res
        });

        // Update client's broker_id
        debug!(peer_id = ?peer_id, "Setting chatroom_broker_id for client {}", peer_id);
        client.chatroom_broker_id = Some(id);

        // Send the sending half of chatroom-broker-to-client-read channel to the client's read task
        client.new_chatroom_connection_read_sender.send(client_sender)
            .await
            .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} handle connection task a response", peer_id)))?;

        let response = Response::ChatroomCreated {chatroom_name: chatroom_name_clone};

        // Send client's write task a new subscription for receiving responses from the chatroom broker task
        client.new_chatroom_connection_write_sender.send((broadcast_sender.subscribe(), response))
            .await
            .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
        info!(peer_id = ?peer_id, "Sent broadcast subscription and response to client {} write task", peer_id);
    }
    Ok(())
}

pub async fn handle_join_event(
    peer_id: Uuid,
    chatroom_name: String,
    clients: &mut HashMap<Uuid, Client>,
    chatroom_brokers: &mut HashMap<Uuid, Chatroom>,
    chatroom_name_to_id: &mut HashMap<Arc<String>, Uuid>
) -> Result<(), ServerError> {
    info!(peer_id = ?peer_id, chatroom_name, "Client {} has requested to join chatroom {}", peer_id, chatroom_name);
    let mut client = clients.
        get_mut(&peer_id)
        .ok_or(ServerError::StateError(format!("client with id {} not contained in map", peer_id)))?;

    // Ensure same invariants for Event::Create handler also hold i.e.
    // client has set username and does not have broker id set
    if client.username.is_none() {
        return Err(ServerError::IllegalEvent(format!("client with id {} attempted to join a chatroom without having username set", peer_id)));
    }
    if client.chatroom_broker_id.is_some() {
        return Err(ServerError::IllegalEvent(format!("client with id {} attempted to join a chatroom while already inside a chatroom", peer_id)));
    }

    // Check that a chatroom with this name exists
    if let Some(chatroom_id) = chatroom_name_to_id.get(&chatroom_name) {
        // We need a shared reference here since we may need to take another shared
        // reference from chatroom_brokers
        let chatroom = chatroom_brokers
            .get(&chatroom_id)
            .ok_or(ServerError::StateError(format!("chatroom with id {} and name {} should exist in chatroom map", chatroom_id, chatroom_name)))?;

        if chatroom.capacity == chatroom.num_clients {
            info!(peer_id = ?peer_id, chatroom_id = ?chatroom_id, chatroom_name, "Client {} unable to join chatroom, chatroom is full", peer_id);
            let lobby_state = create_lobby_state(&chatroom_brokers);
            let response = Response::ChatroomFull {chatroom_name, lobby_state};
            client.main_broker_write_task_sender.send(response)
                .await
                .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
        } else {
            debug!("Client {} may join chatroom", peer_id);
            // Get mutable reference here to update state of chatroom
            let mut chatroom = chatroom_brokers
                .get_mut(&chatroom_id)
                .ok_or(ServerError::StateError(format!("chatroom with id {} and name {} should exist in chatroom map", chatroom_id, chatroom_name)))?;

            chatroom.num_clients += 1;

            // For sending to client's write/read tasks respectively
            let chatroom_subscriber = chatroom.client_subscriber.subscribe();
            let chatroom_sender = chatroom.client_read_sender.clone();

            // Update state of client
            debug!(peer_id = ?peer_id, "Setting chatroom_broker_id for client {}", peer_id);
            client.chatroom_broker_id = Some(*chatroom_id);

            // Send response to client's read task first
            client.new_chatroom_connection_read_sender
                .send(chatroom_sender)
                .await
                .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} handle connection task a response", peer_id)))?;

            // Send subscriber, response to client's write task
            let response = Response::Subscribed {chatroom_name};
            client.new_chatroom_connection_write_sender
                .send((chatroom_subscriber, response))
                .await
                .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
        }
    } else {
        info!(peer_id = ?peer_id, chatroom_name, "Client {} unable to join chatroom, chatroom does not exist", peer_id);
        let lobby_state = create_lobby_state(&chatroom_brokers);
        let response = Response::ChatroomDoesNotExist {chatroom_name, lobby_state};
        client.main_broker_write_task_sender.send(response)
            .await
            .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
    }
    Ok(())
}