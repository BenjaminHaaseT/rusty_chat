use std::fmt::Debug;
use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use std::io::Read as StdRead;
use clap::Parser;
use tracing::{instrument, info, error, debug};
use tracing_subscriber;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use async_std::io::{Read, ReadExt, Write, WriteExt};
use async_std::channel::{self, Sender, Receiver};
use async_std::net::ToSocketAddrs;
use async_std::net::{TcpListener, TcpStream};
use async_std::task;
use tokio::sync::broadcast;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use futures::{
    future::{Future, FutureExt,  FusedFuture},
    stream::{Stream, StreamExt, FusedStream},
    select,
};
use uuid::Uuid;

use crate::server_error::ServerError;
use rusty_chat::{*, Event, Null};

mod server_handlers;
use server_handlers::prelude::*;
mod chatroom_task;
mod server_error;


/// The accept loop for the server. Binds a 'TcpListener' to 'server_addrs', spawns the main
/// broker task and awaits incoming client connections and spawns them onto new tasks for handling connections.
#[instrument(ret, err)]
async fn accept_loop(server_addrs: impl ToSocketAddrs + Clone + Debug, channel_buf_size: usize, chatroom_capacity: usize) -> Result<(), ServerError> {
    info!("Listening at {:?}...", server_addrs);
    // Connect the supplied address
    let mut listener = TcpListener::bind(server_addrs.clone())
        .await
        .map_err(|_| ServerError::ConnectionFailed(format!("unable to bind listener at address {:?}", server_addrs)))?;
    // For communicating with the main broker
    let (broker_sender, broker_receiver) = channel::bounded::<Event>(channel_buf_size);

    // spawn broker task
    let broker_handle = task::spawn(broker(broker_receiver, channel_buf_size, chatroom_capacity));

    // Listen for incoming client connections
    while let Some(stream) = listener.incoming().next().await {
        let stream_res = stream.map_err(|e| ServerError::ConnectionFailed(format!("accept loop received an error: {:?}", e)));
        match stream_res {
            Ok(stream) => {
                info!(peer_addr = ?stream.peer_addr(), "Accepting client connection: {:?}", stream.peer_addr());
                task::spawn(handle_connection(broker_sender.clone(), stream));
            }
            Err(e) => error!(error = ?e)
        }
    }
    broker_handle.await?;
    Ok(())
}

/// Processes a new client connection.
///
/// Takes 'client_stream' for parsing events triggered by the client and
/// for writing responses back to the client from the server. 'main_broker_sender' is used for sending
/// events triggered by the client to the main broker task.
#[instrument(ret, err, skip(main_broker_sender, client_stream), fields(peer_address = ?client_stream.peer_addr()), msg = "Handling connection for client")]
async fn handle_connection(main_broker_sender: Sender<Event>, client_stream: TcpStream) -> Result<(), ServerError> {
    // debug!(peer_address = ?client_stream.peer_addr(), "Handling connection for client {:?}", client_stream.peer_addr());
    // Move into an arc, so it can be shared between read/write tasks
    let client_stream = Arc::new(client_stream);
    // strictly for reading from the client
    let mut client_reader = &*client_stream;

    // Id for the client
    let peer_id = Uuid::new_v4();

    // Channel for synchronizing graceful shutdown with the client's writer task. client_shutdown_receiver,
    // will be sent to the main broker task as part of a Event::NewClient so when the broker spawns,
    // this client's write task, the write task will be synchronized with this task when shutdown needs to occur.
    // When this tasks finishes, _client_shutdown_sender will be dropped signalling to client_shutdown_receiver
    // that the client has disconnected.
    let (_client_shutdown_sender, client_shutdown_receiver) = channel::unbounded::<Null>();

    // Channel for receiving new chatroom-sub-broker connections from the main broker.
    // Receives a 'Sender<Event>' when the client has joined a new chatroom
    let (chatroom_broker_sender, chatroom_broker_receiver) = channel::unbounded::<AsyncStdSender<Event>>();

    // New client event
    let event = Event::NewClient {
        peer_id,
        stream: client_stream.clone(),
        shutdown: client_shutdown_receiver,
        chatroom_connection: chatroom_broker_sender
    };

    // Send a new client event to the main broker, should not be disconnected,
    // function should return an error if this is unsuccessful
    main_broker_sender.send(event)
        .await
        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;

    // for sending message events when the client joins a new chatroom. Only set to 'Some'
    // variant when the client is inside a chatroom
    let mut chatroom_sender: Option<AsyncStdSender<Event>> = None;

    // Fuse so it can be selected over
    let mut chatroom_broker_receiver = chatroom_broker_receiver.fuse();

    loop {
        // We need to listen to 2 potential events, we receive input from the client's stream
        // or we receive a sending part of a chatroom broker task
        let frame = select! {
            // Read input from client
            res = Frame::try_parse(&mut client_reader).fuse() => {
                info!(frame_res = ?res, peer_id = ?peer_id, "Received frame in client {} handle_connection task", peer_id);
                match res {
                    Ok(frame) => frame,
                    Err(e) => return Err(ServerError::ParseFrame(format!("client {} unable to parse frame in handle connection task", peer_id))),
                }
            },
            // Receive a new connection to a chatroom-sub broker task
            chatroom_sender_opt = chatroom_broker_receiver.next().fuse() => {
                info!(peer_id = ?peer_id, "Client {} Received new chatroom broker channel", peer_id);
                match chatroom_sender_opt {
                    Some(chatroom_channel) => {
                        chatroom_sender = Some(chatroom_channel);
                        // Send the main broker an event that this client is now able to start sending messages
                        main_broker_sender.send(Event::ReadSync {peer_id})
                            .await
                            .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                        debug!(peer_id = ?peer_id, "Client {} sent `ReadSync` event to main broker from handle_connection task", peer_id);
                        continue;
                    },
                    None => return Err(ServerError::ChannelReceiveError(format!("client {} handle connection task received invalid value from 'chatroom_broker_receiver'", peer_id))),
                }
            },
        };
        // If we have a chatroom sender channel, we assume all parsed events are being sent to the
        // current chatroom, otherwise we send events to main broker. This necessarily means
        // an invariant needs to be enforced by the client, that all Frames sent to the server after
        // joining/creating a chatroom are only Message/Quit variants.
        if let Some(chat_sender) = chatroom_sender.take() {
            match frame {
                Frame::Quit => {
                    // take chat_sender without replacing, since we are sending a quit message
                    // client no longer desires to be in this chatroom
                    chat_sender.send(Event::Quit {peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to chatroom broker", peer_id)))?;
                    info!(peer_id = ?peer_id, "Client {} quiting chatroom", peer_id);
                }
                Frame::Message {message} => {
                    chat_sender.send(Event::Message {message, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to chatroom broker", peer_id)))?;
                    // place chat_sender back into chatroom_sender
                    chatroom_sender = Some(chat_sender);
                    debug!(peer_id = ?peer_id, "Client {} sending message", peer_id);

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
                    info!(peer_id = ?peer_id, "Client {} leaving chatroom lobby", peer_id);
                    break;
                },
                Frame::Lobby => {
                    main_broker_sender.send(Event::Lobby {peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                    debug!(peer_id = ?peer_id, "Client {} sent `Lobby` Event to main broker task", peer_id);
                }
                Frame::Create {chatroom_name} => {
                    main_broker_sender.send( Event::Create {chatroom_name, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                    debug!(peer_id = ?peer_id, "Client {} sent `Create` Event to main broker task", peer_id);

                },
                Frame::Join {chatroom_name} => {
                    main_broker_sender.send(Event::Join {chatroom_name, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                    debug!(peer_id = ?peer_id, "Client {} sent `Join` Event to main broker task", peer_id);

                },
                Frame::Username {new_username} => {
                    main_broker_sender.send(Event::Username {new_username, peer_id})
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to send event to main broker", peer_id)))?;
                    debug!(peer_id = ?peer_id, "Client {} sent `Username` Event to main broker task", peer_id);

                },
                _ => return Err(ServerError::IllegalFrame(format!("client {} handle connection task received an illegal frame: {:?}", peer_id, frame))),
            }
        }
    }

    Ok(())
}

/// The task that will receive 'Response's from a broker and write them to the client's TcpStream.
///
/// Takes 'client_id' for filtering messages received from a chatroom sub-broker, 'client_stream'
/// for writing (only for writing) 'Response's, 'main_broker_receiver' for receiving 'Response's from
/// the main broker task, 'chatroom_broker_receiver' for receiving 'Response's from a chatroom sub-broker
/// task and 'shutdown' a channel used exclusively for signalling graceful shutdown with this client's
/// read task.
#[instrument(ret, err, skip(client_stream, main_broker_receiver, chatroom_broker_receiver, shutdown))]
async fn client_write_loop(
    client_id: Uuid,
    client_stream: Arc<TcpStream>,
    main_broker_receiver: &mut AsyncStdReceiver<Response>,
    chatroom_broker_receiver: &mut AsyncStdReceiver<(TokioBroadcastReceiver<Response>, Response)>,
    shutdown: AsyncStdReceiver<Null>,
) -> Result<(), ServerError> {
    debug!(peer_id = ?client_id, "Inside client_write_loop task for client {}", client_id);
    // Shadow client stream so it can be written to
    let mut client_stream = &*client_stream;

    // The broker for the chatroom, yet to be set, will yield poll pending until a chatroom sub-broker
    // subscription is received from chatroom_broker_receiver
    let (mut dummy_chat_sender, chat_receiver) = tokio::sync::broadcast::channel::<Response>(100);
    let mut chat_receiver = BroadcastStream::new(chat_receiver).fuse();

    // Fuse main broker receiver and shutdown streams
    let mut main_broker_receiver = main_broker_receiver.fuse();
    let mut chatroom_broker_receiver = chatroom_broker_receiver.fuse();
    let mut shutdown = shutdown.fuse();

    loop {
        // Select over possible receiving options, we can receive a shutdown signal from the clients
        // read task, a response from the main broker, a response from a chatroom sub-broker task or
        // a new connection to a chatroom sub-broker task.
        let response: Response = select! {
            // Check for a disconnection event
            null = shutdown.next().fuse() => {
                info!(peer_id = ?client_id, "Client {} write task shutting down", client_id);
                match null {
                    Some(null) => match null {},
                    _ => {
                        // Write an exit lobby response back to client so the client can finish
                        // if the client task is waiting for a response
                        client_stream.write_all(&Response::ExitLobby.as_bytes())
                            .await
                            .map_err(|_| ServerError::ChannelSendError(format!("client write task {} unable to send 'ExitLobby' response during task shutdown", client_id)))?;
                        debug!(peer_id = ?client_id, "Client {} write task wrote `ExitLobby` response to client's stream", client_id);
                        break;
                    }
                }
            },

            // Check for a response from main broker
            resp = main_broker_receiver.next().fuse() => {
                debug!(peer_id = ?client_id, "Client {} received response from main broker", client_id);
                match resp {
                    Some(resp) => {
                        // Check if client is exiting from a chatroom
                        if resp.is_exit_chatroom() {
                            info!(peer_id = ?client_id, "Client {} write task received `ExitChatroom` response", client_id);
                            // Create new dummy channel to replace old chatroom_broker channel
                            let (new_dummy_chat_sender, new_chat_receiver) = broadcast::channel::<Response>(100);
                            // Reset the chat_receiver channel
                            dummy_chat_sender = new_dummy_chat_sender;
                            chat_receiver = BroadcastStream::new(new_chat_receiver).fuse();
                        }
                        // forward response sent from main broker so it can be written back to the client
                        resp
                    },
                    None => return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received 'None' from 'main_broker_receiver'", client_id)))
                }
            }

            // Check for a new chatroom broker receiver
            subscription = chatroom_broker_receiver.next().fuse() => {
                info!(peer_id = ?client_id, "Client {} write task attempting to subscribe to new chatroom", client_id);
                match subscription {
                    Some(sub) => {
                        let (new_chat_receiver, resp) = sub;
                        // Ensure that resp is the Subscribed or ChatroomCreated variant,
                        // otherwise we need to return early with an error
                        if resp.is_subscribed() || resp.is_chatroom_created() {
                            debug!(peer_id = ?client_id, "Client {} write task received valid subscription/created response from `chatroom_broker_receiver`", client_id);
                            // Update chat_receiver so client can receive new messages from chatroom
                            chat_receiver = BroadcastStream::new(new_chat_receiver).fuse();
                            // Forward response so it can be written to clients stream
                            resp
                        } else {
                            return Err(ServerError::IllegalResponse(format!("client {} received a {:?} response on 'chatroom_broker_receiver'", client_id, resp)));
                        }
                    },
                    None => return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received 'None' from 'chatroom_broker_receiver'", client_id))),
                }
            },

            // Check for a response from the chatroom broker
            resp = chat_receiver.next().fuse() => {
                debug!(peer_id = ?client_id, "Client {} received a response from `chat_receiver`", client_id);
                match resp {
                    Some(msg_resp) => {
                        // Check if client has lagged or not
                        let msg_resp = match msg_resp {
                            Ok(msg_resp) => msg_resp,
                            Err(BroadcastStreamRecvError::Lagged(n)) => {
                                // Log the error but do not quit the task altogether
                                error!(peer_id = ?client_id, num_lagged_msg = n, "Client {} lagged {} messages", client_id, n);
                                continue;
                            }
                            Err(e) => return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received an error from 'chat_receiver'", client_id)))
                        };
                        // Filter the response, check that we only receive messages and filter
                        // all messages not from the current client
                        match &msg_resp {
                            Response::Message {msg, peer_id} if *peer_id != client_id => msg_resp,
                            Response::Message { msg, peer_id } => continue,
                            _ => return Err(ServerError::IllegalResponse(format!("client {} write task received {:?} response from chatroom broker", client_id, msg_resp))),
                        }
                    },
                    None => return Err(ServerError::ChannelReceiveError(format!("client {} write loop task received an 'None' from 'chat_receiver'", client_id))),
                }
            }
        };

        // Write response back to client's stream
        client_stream.write_all(response.as_bytes().as_slice())
            .await
            .map_err(|_| ServerError::ChannelSendError(format!("client {} unable to write response to stream", client_id)))?;
        debug!(peer_id = ?client_id, "Client {} write task successfully wrote response to stream", client_id);
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

/// Helper function for creating a vector of bytes that represents the current state of the chatroom lobby.
fn create_lobby_state(chatroom_brokers: &HashMap<Uuid, Chatroom>) -> Vec<u8> {
    let mut lobby_state = vec![];
    for (_, chatroom) in chatroom_brokers {
        lobby_state.append(&mut chatroom.as_bytes());
    }
    lobby_state
}

/// The main broker task that acts as the brain of the chatroom server.
///
/// This function orchestrates new handling events from clients and chatroom sub-broker tasks and
/// manages the state necessary for making this work.
///
/// # Parameters
/// 'event_receiver' for incoming events sent directly from clients
#[instrument(ret, err, skip_all)]
async fn broker(event_receiver: Receiver<Event>, channel_buf_size: usize, chatroom_capacity: usize) -> Result<(), ServerError> {
    debug!("Inside main broker task");
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

    // Channel for communicating with chatroom-sub-broker tasks, used exclusively for a client exiting from a chatroom
    // that way the main broker task knows to send an ExitChatroom response to the client that has exited
    let (client_exit_sub_broker_sender, client_exit_sub_broker_receiver) = channel::unbounded::<Event>();

    // Channel for harvesting disconnected chatroom-sub-broker tasks
    let (disconnected_sub_broker_sender, disconnected_sub_broker_receiver) = channel::unbounded::<(Uuid, AsyncStdReceiver<Event>, AsyncStdSender<Event>, TokioBroadcastSender<Response>)>();

    // Fuse receivers
    // For receiving events directly from client read tasks
    let mut client_event_receiver = event_receiver.fuse();
    // For harvesting disconnecting/quiting clients
    let mut client_disconnection_receiver = client_disconnection_receiver.fuse();
    // For receiving clients exiting from chatrooms
    let mut client_exit_sub_broker_receiver = client_exit_sub_broker_receiver.fuse();
    // For harvesting disconnecting chatroom sub-broker tasks
    let mut disconnected_sub_broker_receiver = disconnected_sub_broker_receiver.fuse();

    debug!("Entering broker select loop");

    loop {

        // We can receive events from disconnecting clients, connected client reader tasks, sub-broker tasks
        // or disconnecting sub-broker tasks, select over all possible receiving options.
        let event = select! {
            // Listen for disconnecting clients
            disconnection = client_disconnection_receiver.next().fuse() => {
                debug!("Received disconnecting client");
                // Harvest the data sent by the client's disconnecting procedure and remove the client
                let (peer_id, _client_response_channel, _client_chat_channel) = disconnection
                        .ok_or(ServerError::ChannelReceiveError(String::from("'client_disconnection_receiver' should send harvestable data")))?;

                info!(peer_id = ?peer_id, "Harvested disconnected client: {}", peer_id);

                // Thus we guarantee that the client in the hashmap does not outlive any of the channels
                // that were spawned inside the main broker task
                let mut removed_client = clients.remove(&peer_id).ok_or(ServerError::StateError(format!("client with peer id {} should exist in client map", peer_id)))?;

                info!(peer_id = ?peer_id, "Client {} was successfully removed from client map", peer_id);

                // Check if client had set their username
                if let Some(username) = removed_client.username.as_ref() {
                    // Attempt to remove clients username from name set
                    if !client_usernames.remove(username) {
                        return Err(ServerError::StateError(format!("client username {} should exist in set", username)));
                    }
                    debug!(username = ?username, "Successfully removed client {} username", peer_id);
                }

                // Check if the client disconnected while inside a chatroom or not, if so ensure
                // we update the client count for the specified chatroom
                if let Some(chatroom_id) = removed_client.chatroom_broker_id.take() {
                    info!(peer_id = ?peer_id, chatroom_id = ?chatroom_id, "Client {} was disconnected while inside chatroom {}", peer_id, chatroom_id);
                    // Take chatroom broker for the purpose of updating and potentially removing
                    // if the chatroom is now empty
                    let mut chatroom_broker = chatroom_brokers
                        .remove(&chatroom_id)
                        .ok_or(ServerError::StateError(format!("chatroom sub-broker with id {} should exist in map", chatroom_id)))?;

                    // Decrement client count for the broker
                    chatroom_broker.num_clients -= 1;

                    // Check if client count is zero, if so remove it from the name set and send a shutdown signal
                    // do not place broker back into map
                    if chatroom_broker.num_clients == 0 {
                        info!(chatroom_id = ?chatroom_id, "Chatroom {} is now empty, initiating shutdown procedure", chatroom_id);
                        // Attempt to remove name from  name map
                        chatroom_name_to_id.remove(&chatroom_broker.name)
                            .ok_or(ServerError::StateError(format!("chatroom sub-broker with name {} should exist in map", chatroom_broker.name)))?;

                        // Attempt to send shutdown signal
                        match chatroom_broker.shutdown.take() {
                            Some(shutdown) => {
                                info!(chatroom_id = ?chatroom_id, "Dropping chatroom shutdown sender");
                                drop(shutdown);
                            }
                            _ => return Err(ServerError::StateError(format!("chatroom sub-broker with id {} should have shutdown set to Some", chatroom_id))),
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
                info!("Received disconnected chatroom sub-broker");
                // Harvest the returned data and ensure that the chatroom broker has been successfully removed
                let (chatroom_id, _sub_broker_events, _client_exits, _sub_broker_messages) = disconnection.ok_or(ServerError::ChannelReceiveError(String::from("received 'None' from 'disconnected_sub_broker_receiver'")))?;
                info!(chatroom_id = ?chatroom_id, "Harvesting disconnected chatroom {} sub-broker", chatroom_id);
                if chatroom_brokers.contains_key(&chatroom_id) {
                    return Err(ServerError::StateError(format!("chatroom sub-broker with id {} should no longer exist in map", chatroom_id)));
                }
                info!(chatroom_id = ?chatroom_id, "Harvested disconnected chatroom {} sub-broker successfully", chatroom_id);
                // Continue listening for more events
                continue;
            }

            // Listen for events triggered by clients
            event = client_event_receiver.next().fuse() => {
                match event {
                    Some(event) => {
                        info!("Main broker received event");
                        event
                    },
                    // This case triggers graceful shutdown for the broker task, ie
                    // the accept loop has started shutdown, and no client has a sending half of this channel
                    None => {
                        info!("Main broker received shutdown signal, shutting down");
                        break;
                    },
                }
            }

            // Listen for exiting clients from sub-brokers
            event = client_exit_sub_broker_receiver.next().fuse() => {
                info!("Main broker received a client exiting from chatroom event");
                let peer_id = event.map_or(
                    Err(ServerError::ChannelReceiveError(String::from("received 'None' from 'client_exit_sub_broker_receiver'"))),
                    |ev| -> Result<Uuid, ServerError> {
                        match ev {
                            Event::Quit {peer_id} => Ok(peer_id),
                            _ => Err(ServerError::IllegalEvent(String::from("received illegal 'Event' from 'client_exit_sub_broker_receiver'")))
                        }
                    }
                )?;

                info!(peer_id = ?peer_id, "Successfully received client {}", peer_id);

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
                        info!(chatroom_id = ?chatroom_broker_id, "Chatroom {} is now empty, initiating shutdown procedure", chatroom_broker_id);
                    chatroom_name_to_id.remove(&chatroom_broker.name)
                            .ok_or(ServerError::StateError(format!("chatroom sub-broker with name {} should exist in map", chatroom_broker.name)))?;
                    match chatroom_broker.shutdown.take() {
                        Some(shutdown) => {
                            info!(chatroom_id = ?chatroom_broker_id, "Dropping chatroom shutdown sender");
                            drop(shutdown);
                        },
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
        info!(event = ?event, "Responding to event");
        // Now attempt to parse event and send a response
        match event {
            Event::Quit {peer_id} => {
                info!(peer_id = ?peer_id, "Client {:?} has quit", peer_id);
            }
            Event::Lobby {peer_id} => {
                handle_lobby_event(peer_id, &mut clients, &chatroom_brokers).await?;
            }
            Event::ReadSync  {peer_id} => {
                handle_read_sync_event(peer_id, &mut clients).await?;
            }
            Event::Create {peer_id, chatroom_name} => {
                handle_create_event(
                    peer_id,
                    chatroom_name,
                    &mut clients,
                    &mut chatroom_brokers,
                    &mut chatroom_name_to_id,
                    &client_exit_sub_broker_sender,
                    &disconnected_sub_broker_sender,
                    channel_buf_size,
                    chatroom_capacity
                ).await?;
            }
            Event::Join {peer_id, chatroom_name} => {
                handle_join_event(
                    peer_id,
                    chatroom_name,
                    &mut clients,
                    &mut chatroom_brokers,
                    &mut chatroom_name_to_id,
                ).await?;
            }
            Event::Username { peer_id, new_username} => {
                handle_username_event(
                    peer_id,
                    new_username,
                    &mut clients,
                    &mut client_usernames,
                    &mut chatroom_brokers
                ).await?;
            }
            Event::NewClient {stream, shutdown, chatroom_connection, peer_id} => {
                handle_new_client_event(
                    peer_id,
                    stream,
                    shutdown,
                    chatroom_connection,
                    &mut clients,
                    &client_disconnection_sender
                ).await?;
            }
            Event::Message {ref message, peer_id} => return Err(ServerError::IllegalEvent(format!("main broker received illegal event from client {}", peer_id))),
        }
    }

    // Once we reach here we know there are no more client's sending events, and we can drain the
    // client disconnection channel
    info!("Graceful shutdown initiated, draining `client_disconnection_receiver` channel...");
    while let Some((peer_id, _client_response_channel, _client_message_channel)) = client_disconnection_receiver.next().await {
        info!(peer_id = ?peer_id, "Harvested client {}", peer_id);
    }
    info!("Draining `client_disconnection_receiver` channel complete");
    info!("Sending shutdown signals to all remaining chatroom brokers");
    for (_, mut chatroom) in chatroom_brokers {
        if let Some(shutdown) = chatroom.shutdown.take() {
            // Check if we need to initiate shutdown for any chatroom broker
            drop(shutdown);
        }
    }
    info!("Draining `disconnected_sub_broker_receiver` channel..");
    // Now drain the chatroom sub-broker disconnection receiver
    while let Some((chatroom_id, _sub_broker_events, _sub_broker_client_exits, _sub_broker_messages)) = disconnected_sub_broker_receiver.next().await {
        info!(chatroom_id = ?chatroom_id, "Harvested chatroom sub-broker {}", chatroom_id);
    }
    info!("Draining `disconnected_sub_broker_receiver` channel complete");
    info!("Graceful shutdown completed");
    Ok(())
}

/// Task that manages sending/broadcasting of messages for a single chatroom.
///
/// # Parameters
/// 'id' The id of the chatroom
/// 'events' A stream of events received directly from clients
/// 'client_exit' The sending half of a channel used to send notifications to the main broker
/// that a client has exited its chatroom
/// 'broadcast_sender' A broadcast channel so that any client write task that has subscribed can receive messages
/// 'shutdown_receiver' A channel for receiving shutdown signals from the main broker
#[instrument(ret, err, skip(events, client_exit, broadcast_sender, shutdown_receiver), fields(message = "Chatroom sub-broker task started"))]
async fn chatroom_broker(
    id: Uuid,
    events: &mut AsyncStdReceiver<Event>,
    client_exit: &mut AsyncStdSender<Event>,
    broadcast_sender: &mut TokioBroadcastSender<Response>,
    shutdown_receiver: AsyncStdReceiver<Null>,
) -> Result<(), ServerError> {
    // Fuse receivers for select loop
    let mut events = events.fuse();
    let mut shutdown_receiver = shutdown_receiver.fuse();

    loop {
        let response = select! {
            event = events.select_next_some().fuse() => {
                info!(chatroom_id = ?id, event = ?event, "Received event");
                match event {
                    Event::Quit { peer_id } => {
                        info!(peer_id = ?peer_id, chatroom_id = ?id, "Client {} exiting chatroom {}", peer_id, id);
                        client_exit.send(Event::Quit { peer_id })
                        .await
                        .map_err(|_| ServerError::ChannelSendError(format!("chatroom-sub-broker {} unable to send quit event to main broker", id)))?;
                        info!(peer_id = ?peer_id, chatroom_id = ?id, "Successfully sent `Quit` event to main broker");
                        continue;
                    }
                    Event::Message { message, peer_id} => {
                        debug!(peer_id = ?peer_id, chatroom_id = ?id, "Received message from {}", peer_id);
                        Response::Message {peer_id, msg: message}
                    },
                    _ => return Err(ServerError::IllegalEvent(format!("received illegal event, should only receive message and quit events"))),
                }
            },
            shutdown = shutdown_receiver.next().fuse() => {
                info!(chatroom_id = ?id, "Received shutdown signal");
                match shutdown {
                    Some(null) => match null {}
                    None => {
                        info!(chatroom_id = ?id, "Shutting down");
                        break;
                    },
                }
            }
        };
        broadcast_sender
            .send(response)
            .map_err(|_| ServerError::ChannelSendError(format!("chatroom-sub-broker {} unable to broadcast message", id)))?;
        debug!(chatroom_id = ?id, "Message was successfully broadcast to subscribers");
    }

    Ok(())
}

/// The command line interface used configure and start the server.
#[derive(Parser)]
struct CLI {
    /// The address the server will be run on
    #[arg(short = 'a')]
    address: String,
    /// The tcp port for the server
    #[arg(short = 'p')]
    port: u16,
    /// The size of the channel buffer for any channel that has a bounded buffer size
    #[arg(short = 'b')]
    channel_buf_size: usize,
    /// Determines the size of the chatroom capacity
    #[arg(short = 'c')]
    chatroom_capacity: Option<usize>,

}

fn main() {
    let format = tracing_subscriber::fmt::layer()
        .with_line_number(true)
        .with_level(true)
        .with_file(true)
        .with_target(true)
        .with_thread_ids(true);

    let filter = tracing_subscriber::EnvFilter::from_default_env();

    tracing_subscriber::registry()
        .with(format)
        .with(filter)
        .init();
    let cli = CLI::parse();

    let res = task::block_on(accept_loop(
        (cli.address.as_str(), cli.port),
        cli.channel_buf_size,
        cli.chatroom_capacity.unwrap_or(1000)
    ));
    if let Err(e) = res {
        eprintln!("error from main: {e}");
    } else {
        println!("exited successfully from server");
    }
}

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

        let tag = chatroom.serialize();

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

        let mut expected_bytes = chatroom.serialize().to_vec();
        expected_bytes.extend_from_slice(name.as_bytes());

        assert_eq!(chatroom_bytes, expected_bytes);
    }
}