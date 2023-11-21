
use uuid::Uuid;

use std::collections::HashMap;
use async_std::net;
pub use async_std::channel::{Receiver as AsyncStdReceiver, Sender as AsyncStdSender};
pub use tokio::sync::broadcast::{Sender as TokioBroadcastSender, Receiver as TokioBroadcastReceiver};
// pub mod room;

pub mod prelude {
    pub use super::*;
}

pub struct Chatroom {
    name: String,
    id: Uuid,
    peers: HashMap<Uuid, String>,
    // message_sender: TokioBroadcastSender<Message>,
    // message_receiver: AsyncStdReceiver<Message>,
    // client_receiver: AsyncStdReceiver<Client>,
    // client_disconnect_receiver: AsyncStdReceiver<(Client, AsyncStdSender<Empty>)>

}

// TODO: implement parsing to and from bytes for message events
#[derive(Clone)]
pub enum Event {
    Quit,
    Join {
        chatroom_name: String,
    },
    Create {
        chatroom_name: String,
    },
    NewClient {
        username: String,
        stream: net::TcpStream,
    },
    UpdateUsername {
        new_username: String,
        peer_id: Uuid,
    }
}

impl Event {
    
    fn parse_length(tag: &mut EventTag, length: u32) {
        for i in 1..tag.len() {
            tag[i] ^= ((length >> ((i - 1) * 8)) & 0xff) as u8;
        }
    }
}

pub type EventTag = [u8; 5];

impl SerializationTag for EventTag {}

pub type EventTvlTag = (u8, u32);

impl DeserializationTag for EventTvlTag {}

impl SerAsBytes for Event {
    type Tag = EventTag;

    fn serialize(&self) -> Self::SerializationTag {
        let mut tag = [0u8; 5];
        match self {
            Event::Quit => {
                tag[0] ^= 1;
            }
            Event::Create {chatroom_name} => {
                tag[0] ^= 1 << 1;
                Event::parse_length(&mut tag, chatroom_name.len() as u32);
            }
            Event::Join {chatroom_name} => {
                tag[0] ^= 1 << 2;
                Event::parse_length(&mut tag, chatroom_name.len() as u32);
            }
        }
        
        tag
    }
}

// impl DeSerAsBytes for Event {
//
// }


pub struct Client {
    pub username: String,
    pub id: Uuid,
    pub stream: Option<net::TcpStream>
}

pub enum Null {}

pub trait SerAsBytes {
    type Tag: SerializationTag;
    fn serialize(&self) -> Self::SerializationTag;
}

pub trait DeSerAsBytes: SerAsBytes {
    type TvlTag: DeserializationTag;

    fn deserialize(tag: Self::Tag) -> Self::TvlTag;
}

pub trait SerializationTag {}

pub  trait DeserializationTag {}