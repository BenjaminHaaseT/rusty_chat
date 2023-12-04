use std::fmt::Debug;
use std::sync::Arc;
use std::collections::{HashMap, HashSet};

// use std::
use std::pin::Pin;
use std::task::{Context, Poll};


use async_std::io::{Read, ReadExt, Write, WriteExt};
use async_std::channel::{self, Sender, Receiver};
use async_std::net::ToSocketAddrs;
use async_std::net::{TcpListener, TcpStream};
use async_std::task;
use tokio::sync::broadcast;

use futures::{
    future::{Future, FutureExt,  FusedFuture, Fuse},
    stream::{Stream, StreamExt, FusedStream},
    select,
};

use tokio_stream::wrappers::BroadcastStream;

use uuid::Uuid;

use crate::server_error::ServerError;
use rusty_chat::{*, Event, Null};


mod chatroom_task;
mod server_error;

async fn accept_loop(server_addrs: impl ToSocketAddrs + Clone + Debug, channel_buf_size: usize) -> Result<(), ServerError> {
    // TODO: add logging/tracing
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

    // send a new client event to the main broker, should not be disconnected
    main_broker_sender.send(event)
        .await
        .expect("broker should be connected");

    // for sending message events when the client joins a new chatroom.
    let mut chatroom_sender: Option<AsyncStdSender<Event>> = None;

    // fuse the readers for selecting
    let mut client_reader = client_reader;
    let mut chatroom_broker_receiver = chatroom_broker_receiver.fuse();

    loop {
        // We need to listen to 2 potential events, we receive input from the client's stream
        // or we receive a sending part of a chatroom broker task
        let frame = select! {
            // read input from client
            res = Frame::try_parse(&mut client_reader).fuse() => {
                match res {
                    Ok(frame) => frame,
                    Err(e) => return Err(ServerError::ConnectionFailed),
                }
            },
            chatroom_sender_opt = chatroom_broker_receiver.next().fuse() => {
                match chatroom_sender_opt {
                    Some(chatroom_channel) => {
                        chatroom_sender = Some(chatroom_channel);
                        continue;
                    },
                    None => {
                        eprintln!("received None from 'chatroom_broker_receiver'");
                        return Err(ServerError::ConnectionFailed);
                    }
                }
            },
            default => unreachable!("should not happen")
        };

        // If we have a chatroom sender channel, we assume all parsed events are being sent to the
        // current chatroom, otherwise we send events to main broker
        if let Some(chat_sender) = chatroom_sender.take() {
            match frame {
                Frame::Quit => {
                    // take chat_sender without replacing, since we are sending a quit message
                    // client no longer desires to be in this chatroom
                    chat_sender.send(Event::Quit {peer_id})
                        .await
                        .expect("chatroom broker should be connected");
                }
                Frame::Message {message} => {
                    chat_sender.send(Event::Message {message, peer_id})
                        .await
                        .expect("chatroom broker should be connected");
                    // place chat_sender back into chatroom_sender
                    chatroom_sender = Some(chat_sender);
                }
                _ => panic!("invalid frame sent by client, client may only send 'Quit' and 'Message' variants when connected to a chatroom broker"),
            }

        } else {
            match frame {
                Frame::Quit =>  {
                    main_broker_sender.send(Event::Quit { peer_id })
                        .await
                        .expect("main broker should be connected");
                    // We are quiting the program all together at this point
                    break;
                },
                Frame::Create {chatroom_name} => {
                    main_broker_sender.send( Event::Create {chatroom_name, peer_id})
                        .await
                        .expect("main broker should be connected");
                },
                Frame::Join {chatroom_name} => {
                    main_broker_sender.send(Event::Join {chatroom_name, peer_id})
                        .await
                        .expect("main broker should be connected");
                },
                Frame::Username {new_username} => {
                    main_broker_sender.send(Event::Username {new_username, peer_id})
                        .await
                        .expect("main broker should be connected");
                },
                _ => panic!("invalid frame sent by client, cannot send messages until connection with chatroom broker is established"),
            }
        }
    }

    Ok(())
}

// TODO: need to find a way to handle the exit from a chatroom
async fn client_write_loop(
    client_id: Uuid,
    client_stream: Arc<TcpStream>,
    main_broker_receiver: AsyncStdReceiver<Response>,
    chatroom_broker_receiver: AsyncStdReceiver<(TokioBroadcastReceiver<Response>, Response)>,
    shutdown: AsyncStdReceiver<Null>,
) -> Result<(), ServerError> {
    println!("inside client write loop");
    // Shadow client stream so it can be written to
    let mut client_stream = &*client_stream;

    // The broker for the chatroom, yet to be set, will yield poll pending until it is reset
    let (_, chat_receiver) = tokio::sync::broadcast::channel::<Response>(100);
    let mut chat_receiver = BroadcastStream::new(chat_receiver).fuse();

    // Fuse main broker receiver and shutdown streams
    let mut main_broker_receiver = main_broker_receiver.fuse();
    let mut chatroom_broker_receiver = chatroom_broker_receiver.fuse();
    let mut shutdown = shutdown.fuse();

    loop {
        // Select over possible receiving options
        let response: Response = select! {

            // Check for a disconnection event
            null = shutdown.next().fuse() => {
                println!("Client write loop shutting down");
                match null {
                    Some(null) => match null {},
                    _ => break,
                }
            },

            // Check for a response from main broker
            resp = main_broker_receiver.next().fuse() => {
                match resp {
                    Some(resp) => resp,
                    None => {
                        eprintln!("received none from main broker receiver");
                        return Err(ServerError::ConnectionFailed);
                    }
                }
            }

            // Check for a new chatroom broker receiver
            subscription = chatroom_broker_receiver.next().fuse() => {
                println!("attempting to subscribe to chatroom");
                match subscription {
                    Some(resp) => {
                        let (new_chat_receiver, resp) = resp;
                        // assert that resp is the Subscribed or ChatroomCreated variant
                        // it is a panic condition if the response sent on this channel from the
                        // main broker is neither of these variants
                        assert!(
                            resp.is_subscribed() || resp.is_chatroom_created(),
                            "received invalid response from main chatroom broker on 'chatroom_broker_receiver'"
                        );
                        // Update chat_receiver so client can receive new messages from chatroom
                        chat_receiver = BroadcastStream::new(new_chat_receiver).fuse();
                        resp
                    },
                    None => {
                        eprintln!("received None from chatroom_broker_receiver");
                        return Err(ServerError::ConnectionFailed);
                    }
                }
            },

            // Check for a response from the chatroom broker
            resp = chat_receiver.next().fuse() => {
                match resp {
                    Some(resp_res) => {
                        match resp_res {
                            Ok(resp) => {
                                // Check if we have received an exit response from the chatroom broker,
                                // if so we need to update chat_receiver
                                if resp.is_exit_chatroom() {
                                    let (_, new_chat_receiver) = tokio::sync::broadcast::channel::<Response>(100);
                                    let mut new_chat_receiver = BroadcastStream::new(new_chat_receiver).fuse();
                                    chat_receiver = new_chat_receiver;
                                }
                                resp
                            }
                            Err(_) => {
                                eprintln!("error parsing response from 'chat_receiver'");
                                return Err(ServerError::ConnectionFailed);
                            }
                        }
                    },
                    None => {
                        eprintln!("received None from chatroom_broker_receiver");
                        return Err(ServerError::ConnectionFailed);
                    }
                }
            }
        };

        // Write response back to client's stream
        client_stream.write_all(response.as_bytes().as_slice())
            .await
            .map_err(|_| ServerError::ConnectionFailed)?;
    }

    Ok(())
}

#[derive(Debug)]
struct Client {
    id: Uuid,
    username: Option<Arc<String>>,
    main_broker_write_task_sender: AsyncStdSender<Response>,
    new_chatroom_connection_read_sender: AsyncStdSender<AsyncStdSender<Event>>,
    new_chatroom_connection_write_sender: AsyncStdSender<(TokioBroadcastReceiver<Response>, Response)>,
    chatroom_broker_id: Option<Uuid>,
}

#[derive(Debug)]
pub struct Chatroom {
    id: Uuid,
    name: Arc<String>,
    client_subscriber: TokioBroadcastSender<Response>,
    client_read_sender: AsyncStdSender<Event>,
    shutdown: Option<AsyncStdSender<Null>>,
    capacity: usize,
    num_clients: usize,
}

impl Chatroom {
    fn serialize_name_length(&self, tag: &mut [u8; 12]) {
        let name_len = self.name.len();
        for i in 0..4 {
            tag[i] ^= ((name_len >> (i * 8)) & 0xff) as u8;
        }
    }

    fn serialize_capacity(&self, tag: &mut [u8; 12]) {
        for i in 4..8 {
            tag[i] ^= ((self.capacity >> ((i % 4) * 8)) & 0xff) as u8;
        }
    }

    fn serialize_num_clients(&self, tag: &mut [u8; 12]) {
        for i in 8..12 {
            tag[i] ^= ((self.num_clients >> ((i % 4) * 8)) & 0xff) as u8;
        }
    }
}

#[derive(Debug)]
pub struct ChatroomEncodeTag([u8; 12]);

impl SerializationTag for ChatroomEncodeTag {}

#[derive(Debug)]
pub struct ChatroomDecodeTag(u32, u32, u32);

impl DeserializationTag for ChatroomDecodeTag {}

impl SerAsBytes for Chatroom {
    type Tag = ChatroomEncodeTag;

    fn serialize(&self) -> Self::Tag {
        let mut tag = [0u8; 12];
        self.serialize_name_length(&mut tag);
        self.serialize_capacity(&mut tag);
        self.serialize_num_clients(&mut tag);
        ChatroomEncodeTag(tag)
    }
}

impl DeserAsBytes for Chatroom {
    type TvlTag = ChatroomDecodeTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag {
        let inner = tag.0;
        let mut name_len = 0;
        for i in 0..4 {
            name_len ^= (inner[i] as u32) << (i * 8);
        }
        let mut capacity = 0;
        for i in 4..8 {
            capacity ^= (inner[i] as u32) << ((i % 4) * 8);
        }
        let mut num_clients = 0;
        for i in 8..12 {
            num_clients ^= (inner[i] as u32) << ((i % 4) * 8);
        }
        ChatroomDecodeTag(name_len, capacity, num_clients)
    }
}

impl AsBytes for Chatroom {
    fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = self.serialize().0.to_vec();
        bytes.extend_from_slice(self.name.as_bytes());
        bytes
    }
}

async fn broker(_event_sender: Sender<Event>, event_receiver: Receiver<Event>) -> Result<(), ServerError> {
    // For keeping track of current clients
    let mut clients: HashMap<Uuid, Client> = HashMap::new();
    let mut client_usernames: HashSet<Arc<String>> = HashSet::new();

    // Channel for harvesting disconnected clients
    let (
        client_disconnection_sender,
        client_disconnection_receiver
    ) = channel::unbounded::<(Uuid, AsyncStdReceiver<Response>, AsyncStdReceiver<TokioBroadcastReceiver<Response>>)>();

    // For keeping track of chatroom sub-broker tasks
    let mut chatroom_brokers: HashMap<Uuid, Chatroom> = HashMap::new();
    let mut chatroom_names : HashSet<Arc<String>> = HashSet::new();

    // Channel for communicating with chatroom-sub-broker tasks, used exclusively for an exiting client
    let (client_exit_sub_broker_sender, client_exit_sub_broker_receiver) = channel::unbounded::<Event>();

    // Channel for harvesting disconnected chatroom-sub-broker tasks
    let (disconnected_sub_broker_sender, disconnected_sub_broker_receiver) = channel::unbounded::<(Uuid, AsyncStdReceiver<Event>, TokioBroadcastSender<Response>)>();

    // Fuse receivers
    let mut client_event_receiver = event_receiver.fuse();
    let mut client_disconnection_receiver = client_disconnection_receiver.fuse();
    let mut client_exit_sub_broker_receiver = client_exit_sub_broker_receiver.fuse();
    let mut disconnected_sub_broker_receiver = disconnected_sub_broker_receiver.fuse();

    loop {

        // We can receive events from disconnecting clients, connected clients, sub-broker tasks
        // or disconnecting sub-broker tasks
        let event = select! {

            // Listen for disconnecting clients
            disconnection = client_disconnection_receiver.next().fuse() => {

                // Harvest the data sent by the client's disconnecting procedure and remove the client
                let (peer_id, _client_response_channel, _client_chat_channel) = disconnection
                        .ok_or(ServerError::ChannelReceiveError(String::from("'client_disconnection_receiver' should send harvestable data")))?;

                let mut removed_client = clients.remove(&peer_id).ok_or(ServerError::StateError(format!("client with peer id {} should exist in client map", peer_id)))?;

                // Check if client had set their username
                if let Some(username) = removed_client.username.as_ref() {
                    // Attempt to remove clients username from name set
                    if !client_usernames.remove(username) {
                        return Err(ServerError::StateError(format!("client username {} should exist in set", username)));
                    }
                }

                // Check if the client disconnected while inside a chatroom or not, if so ensure
                // we update the client count for the specified chatroom
                if let Some(chatroom_id) = removed_client.chatroom_broker_id.take() {

                    // Take chatroom broker for the purpose of updating and potentially removing
                    let mut chatroom_broker = chatroom_brokers
                        .remove(&chatroom_id)
                        .ok_or(ServerError::StateError(format!("chatroom sub-broker with id {} should exist in map", chatroom_id)))?;

                    // Decrement client count for the broker
                    chatroom_broker.num_clients -= 1;

                    // Check if client count is zero, if so remove it from the name set and send a shutdown signal
                    // do not place broker back into map
                    if chatroom_broker.num_clients == 0 {
                        // Attempt to remove name from  name map
                        if !chatroom_names.remove(&chatroom_broker.name) {
                            return Err(ServerError::StateError(format!("chatroom sub-broker with name {} should exist in set", chatroom_broker.name)));
                        }
                        // Attempt to send shutdown signal
                        match chatroom_broker.shutdown.take() {
                            Some(shutdown) => {
                                drop(shutdown)
                            }
                            _ => return Err(ServerError::StateError(format!("chatroom sub-broker with id {} should have shutdown set to some", chatroom_id))),
                        }
                    } else {
                        // Otherwise put the broker back into broker's map with updated client count
                        chatroom_brokers.insert(chatroom_id, chatroom_broker);
                    }
                }

                // Continue listening for more events
                continue;
            },

            // Listen for disconnection chatroom-sub-broker tasks
            disconnection = disconnected_sub_broker_receiver.next().fuse() => {
                // Harvest the returned data and ensure that the chatroom broker has been successfully removed
                let (chatroom_id, _sub_broker_events, _sub_broker_messages) = disconnection.ok_or(ServerError::ChannelReceiveError(String::from("received 'None' from 'disconnected_sub_broker_receiver'")))?;
                if chatroom_brokers.contains_key(&chatroom_id) {
                    return Err(ServerError::StateError(format!("chatroom sub-broker with id {} should no longer exist in map", chatroom_id)));
                }

                // Continue listening for more events
                continue;
            }

            // Listen for events triggered by clients
            event = client_event_receiver.next().fuse() => {
                match event {
                    Some(event) => event,
                    None => break,
                }
            }

            // Listen for exiting clients from sub-brokers
            event = client_exit_sub_broker_receiver.next().fuse() => {
                let peer_id = event.map_or(
                    Err(ServerError::ChannelReceiveError(String::from("received 'None' from 'client_exit_sub_broker_receiver'"))),
                    |ev| -> Result<Uuid, ServerError> {
                        match ev {
                            Event::Quit {peer_id} => Ok(peer_id),
                            _ => Err(ServerError::ChannelReceiveError(String::from("received invalid 'Event' from 'client_exit_sub_broker_receiver'")))
                        }
                    }
                )?;

                // Ensure we have a client with peer_id
                let client = clients.get_mut(&peer_id).ok_or(ServerError::StateError(format!("client with id {} should exist in map", peer_id)))?;

                // Take chatroom broker id from client, since client is exiting
                let chatroom_broker_id = client.chatroom_broker_id
                        .take()
                        .ok_or(ServerError::StateError(format!("client with id {} should have not have 'chatroom_broker_id' set to 'None'", peer_id)))?;

                // Take chatroom broker from map, for updating its state
                let mut chatroom_broker = chatroom_brokers
                    .remove(&chatroom_broker_id)
                    .ok_or(ServerError::StateError(format!("chatroom sub-broker with id {} should exist in map", chatroom_broker_id)))?;

                // Decrement client count for the current broker
                chatroom_broker.num_clients -= 1;

                // Check if we need to initiate a shutdown for the current sub-broker
                if chatroom_broker.num_clients == 0 {
                    if !chatroom_names.remove(&chatroom_broker.name) {
                        return Err(ServerError::StateError(format!("chatroom sub-broker with name {} should exist in set", chatroom_broker.name)));
                    }
                    match chatroom_broker.shutdown.take() {
                        Some(shutdown) => drop(shutdown),
                        _ => return Err(ServerError::StateError(format!("chatroom sub-broker with id {} should have shutdown set to 'Some'", chatroom_broker_id))),
                    }
                } else {
                    // Otherwise re-insert broker back into map
                    chatroom_brokers.insert(chatroom_broker_id, chatroom_broker);
                }

                // Continue listening for new events
                continue;
            }
        };

        // Now attempt to parse event and send a response
        match event {
            Event::Quit {peer_id} => {
                println!("Logging that client {:?} has quit", peer_id);
            }
            Event::Create {peer_id, chatroom_name} => {
                // Get client reference first, ensure client is a valid connected client
                let mut client = clients.get_mut(&peer_id)
                    .ok_or(ServerError::StateError(format!("no client with id {:?} contained in map", peer_id)))?;

                // Ensure two invariants about state of client, 1) client has username set and 2)
                // client has no existing chatroom broker id set, neither of these states should occur,
                // therefore if they do occur, the program should terminate
                if client.username.is_none() {
                    return Err(ServerError::IllegalEvent(format!("client with id {} attempted to create a new chatroom without having a valid username set", client.id)));
                }
                if client.chatroom_broker_id.is_some() {
                    return Err(ServerError::IllegalEvent(format!("client with id {} already has field 'chatroom_broker_id' set", client.id)));
                }

                // Check if chatroom_name is already in use, if so new chatroom cannot be created
                let chatroom_name_clone = chatroom_name.clone();
                let chatroom_name = Arc::new(chatroom_name);

                if chatroom_names.contains(&chatroom_name) {
                    let lobby_state = create_lobby_state(&mut chatroom_brokers);
                    client.main_broker_write_task_sender.send(
                        Response::ChatroomAlreadyExists {
                            chatroom_name: chatroom_name_clone,
                            lobby_state,
                        })
                        .await
                        .map_err(|_e| ServerError::ConnectionFailed)?;
                } else {
                    // Otherwise we can create the chatroom
                    let id = Uuid::new_v4();

                    // Channel for client subscriptions i.e from chatroom broker to client write tasks
                    // TODO: Set capacity has a parameter
                    let (mut broadcast_sender, broadcast_receiver) = broadcast::channel::<Response>(100);

                    // Channel for client sending i.e from client read tasks to chatroom broker
                    // TODO: add capacity to avoid overflow of messages received
                    let (client_sender, mut client_receiver) = channel::unbounded::<Event>();
                    let (shutdown_sender, shutdown_receiver) = channel::unbounded::<Null>();

                    let chatroom = Chatroom {
                        id,
                        name: chatroom_name.clone(),
                        client_subscriber: broadcast_sender.clone(),
                        client_read_sender: client_sender.clone(),
                        shutdown: Some(shutdown_sender),
                        capacity: 1000,
                        num_clients: 1
                    };

                    // Ensure that the lifetime of chatroom name does not exceed lifetime of chatroom
                    chatroom_brokers.insert(id, chatroom);
                    chatroom_names.insert(chatroom_name.clone());

                    // Clone channel senders for moving into a new task
                    let mut disconnection_sender_clone = disconnected_sub_broker_sender.clone();
                    let mut broadcast_sender_clone = broadcast_sender.clone();

                    // Spawn chatroom broker task
                    let chatroom_handle = task::spawn(async move {
                        let res = chatroom_broker(&mut client_receiver, &mut broadcast_sender_clone, shutdown_receiver).await;
                        if let Err(ref e) = res {
                            // TODO: add logging/tracing
                            eprintln!("error occurred inside chatroom broker task with id {}", id);
                            eprintln!("{e}");
                        }
                        disconnection_sender_clone.send((id, client_receiver, broadcast_sender_clone))
                            .await
                            .map_err(|_| ServerError::ConnectionFailed)?;
                        res
                    });

                    // Update client's broker_id
                    client.chatroom_broker_id = Some(id);

                    // Send the sending half the client-read-task to chatroom broker task channel to the client
                    client.new_chatroom_connection_read_sender.send(client_sender)
                        .await
                        .map_err(|_| ServerError::ConnectionFailed)?;

                    let response = Response::ChatroomCreated {chatroom_name: chatroom_name_clone};

                    // Send client's write task a new subscription for receiving responses from the chatroom broker task
                    client.new_chatroom_connection_write_sender.send((broadcast_sender.subscribe(), response))
                        .await
                        .map_err(|_| ServerError::ConnectionFailed)?;
                }
            }
            _ => todo!()
        }
    }

    todo!()
}

async fn chatroom_broker(
    events: &mut AsyncStdReceiver<Event>,
    broadcast_sender: &mut TokioBroadcastSender<Response>,
    shutdown_receiver: AsyncStdReceiver<Null>,
    // disconnection_sender: AsyncStdSender<(Uuid, AsyncStdReceiver<Event>, TokioBroadcastSender<Response>)>
) -> Result<(), ServerError> {

    todo!()
}

fn create_lobby_state(chatroom_brokers: &HashMap<Uuid, Chatroom>) -> Vec<u8> {
    let mut lobby_state = vec![];
    for (_, chatroom) in chatroom_brokers {
        lobby_state.append(&mut chatroom.as_bytes());
    }
    lobby_state
}


fn main() {todo!()}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_chatroom_serialize() {
        let id = Uuid::new_v4();
        let (broadcast_sender, _) = broadcast::channel::<Response>(1);
        let (chat_sender, _) = channel::unbounded::<Event>();

        let chatroom = Chatroom {
            id,
            name: String::from("Test chatroom 666"),
            client_subscriber:  broadcast_sender,
            client_read_sender: chat_sender,
            shutdown: None,
            capacity: 4798,
            num_clients: 2353,
        };

        let tag = chatroom.serialize().0;

        println!("{:?}", tag);
        assert_eq!(tag, [17, 0, 0, 0, 190, 18, 0, 0, 49, 9, 0, 0])
    }

    #[test]
    fn test_chatroom_deserialize() {
        let id = Uuid::new_v4();
        let (broadcast_sender, _) = broadcast::channel::<Response>(1);
        let (chat_sender, _) = channel::unbounded::<Event>();
        let name = String::from("Test chatroom 666");

        let chatroom = Chatroom {
            id,
            name: name.clone(),
            client_subscriber:  broadcast_sender,
            client_read_sender: chat_sender,
            shutdown: None,
            capacity: 4798,
            num_clients: 2353,
        };

        let tag = chatroom.serialize();
        let decode_tag = Chatroom::deserialize(&tag);

        println!("{:?}", decode_tag);
        assert_eq!(decode_tag.0, name.len() as u32);
        assert_eq!(decode_tag.1, 4798);
        assert_eq!(decode_tag.2, 2353);
    }

    #[test]
    fn test_chatroom_as_bytes() {
        let id = Uuid::new_v4();
        let (broadcast_sender, _) = broadcast::channel::<Response>(1);
        let (chat_sender, _) = channel::unbounded::<Event>();
        let name = String::from("Test chatroom 666");

        let chatroom = Chatroom {
            id,
            name: name.clone(),
            client_subscriber:  broadcast_sender,
            client_read_sender: chat_sender,
            shutdown: None,
            capacity: 4798,
            num_clients: 2353,
        };

        let chatroom_bytes = chatroom.as_bytes();
        println!("{:?}", chatroom_bytes);

        let mut expected_bytes = chatroom.serialize().0.to_vec();
        expected_bytes.extend_from_slice(name.as_bytes());

        assert_eq!(chatroom_bytes, expected_bytes);
    }

}