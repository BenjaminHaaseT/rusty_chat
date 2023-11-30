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
    // TODO: add logging/tracing
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
    // TODO: add logging/tracing
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
                if let Some(chatroom_channel_sender) = chatroom_sender_opt {
                    chatroom_sender = Some(chatroom_channel_sender);
                    continue;
                }
                eprintln!("None sent from main broker");
                break;
            },
            default => unreachable!("should not happen")
        };

        // If we have a chatroom sender channel, we assume all parsed events are being sent to the
        // current chatroom, otherwise we send events to main broker
        if let Some(chat_sender) = chatroom_sender.take() {
            match frame {
                Frame::Quit => {
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
    mut chatroom_broker_receiver: AsyncStdReceiver<(TokioBroadcastReceiver<Response>, Response)>,
    shutdown: AsyncStdReceiver<Null>,
) -> Result<(), ServerError> {
    println!("inside client write loop");
    // Shadow client stream so it can be written to
    let mut client_stream = &*client_stream;

    // The broker for the chatroom, yet to be set will yield poll pending until it is reset
    let (_, chat_receiver) = tokio::sync::broadcast::channel::<Response>(100);
    let mut chat_receiver = BroadcastStream::new(chat_receiver).fuse();

    // Fuse main broker receiver and shutdown streams
    let mut main_broker_receiver = main_broker_receiver.fuse();
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
                // TODO: handle the exit case from the chatroom
                if let Some(resp) = resp {
                    resp
                    .map_err(|_| ServerError::SubscriptionError("error receiving response from chatroom subscriber"))?
                } else {
                    eprintln!("received none from chat_receiver");
                    break;
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
    username: Option<String>,
    write_task_sender: AsyncStdSender<Response>,
    chatroom_broker_id: Option<Uuid>,
}

#[derive(Debug)]
pub struct Chatroom {
    id: Uuid,
    name: String,
    client_subscriber: TokioBroadcastSender<Response>,
    client_write_sender: AsyncStdSender<Event>,
    capacity: usize,
    num_clients: usize,
}

enum BrokerEvent {}

async fn broker(_event_sender: Sender<Event>, event_receiver: Receiver<Event>) -> Result<(), ServerError> {
    // For keeping track of current clients
    let mut clients:HashMap<Uuid, Client> = HashMap::new();
    let mut client_usernames: HashSet<String> = HashSet::new();

    // Channel for harvesting disconnected clients
    let (
        client_disconnection_sender,
        client_disconnection_receiver
    ) = channel::unbounded::<(Uuid, AsyncStdReceiver<Response>, AsyncStdReceiver<TokioBroadcastReceiver<Response>>)>();

    // For keeping track of chatroom sub-broker tasks
    let mut chatroom_brokers: HashMap<Uuid, Chatroom> = HashMap::new();
    let mut chatroom_names : HashSet<String> = HashSet::new();

    // Channel for communicating with chatroom-sub-broker tasks
    let (sub_broker_sender, sub_broker_receiver) = channel::unbounded::<Event>();

    // Channel for harvesting disconnected chatroom-sub-broker tasks
    let (_disconnected_sub_broker_sender, disconnected_sub_broker_receiver) = channel::unbounded::<(Uuid, AsyncStdSender<Event>, TokioBroadcastSender<Response>)>();

    // Fuse receivers
    let mut event_receiver = event_receiver.fuse();
    let mut client_disconnection_receiver = client_disconnection_receiver.fuse();
    let mut sub_broker_receiver = sub_broker_receiver.fuse();
    let mut disconnected_sub_broker_receiver = disconnected_sub_broker_receiver.fuse();

    // loop and listen for events triggered by input channels
    loop {

        // We can receive events from disconnecting clients, connected clients or sub_broker tasks
        let event = select! {

            // Listen for disconnecting clients
            disconnection = client_disconnection_receiver.next().fuse() => {
                // Harvest the data sent by the client's disconnecting procedure
                let (peer_id, client_responses, client_messasges) = disconnection;
                // Remove the client
                let removed_client = clients.remove(&peer_id);

                // Ensure the client does it exist, it is a panic condition if the client does not exist
                assert!(removed_client.is_some(), "client should exist in 'clients' map");

                // Check if the client has a set username
                if let Some(username) = removed_client.as_ref().unwrap().username.as_ref() {
                    // Ensure the client's username is removed, it is a panic condition if the client's
                    // username is not set in the hashset
                    assert!(
                        client_usernames.remove(username),
                        "client user name should exist in 'client_usernames' set"
                    );
                }

                // Check if the client disconnected while inside a chatroom or not, if so ensure
                // we update the count of number of clients for the specified chatroom
                if let Some(chatroom_id) = removed_client.as_ref().unwrap().chatroom_broker_id.as_ref() {
                    // Ensure the chatroom_id is contained in, it is a panic condition if the chatroom broker's
                    // id does not exist in the hashmap
                    assert!(chatroom_brokers.contains_key(chatroom_id), "chatroom id should exist in 'chatroom_brokers' map");

                    // Update the client count for this chatroom
                    chatroom_brokers.get_mut(&chatroom_id).unwrap().num_clients -= 1;
                }

                // Continue listening for more events
                continue;
            },

            // Listen for disconnection chatroom-sub-broker tasks
            disconnection = disconnected_sub_broker_receiver.next().fuse() => {
                let (chatroom_id, sub_broker_events, sub_broker_messages) = disconnection;
                todo!()
            }

            // Listen for events triggered by clients
            event = event_receiver.next().fuse() => {
                match event {
                    Some(event) => event,
                    None => break,
                }
            }

            // Listen for events triggered by sub-broker-tasks
            event = sub_broker_receiver.next().fuse() => {
                todo!()
            }

        };
    }

    todo!()
}



fn main() {todo!()}