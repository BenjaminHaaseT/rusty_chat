
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

pub enum Response {}

type ResponseEncodeTag = [u8; 1];

impl SerializationTag for ResponseEncodeTag {}

type ResponseDecodeTag = (u8, u8);

impl DeserializationTag for ResponseDecodeTag {}

impl SerAsBytes for Response {
    type Tag = ResponseEncodeTag;

    fn serialize(&self) -> Self::Tag {
        todo!()
    }
}

impl DeserAsBytes for Response {
    type TvlTag = ResponseDecodeTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag {
        todo!()
    }
}

impl AsBytes for Response {
    fn as_bytes(&self) -> Vec<u8> {
        todo!()
    }
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
#[derive(Debug, Clone, PartialEq)]
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

impl Eq for Frame {}

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
        if type_byte & 1 != 0 {
            Ok(Frame::Quit)
        } else if type_byte & 2 != 0 {
            let mut chatroom_name_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut chatroom_name_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Join {chatroom_name: String::from_utf8(chatroom_name_bytes).map_err(|_| "unable to parse chatroom name as valid utf8")?})
        } else if type_byte & 4 != 0 {
            let mut chatroom_name_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut chatroom_name_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Create {chatroom_name: String::from_utf8(chatroom_name_bytes).map_err(|_| "unable to parse chatroom name as valid utf8")?})
        } else if type_byte & 8 != 0 {
            let mut new_username_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut new_username_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Username {new_username: String::from_utf8(new_username_bytes).map_err(|_| "unable to parse new username as valid utf8")?})
        } else if type_byte & 16 != 0 {
            let mut message_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut message_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Message {message: String::from_utf8(message_bytes).map_err(|_| "unable to parse message as valid utf8")?})
        } else {
            Err("invalid type of frame received")
        }
    }

    // TODO: maybe
    // pub fn is_quit(&self) -> bool {
    //
    // }
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

impl AsBytes for Frame {
    fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend_from_slice(&self.serialize());
        match self {
            Frame::Quit => {},
            Frame::Join {chatroom_name} => bytes.extend_from_slice(chatroom_name.as_bytes()),
            Frame::Create {chatroom_name} => bytes.extend_from_slice(chatroom_name.as_bytes()),
            Frame::Username {new_username} => bytes.extend_from_slice(new_username.as_bytes()),
            Frame::Message {message} => bytes.extend_from_slice(message.as_bytes()),
        }
        bytes
    }
}

impl DeserAsBytes for Frame {
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

pub trait DeserAsBytes: SerAsBytes {
    type TvlTag: DeserializationTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag;
}

pub trait AsBytes: SerAsBytes + DeserAsBytes {
    fn as_bytes(&self) -> Vec<u8>;
}

pub trait SerializationTag {}

pub  trait DeserializationTag {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_serialize_frame() {
        let frame = Frame::Quit;
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [1, 0, 0, 0, 0]);

        let frame = Frame::Join { chatroom_name: String::from("Test Chatroom 1") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [2, 15, 0, 0, 0]);

        let frame = Frame::Create { chatroom_name: String::from("Chatroom 1") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [4, 10, 0, 0, 0]);

        let frame = Frame::Username { new_username: String::from("My new username") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [8, 15, 0, 0, 0]);

        let frame = Frame::Message { message: String::from("My test message") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [16, 15, 0, 0, 0]);
    }

    #[test]
    fn test_deserialize_frame() {
        let frame = Frame::Quit;
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 1);
        assert_eq!(length, 0);

        let frame = Frame::Join { chatroom_name: String::from("Test Chatroom 1") };
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 2);
        assert_eq!(length, 15);

        let frame = Frame::Create { chatroom_name: String::from("Chatroom 1") };
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 4);
        assert_eq!(length, 10);

        let frame = Frame::Username { new_username: String::from("My new username") };
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 8);
        assert_eq!(length, 15);

        let frame = Frame::Message { message: String::from("My test message") };
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 16);
        assert_eq!(length, 15);
    }

    #[test]
    fn test_frame_as_bytes() {
        let frame = Frame::Quit;
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        assert_eq!(frame_bytes, vec![1, 0, 0, 0, 0]);

        let frame = Frame::Join { chatroom_name: String::from("Test Chatroom 1") };
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        let mut res_bytes = vec![2, 15, 0, 0, 0];
        res_bytes.extend_from_slice("Test Chatroom 1".as_bytes());
        assert_eq!(frame_bytes, res_bytes);

        let frame = Frame::Create { chatroom_name: String::from("Chatroom 1") };
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        let mut res_bytes = vec![4, 10, 0, 0, 0];
        res_bytes.extend_from_slice("Chatroom 1".as_bytes());
        assert_eq!(frame_bytes, res_bytes);

        let frame = Frame::Username { new_username: String::from("My new username") };
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        let mut res_bytes = vec![8, 15, 0, 0, 0];
        res_bytes.extend_from_slice("My new username".as_bytes());
        assert_eq!(frame_bytes, res_bytes);

        let frame = Frame::Message { message: String::from("My test message") };
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        let mut res_bytes = vec![16, 15, 0, 0, 0];
        res_bytes.extend_from_slice("My test message".as_bytes());
        assert_eq!(frame_bytes, res_bytes);
    }

    #[test]
    fn test_frame_try_parse() {
        use async_std::io::Cursor;
        use async_std::task::block_on;

        // Test quit
        let frame = Frame::Quit;
        let mut tag = [0u8; 5];
        let mut input_stream = Cursor::new(frame.as_bytes());

        assert!(block_on(input_stream.read_exact(&mut tag)).is_ok());

        let parsed_frame_res = block_on(Frame::try_parse(&tag, &mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test join
        let frame = Frame::Join {chatroom_name: String::from("Testing testing... Chatroom good name")};
        let mut tag = [0u8; 5];
        let mut input_stream = Cursor::new(frame.as_bytes());

        assert!(block_on(input_stream.read_exact(&mut tag)).is_ok());

        let parsed_frame_res = block_on(Frame::try_parse(&tag, &mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test create
        let frame = Frame::Create {chatroom_name: String::from("Testing testing... another testing Chatroom good name")};
        let mut tag = [0u8; 5];
        let mut input_stream = Cursor::new(frame.as_bytes());

        assert!(block_on(input_stream.read_exact(&mut tag)).is_ok());

        let parsed_frame_res = block_on(Frame::try_parse(&tag, &mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test username
        let frame = Frame::Username {new_username: String::from("A good testing username")};
        let mut tag = [0u8; 5];
        let mut input_stream = Cursor::new(frame.as_bytes());

        assert!(block_on(input_stream.read_exact(&mut tag)).is_ok());

        let parsed_frame_res = block_on(Frame::try_parse(&tag, &mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test message
        let frame = Frame::Message {message: String::from("Testing testing... another test message")};
        let mut tag = [0u8; 5];
        let mut input_stream = Cursor::new(frame.as_bytes());

        assert!(block_on(input_stream.read_exact(&mut tag)).is_ok());

        let parsed_frame_res = block_on(Frame::try_parse(&tag, &mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);
    }
}