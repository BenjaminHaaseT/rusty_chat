
use uuid::Uuid;

use std::collections::HashMap;
use std::sync::Arc;
use async_std::net;
pub use async_std::channel::{Receiver as AsyncStdReceiver, Sender as AsyncStdSender};
pub use tokio::sync::broadcast::{Sender as TokioBroadcastSender, Receiver as TokioBroadcastReceiver};
use futures::AsyncReadExt;
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

#[derive(Debug)]
pub enum Event {
    Quit {peer_id: Uuid},
    Join {
        chatroom_name: String,
        peer_id: Uuid
    },
    Create {
        chatroom_name: String,
        peer_id: Uuid
    },
    Username {
        new_username: String,
        peer_id: Uuid
    },
    NewClient {
        stream: Arc<net::TcpStream>,
        shutdown: AsyncStdReceiver<Null>,
        chatroom_connection: AsyncStdSender<AsyncStdSender<Event>>,
        peer_id: Uuid,
    },
    Message {
        message: String,
        peer_id: Uuid,
    }
}


// TODO: implement parsing to and from bytes for message events
#[derive(Clone)]
pub enum Frame {
    Quit,

    Join {
        chatroom_name: String,
    },

    Create {
        chatroom_name: String,
    },

    Username {
        new_username: String,
    },

    Message {
        message: String,
    }
}

impl Frame {
    fn serialize_len(tag: &mut FrameEncodeTag, idx: usize, length: u32) {
        for i in idx ..idx + 4 {
            tag[i] ^= ((length >> ((i - idx) * 8)) & 0xff) as u8;
        }
    }

    fn deserialize_len(tag: &FrameEncodeTag, idx: usize, length: &mut u32) {
        for i in idx .. idx + 4 {
            *length ^= (tag[i] as u32) << ((i - idx) * 8);
        }
    }

    pub async fn try_parse<R: AsyncReadExt + Unpin>(tag: &FrameEncodeTag, mut input_reader: R) -> Result<Self, &'static str> {
        let (type_byte, length) = Frame::deserialize(tag);
        if type_byte ^ 1 != 0 {
            Ok(Frame::Quit)
        } else if type_byte ^ 2 != 0 {
            let mut chatroom_name_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut chatroom_name_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Join {chatroom_name: String::from_utf8(chatroom_name_bytes).map_err(|_| "unable to parse chatroom name as valid utf8")?})
        } else if type_byte ^ 4 != 0 {
            let mut chatroom_name_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut chatroom_name_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Create {chatroom_name: String::from_utf8(chatroom_name_bytes).map_err(|_| "unable to parse chatroom name as valid utf8")?})
        } else if type_byte ^ 8 != 0 {
            let mut new_username_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut new_username_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Username {new_username: String::from_utf8(new_username_bytes).map_err(|_| "unable to parse new username as valid utf8")?})
        } else if type_byte ^ 16 != 0 {
            let mut message_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut message_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Message {message: String::from_utf8(message_bytes).map_err(|_| "unable to parse message as valid utf8")?})
        } else {
            Err("invalid type of frame received")
        }
    }
}

pub type FrameEncodeTag = [u8; 5];

impl SerializationTag for FrameEncodeTag {}

pub type FrameDecodeTag = (u8, u32);

impl DeserializationTag for FrameDecodeTag {}

impl SerAsBytes for Frame {
    type Tag = FrameEncodeTag;

    fn serialize(&self) -> Self::Tag {
        let mut tag = [0u8; 5];

        match self {
            Frame::Quit => tag[0] ^= 1,
            Frame::Join {chatroom_name} => {
                tag[0] ^= 1 << 1;
                Frame::serialize_len(&mut tag, 1, chatroom_name.len() as u32);
            }
            Frame::Create {chatroom_name} => {
                tag[0] ^= 1 << 2;
                Frame::serialize_len(&mut tag, 1, chatroom_name.len() as u32);
            }
            Frame::Username {new_username} => {
                tag[0] ^= 1 << 3;
                Frame::serialize_len(&mut tag, 1,new_username.len() as u32);
            }
            Frame::Message { message} => {
                tag[0] ^= 1 << 4;
                Frame::serialize_len(&mut tag, 1, message.len() as u32);
            }
        }

        tag
    }
}

impl DeSerAsBytes for Frame {
    type TvlTag = FrameDecodeTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag {
        if tag[0] & 1 != 0 {
            (1, 0)
        } else if tag[0] & 2 != 0 {
            let mut chatroom_name_len = 0u32;
            Frame::deserialize_len(&tag, 1, &mut chatroom_name_len);
            (2, chatroom_name_len)
        } else if tag[0] & 4 != 0 {
            let mut chatroom_name_len = 0u32;
            Frame::deserialize_len(&tag, 1, &mut chatroom_name_len);
            (4, chatroom_name_len)
        } else if tag[0] & 8 != 0 {
            let mut username_len = 0;
            Frame::deserialize_len(&tag,1, &mut username_len);
            (8, username_len)
        } else if tag[0] & 16 != 0 {
            let mut message_len = 0;
            Frame::deserialize_len(&tag, 1, &mut message_len);
            (16, message_len)
        } else {
            panic!("invalid type byte detected, unable to deserialize 'Frame' tag")
        }
    }
}


pub struct Client {
    pub username: String,
    pub id: Uuid,
    pub stream: Option<net::TcpStream>
}

pub enum Null {}

pub trait SerAsBytes {
    type Tag: SerializationTag;
    fn serialize(&self) -> Self::Tag;
}

pub trait DeSerAsBytes: SerAsBytes {
    type TvlTag: DeserializationTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag;
}

pub trait SerializationTag {}

pub  trait DeserializationTag {}