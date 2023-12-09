use std::fmt::Debug;
use std::sync::Arc;
use std::collections::{HashMap, HashSet};

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
    println!("listening at {:?}...", server_addrs);

    let mut listener = TcpListener::bind(server_addrs.clone())
        .await
        .map_err(|_| ServerError::ConnectionFailed(format!("unable to bind listener at address {:?}", server_addrs)))?;

    let (broker_sender, broker_receiver) = channel::bounded::<Event>(channel_buf_size);

    // spawn broker task
    task::spawn(broker(broker_receiver));

    while let Some(stream) = listener.incoming().next().await {
        // TODO: log error in this case do not return
        let stream = stream.map_err(|e| ServerError::ConnectionFailed(format!("accept loop received an error: {:?}", e)))?;
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

    // channel for synchronizing graceful shutdown with the client's writer task
    let (_client_shutdown_sender, client_shutdown_receiver) = channel::unbounded::<Null>();

    // channel for receiving new chatroom-sub-broker connections from the main broker
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
        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;

    // for sending message events when the client joins a new chatroom. Only set to 'Some'
    // variant when the client is inside a chatroom
    let mut chatroom_sender: Option<AsyncStdSender<Event>> = None;

    // Shadow client_reader to it can be read from
    let mut client_reader = client_reader;
    // Fuse so it can be selected over
    let mut chatroom_broker_receiver = chatroom_broker_receiver.fuse();

    loop {
        // We need to listen to 2 potential events, we receive input from the client's stream
        // or we receive a sending part of a chatroom broker task
        let frame = select! {
            // read input from client
            res = Frame::try_parse(&mut client_reader).fuse() => {
                match res {
                    Ok(frame) => frame,
                    Err(e) => return Err(ServerError::ParseFrame(format!("client {} unable to parse frame in handle connection task", peer_id))),
                }
            },
            chatroom_sender_opt = chatroom_broker_receiver.next().fuse() => {
                match chatroom_sender_opt {
                    Some(chatroom_channel) => {
                        chatroom_sender = Some(chatroom_channel);
                        // Send the main broker an event that this client is now able to start sending messages
                        main_broker_sender.send(Event::ReadSync {peer_id})
                            .await
                            .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                        continue;
                    },
                    None => {
                        eprintln!("received None from 'chatroom_broker_receiver'");
                        return Err(ServerError::ChannelReceiveError(format!("client {} handle connection task received invalid value from 'chatroom_broker_receiver'", peer_id)));
                    }
                }
            },
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
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                }
                Frame::Message {message} => {
                    chat_sender.send(Event::Message {message, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                    // place chat_sender back into chatroom_sender
                    chatroom_sender = Some(chat_sender);
                }
                _ => return Err(ServerError::IllegalFrame(format!("client {} handle connection task received an illegal frame: {:?}", peer_id, frame))),
            }

        } else {
            match frame {
                Frame::Quit =>  {
                    main_broker_sender.send(Event::Quit { peer_id })
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                    // We are quiting the program all together at this point
                    break;
                },
                Frame::Lobby => {
                    main_broker_sender.send(Event::Lobby {peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                }
                Frame::Create {chatroom_name} => {
                    main_broker_sender.send( Event::Create {chatroom_name, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                },
                Frame::Join {chatroom_name} => {
                    main_broker_sender.send(Event::Join {chatroom_name, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                },
                Frame::Username {new_username} => {
                    main_broker_sender.send(Event::Username {new_username, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                },
                _ => return Err(ServerError::IllegalFrame(format!("client {} handle connection task received an illegal frame: {:?}", peer_id, frame))),
            }
        }
    }

    Ok(())
}


async fn client_write_loop(
    client_id: Uuid,
    client_stream: Arc<TcpStream>,
    main_broker_receiver: &mut AsyncStdReceiver<Response>,
    chatroom_broker_receiver: &mut AsyncStdReceiver<(TokioBroadcastReceiver<Response>, Response)>,
    shutdown: AsyncStdReceiver<Null>,
) -> Result<(), ServerError> {
    println!("inside client write loop");
    // Shadow client stream so it can be written to
    let mut client_stream = &*client_stream;

    // The broker for the chatroom, yet to be set, will yield poll pending until it is reset
    let (mut dummy_chat_sender, chat_receiver) = tokio::sync::broadcast::channel::<Response>(100);
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
                    Some(resp) => {
                        if resp.is_exit_chatroom() {
                            // Create new dummy channel to replace old chatroom_broker channel
                            let (new_dummy_chat_sender, new_chat_receiver) = broadcast::channel::<Response>(100);
                            // Reset the chat_receiver channel
                            dummy_chat_sender = new_dummy_chat_sender;
                            chat_receiver = BroadcastStream::new(new_chat_receiver).fuse();
                        }
                        resp
                    },
                    None => {
                        eprintln!("received none from main broker receiver");
                        return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received 'None' from 'main_broker_receiver'", client_id)));
                    }
                }
            }

            // Check for a new chatroom broker receiver
            subscription = chatroom_broker_receiver.next().fuse() => {
                println!("attempting to subscribe to chatroom");
                match subscription {
                    Some(sub) => {
                        let (new_chat_receiver, resp) = sub;
                        // assert that resp is the Subscribed or ChatroomCreated variant
                        // it is a panic condition if the response sent on this channel from the
                        // main broker is neither of these variants

                        // assert!(
                        //     resp.is_subscribed() || resp.is_chatroom_created(),
                        //     "received invalid response from main chatroom broker on 'chatroom_broker_receiver'"
                        // );

                        // Ensure we received a valid response
                        if resp.is_subscribed() || resp.is_chatroom_created() {
                            // Update chat_receiver so client can receive new messages from chatroom
                            chat_receiver = BroadcastStream::new(new_chat_receiver).fuse();
                            resp
                        } else {
                            return Err(ServerError::IllegalResponse(format!("client {} received a {:?} response on 'chatroom_broker_receiver'", client_id, res)));
                        }
                    },
                    None => {
                        eprintln!("received None from chatroom_broker_receiver");
                        return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received 'None' from 'chatroom_broker_receiver'", client_id)));
                    }
                }
            },

            // Check for a response from the chatroom broker
            resp = chat_receiver.next().fuse() => {
                match resp {
                    Some(msg_resp) => {
                        let msg_resp = if let Ok(r) = msg_resp {
                            r
                        } else {
                            return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received an error from 'chat_receiver'", client_id)))
                        };
                        // Filter the response, check that we only receive messages and filter
                        // all messages not from the current client
                        match &msg_resp {
                            Response::Message {msg, peer_id} if *peer_id != client_id => msg_resp,
                            Response::Message { msg, peer_id } => continue,
                            _ => return Err(ServerError::IllegalResponse(format!("client {} write task received {:?} response from chatroom broker", client_id, msg_resp))),
                        }
                    },
                    None => {
                        eprintln!("received None from chatroom_broker_receiver");
                        return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received an 'None' from 'chat_receiver'", client_id)));
                    }
                }
            }
        };

        // Write response back to client's stream
        client_stream.write_all(response.as_bytes().as_slice())
            .await
            .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to write response to stream", client_id)))?;
    }

    Ok(())
}

/// Models a client connected to the chatroom.
#[derive(Debug)]
struct Client {
    /// The unique id of the client.
    id: Uuid,

    /// The username of the client, may or may not be set.
    username: Option<Arc<String>>,

    /// The sending half of the channel connecting the client's write task to the main broker task
    main_broker_write_task_sender: AsyncStdSender<Response>,

    /// The sending half of the channel connecting the client's read task to main broker task,
    /// used exclusively for the purpose of sending the client's read task a new chatroom-sub-broker connection
    /// Allows the client's read task to update where it sends 'Events'.
    new_chatroom_connection_read_sender: AsyncStdSender<AsyncStdSender<Event>>,

    /// The receiving half of the channel that connects the main broker to the client's write task
    /// use exclusively for sending the client's write task a subscription to a new chatroom. Allows
    /// the client's write task to update where it receives 'Responses' from,
    /// intended to be used exclusively for messages and quit responses.
    new_chatroom_connection_write_sender: AsyncStdSender<(TokioBroadcastReceiver<Response>, Response)>,

    /// The unique id of the chatroom-sub-broker that the client is engaged with, may or may not be set
    /// depending on whether or not the client is currently in a chatroom or not.
    chatroom_broker_id: Option<Uuid>,
}

/// Models a single chatroom that is hosted by the server.
#[derive(Debug)]
pub struct Chatroom {
    /// The unique id of the chatroom.
    id: Uuid,

    /// The name of the chatroom.
    name: Arc<String>,

    /// A sending half of the broadcast channel that the chatroom-sub-broker task
    /// will use to broadcast messages to all subscribing clients. Used for creating new subscriptions,
    /// whenever a new client wants to join.
    client_subscriber: TokioBroadcastSender<Response>,

    /// The sending half of the channel that can be cloned and sent to any client's read task,
    /// that way new client's can take a clone of the sending half of this channel and start sending events.
    client_read_sender: AsyncStdSender<Event>,

    /// The sending half of a channel used for synchronizing shutdown ie, dropping this channel (when it is 'Some')
    /// will initiate a graceful shutdown procedure for the current chatroom-sub-broker task. May or may not be set.
    shutdown: Option<AsyncStdSender<Null>>,

    /// The capacity of the chatroom ie number of clients it can serve.
    capacity: usize,

    /// The current number of clients connected with the chatroom.
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

fn create_lobby_state(chatroom_brokers: &HashMap<Uuid, Chatroom>) -> Vec<u8> {
    let mut lobby_state = vec![];
    for (_, chatroom) in chatroom_brokers {
        lobby_state.append(&mut chatroom.as_bytes());
    }
    lobby_state
}

async fn broker(event_receiver: Receiver<Event>) -> Result<(), ServerError> {
    // For keeping track of current clients
    let mut clients: HashMap<Uuid, Client> = HashMap::new();
    // For keeping track of client usernames, allows for quicker searching.
    let mut client_usernames: HashSet<Arc<String>> = HashSet::new();

    // Channel for harvesting disconnected clients, allows us to guarantee that any client in clients
    // does not outlive any of the values harvested from this channel
    let (
        client_disconnection_sender,
        client_disconnection_receiver
    ) = channel::unbounded::<(Uuid, AsyncStdReceiver<Response>, AsyncStdReceiver<(TokioBroadcastReceiver<Response>, Response)>)>();

    // For keeping track of chatroom sub-broker tasks
    let mut chatroom_brokers: HashMap<Uuid, Chatroom> = HashMap::new();
    let mut chatroom_name_to_id : HashMap<Arc<String>, Uuid> = HashMap::new();

    // Channel for communicating with chatroom-sub-broker tasks, used exclusively for an exiting client
    let (client_exit_sub_broker_sender, client_exit_sub_broker_receiver) = channel::unbounded::<Event>();

    // Channel for harvesting disconnected chatroom-sub-broker tasks
    let (disconnected_sub_broker_sender, disconnected_sub_broker_receiver) = channel::unbounded::<(Uuid, AsyncStdReceiver<Event>, AsyncStdSender<Event>, TokioBroadcastSender<Response>)>();

    // Fuse receivers
    let mut client_event_receiver = event_receiver.fuse();
    let mut client_disconnection_receiver = client_disconnection_receiver.fuse();
    let mut client_exit_sub_broker_receiver = client_exit_sub_broker_receiver.fuse();
    let mut disconnected_sub_broker_receiver = disconnected_sub_broker_receiver.fuse();

    loop {

        // We can receive events from disconnecting clients, connected client reader tasks, sub-broker tasks
        // or disconnecting sub-broker tasks
        let event = select! {

            // Listen for disconnecting clients
            disconnection = client_disconnection_receiver.next().fuse() => {
                // Harvest the data sent by the client's disconnecting procedure and remove the client
                let (peer_id, _client_response_channel, _client_chat_channel) = disconnection
                        .ok_or(ServerError::ChannelReceiveError(String::from("'client_disconnection_receiver' should send harvestable data")))?;

                // Thus we guarantee that the client in the hashmap does not outlive any of the channels
                // hat were spawned inside the main broker task
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
                        if chatroom_name_to_id.remove(&chatroom_broker.name).is_none() {
                            return Err(ServerError::StateError(format!("chatroom sub-broker with name {} should exist in set", chatroom_broker.name)));
                        }
                        // Attempt to send shutdown signal
                        match chatroom_broker.shutdown.take() {
                            Some(shutdown) => {
                                drop(shutdown);
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
                let (chatroom_id, _sub_broker_events, _client_exits, _sub_broker_messages) = disconnection.ok_or(ServerError::ChannelReceiveError(String::from("received 'None' from 'disconnected_sub_broker_receiver'")))?;
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
                    // This case triggers graceful shutdown for the broker task, ie
                    // the accept loop has started shutdown, no client has a sending half of this channel
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
                let mut client = clients.get_mut(&peer_id).ok_or(ServerError::StateError(format!("client with id {} should exist in map", peer_id)))?;

                // Take chatroom broker id from client, since client is exiting
                let chatroom_broker_id = client.chatroom_broker_id
                        .take()
                        .ok_or(ServerError::StateError(format!("client with id {} should have not have 'chatroom_broker_id' set to 'None'", peer_id)))?;

                // Take chatroom broker from map, for updating its state
                let mut chatroom_broker = chatroom_brokers
                    .remove(&chatroom_broker_id)
                    .ok_or(ServerError::StateError(format!("chatroom sub-broker with id {} should exist in map", chatroom_broker_id)))?;

                // Send ExitChatroom response to client
                let resp_chatroom_name = (&*chatroom_broker.name).clone();
                client.main_broker_write_task_sender.send(Response::ExitChatroom {chatroom_name: resp_chatroom_name})
                    .await
                    .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;

                // Decrement client count for the current broker
                chatroom_broker.num_clients -= 1;

                // Check if we need to initiate a shutdown for the current sub-broker
                if chatroom_broker.num_clients == 0 {
                    if chatroom_name_to_id.remove(&chatroom_broker.name).is_none() {
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
            Event::Lobby {peer_id} => {
                let mut client = clients.get_mut(&peer_id)
                    .ok_or(ServerError::StateError(format!("no client with id {} contained in client map", peer_id)))?;
                let response = Response::Lobby {lobby_state: create_lobby_state(&chatroom_brokers)};
                client.main_broker_write_task_sender
                    .send(response)
                    .await
                    .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
            }
            Event::Create {peer_id, chatroom_name} => {
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
                    let lobby_state = create_lobby_state(&mut chatroom_brokers);
                    client.main_broker_write_task_sender.send(
                        Response::ChatroomAlreadyExists {
                            chatroom_name: chatroom_name_clone,
                            lobby_state,
                        })
                        .await
                        .map_err(|_e| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
                } else {
                    // Otherwise we can create the chatroom
                    let id = Uuid::new_v4();

                    // Channel for client subscriptions i.e from chatroom broker to client write tasks
                    // TODO: Set capacity has a parameter
                    let (mut broadcast_sender, broadcast_receiver) = broadcast::channel::<Response>(100);

                    // Channel for client sending i.e from client read tasks to chatroom broker
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

                    // Spawn chatroom broker task
                    let _chatroom_handle = task::spawn(async move {
                        let res = chatroom_broker(id,&mut client_receiver, &mut client_exit_clone, &mut broadcast_sender_clone, shutdown_receiver).await;
                        if let Err(ref e) = res {
                            // TODO: add logging/tracing
                            eprintln!("error occurred inside chatroom broker task with id {}", id);
                            eprintln!("{e}");
                        }
                        disconnection_sender_clone.send((id, client_receiver, client_exit_clone, broadcast_sender_clone))
                            .await
                            .map_err(|_| ServerError::ChannelSendError(format!("chatroom-sub-broker {} unable to send disconnection event to main broker", id)))?;
                        res
                    });

                    // Update client's broker_id
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
                }
            }
            Event::Join {peer_id, chatroom_name} => {
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
                        let lobby_state = create_lobby_state(&chatroom_brokers);
                        let response = Response::ChatroomFull {chatroom_name, lobby_state};
                        client.main_broker_write_task_sender.send(response)
                            .await
                            .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
                    } else {
                        // Get mutable reference here to update state of chatroom
                        let mut chatroom = chatroom_brokers
                            .get_mut(&chatroom_id)
                            .ok_or(ServerError::StateError(format!("chatroom with id {} and name {} should exist in chatroom map", chatroom_id, chatroom_name)))?;

                        chatroom.num_clients += 1;

                        // For sending to client's write/read tasks respectively
                        let chatroom_subscriber = chatroom.client_subscriber.subscribe();
                        let chatroom_sender = chatroom.client_read_sender.clone();

                        // Update state of client
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
                    let lobby_state = create_lobby_state(&chatroom_brokers);
                    let response = Response::ChatroomDoesNotExist {chatroom_name, lobby_state};
                    client.main_broker_write_task_sender.send(response)
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
                }
            }
            Event::Username { peer_id, new_username} => {
                // Get client from map
                let mut client = clients.get_mut(&peer_id)
                    .ok_or(ServerError::StateError(format!("client with id {} should exist in client map", peer_id)))?;

                // Ensure username for the current client is not set
                if client.username.is_some() {
                    return Err(ServerError::IllegalEvent(format!("client with id {} requested to change username while already having username set", peer_id)));
                }
                // Ensure client is not inside an existing chatroom
                if client.chatroom_broker_id.is_some() {
                    return Err(ServerError::IllegalEvent(format!("client with id {} request to change username while inside an existing chatroom", peer_id)));
                }

                if client_usernames.contains(&new_username) {
                    let response = Response::UsernameAlreadyExists { username: new_username };
                    client.main_broker_write_task_sender
                        .send(response)
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
                } else {
                    // Create an arc of username for multiple references
                    let new_username_arc = Arc::new(new_username.clone());
                    // Update state of username set
                    client_usernames.insert(new_username_arc.clone());
                    // Update state of client
                    client.username = Some(new_username_arc);
                    // Create lobby_state and response
                    let lobby_state = create_lobby_state(&chatroom_brokers);
                    let response = Response::UsernameOk {username: new_username, lobby_state};
                    // Send response to client
                    client.main_broker_write_task_sender
                        .send(response)
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("main broker unable to send client {} write task a response", peer_id)))?;
                }
            }
            Event::NewClient {stream, shutdown, chatroom_connection, peer_id} => {
                if clients.contains_key(&peer_id) {
                    return Err(ServerError::StateError(format!("when trying to create a new client with id {}, a client with id {} already exists", peer_id, peer_id)));
                }

                // Channel for main broker to client's write task
                let (main_broker_write_task_sender, mut main_broker_write_task_receiver) = channel::unbounded::<Response>();

                // Channel so client can receive new chatroom subscriptions
                let (new_chatroom_connection_write_sender, mut new_chatroom_connection_write_receiver) = channel::unbounded::<(TokioBroadcastReceiver<Response>, Response)>();

                // Clone the client disconnection receiver for harvesting disconnected clients
                let mut client_disconnection_clone = client_disconnection_sender.clone();

                // Create the new client
                let client = Client {
                    id: peer_id,
                    username: None,
                    main_broker_write_task_sender,
                    new_chatroom_connection_read_sender: chatroom_connection,
                    new_chatroom_connection_write_sender,
                    chatroom_broker_id: None,
                };

                // Spawn new client's write task
                let _client_write_task = task::spawn(async move {
                    let res = client_write_loop(
                        peer_id,
                        stream,
                        &mut main_broker_write_task_receiver,
                        &mut new_chatroom_connection_write_receiver,
                        shutdown,
                    ).await;

                    client_disconnection_clone.send((peer_id, main_broker_write_task_receiver, new_chatroom_connection_write_receiver))
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send disconnection event to main broker", peer_id)))?;

                    res
                });

                // Insert new client into state
                clients.insert(peer_id, client);
            }
            Event::Message {ref message, peer_id} => return Err(ServerError::IllegalEvent(format!("main broker received illegal event from client {}", peer_id))),
        }
    }

    // Once we reach here we know there are no more client's sending events, and we can drain the
    // client disconnection channel
    while let Some((_peer_id, _client_response_channel, _client_message_channel)) = client_disconnection_receiver.next().await {}


    for (_, mut chatroom) in chatroom_brokers {
        if let Some(shutdown) = chatroom.shutdown.take() {
            // Check if we need to initiate shutdown for any chatroom broker
            drop(shutdown);
        }
    }

    // Now drain the chatroom sub-broker disconnection receiver
    while let Some((_chatroom_id, _sub_broker_events, _sub_broker_client_exits, _sub_broker_messages)) = disconnected_sub_broker_receiver.next().await {}

    Ok(())
}

async fn chatroom_broker(
    id: Uuid,
    events: &mut AsyncStdReceiver<Event>,
    client_exit: &mut AsyncStdSender<Event>,
    broadcast_sender: &mut TokioBroadcastSender<Response>,
    shutdown_receiver: AsyncStdReceiver<Null>,
    // disconnection_sender: AsyncStdSender<(Uuid, AsyncStdReceiver<Event>, TokioBroadcastSender<Response>)>
) -> Result<(), ServerError> {
    // Fuse receivers for select loop
    let mut events = events.fuse();
    let mut shutdown_receiver = shutdown_receiver.fuse();

    loop {
        let response = select! {
            event = events.select_next_some().fuse() => {
                match event {
                    Event::Quit { peer_id } => {
                        client_exit.send(Event::Quit { peer_id })
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("chatroom-sub-broker {} unable to send quit event to main broker", id)))?;
                        continue;
                    }
                    Event::Message { message, peer_id} => Response::Message {peer_id, msg: message},
                    _ => return Err(ServerError::IllegalEvent(format!("received illegal event, should only receive message and quit events"))),
                }
            },
            shutdown = shutdown_receiver.next().fuse() => {
                match shutdown {
                    Some(null) => match null {}
                    None => break,
                }
            }
        };

        // TODO: task may block, maybe use a different approach, repeated clones may be expensive
        broadcast_sender
            .send(response)
            .map_err(|_| ServerError::ChannelSendError(format!("chatroom-sub-broker {} unable to broadcast message", id)))?;
        // let mut sender_clone = broadcast_sender.clone();
        // task::spawn_blocking(move || {
        //     sender_clone.send(response).map_err(|_| ServerError::ConnectionFailed)
        // }).await?;

    }

    Ok(())
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
            name: Arc::new(String::from("Test chatroom 666")),
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
        let name = Arc::new(String::from("Test chatroom 666"));

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
        let name = Arc::new(String::from("Test chatroom 666"));

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