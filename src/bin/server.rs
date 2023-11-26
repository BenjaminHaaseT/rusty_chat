use std::fmt::Debug;
use std::sync::Arc;
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

    let mut frame_tag = [0u8; 5];

    loop {
        // We need to listen to 2 potential events, we receive input from the client's stream
        // or we receive a sending part of a chatroom broker task
        let frame = select! {
            // read input from client
            res = client_reader.read_exact(&mut frame_tag).fuse() => {
                match res {
                    Ok(_) => Frame::try_parse(&frame_tag, &mut client_reader).await.map_err(|e| ServerError::ParseFrame(e))?,
                    Err(e) => return Err(ServerError::ConnectionFailed),
                }
            },
            chatroom_sender_opt = chatroom_broker_receiver.next().fuse() => {
                if let Some(chatroom_channel_sender) = chatroom_sender_opt {
                    chatroom_sender = Some(chatroom_channel_sender);
                    continue;
                }
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

// struct PendingSubscription<T: Clone> {
//     subscriber: Option<TokioBroadcastReceiver<T>>,
// }
//
// impl<T: Clone> PendingSubscription<T> {
//     pub fn new() -> PendingSubscription<T> {
//         PendingSubscription { subscriber: None }
//     }
//
//     pub fn init(subscriber: TokioBroadcastReceiver<T>) -> PendingSubscription<T> {
//         PendingSubscription { subscriber: Some(subscriber) }
//     }
//
//     pub fn set_subscription(&mut self, subscriber: TokioBroadcastReceiver<T>) {
//         self.subscriber = Some(subscriber);
//     }
// }
//
// impl<T: Clone> Stream for PendingSubscription<T> {
//     type Item = Result<T, ServerError>;
//     fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
//         if let Some(mut sub) = self.subscriber.take() {
//             match sub.recv().poll(cx) {
//                 Poll::Ready(val) => {
//                     self.subscriber = Some(sub);
//                     Poll::Ready(Some(val.map_err(|_e| ServerError::PendingSubscriptionError("error when reading from subscriber's stream"))))
//                 }
//                 Poll::Pending  => Poll::Pending
//             }
//         }
//         Poll::Pending
//     }
// }

async fn client_write_loop(
    client_stream: Arc<TcpStream>,
    main_broker_receiver: AsyncStdReceiver<Response>,
    mut chatroom_broker_receiver: AsyncStdReceiver<(TokioBroadcastReceiver<Response>, Response)>,
    shutdown: AsyncStdReceiver<Null>
) -> Result<(), ServerError> {
    // TODO: Add logging/tracing...
    println!("Inside client write loop...");

    // Shadow client stream so it can be written too
    let mut client_stream = &*client_stream;

    // The broker for the chatroom, yet to be set will yield poll pending until it is reset
    let (_, chat_receiver) = tokio::sync::broadcast::channel::<Response>(100);
    let mut chat_receiver = BroadcastStream::new(chat_receiver).fuse();

    // Fuse main broker receiver
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
            resp = main_broker_receiver.next().fuse() => {
                match resp {
                    _ => todo!()
                }
            },
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
                        break;
                    }
                }
            },
            resp = chat_receiver.next().fuse() => {
                if let Some(resp) = resp {
                    resp.map_err(|_| ServerError::SubscriptionError("error receiving response from subscriber"))?
                } else {
                    eprintln!("received none from chat_receiver");
                    break;
                }
            }
        };
    }

    todo!()
}

async fn broker(_event_sender: Sender<Event>, event_receiver: Receiver<Event>) -> Result<(), ServerError> {
    todo!()
}



fn main() {todo!()}