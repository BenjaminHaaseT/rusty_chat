
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


/// An enum that represents all possible responses that can be sent from the server back to the client
///
/// The `Response` is generated by both the main broker and the chatroom broker tasks, in response
/// to `Event`s triggered by client input. `Response` is used by each clients writing task to
/// decide what logic to execute to update the state of the writing task, as well as informing the client.
#[derive(Debug, Clone)]
pub enum Response {
    /// A response informing the client they have successfully connected to the chatroom lobby
    ConnectionOk,

    /// A response informing the client they have successfully subscribed to a chatroom
    Subscribed { chatroom_name: String },

    /// A response informing the client they have successfully created/subscribed to, a new chatroom
    ChatroomCreated { chatroom_name: String },

    /// A response informing the client the chatroom they attempted to create already exists
    ChatroomAlreadyExists {chatroom_name: String, lobby_state: Vec<u8>},

    /// A response informing the client that the chatroom they attempt to join does not exist
    ChatroomDoesNotExist { chatroom_name: String, lobby_state: Vec<u8> },

    /// A response informing the client the chatroom they attempted to join is full and cannot
    /// be joined.
    ChatroomFull {chatroom_name: String, lobby_state: Vec<u8>},

    /// A response that sends a message to the client from the chatroom
    Message { peer_id: Uuid, username: String, msg: String },

    /// A response informing the client they have successfully created a valid username
    UsernameOk { username: String, lobby_state: Vec<u8> },

    /// A response informing the client that the username they entered already exists
    UsernameAlreadyExists { username: String },

    /// A response informing the client they have successfully exited the chatroom
    Exit { chatroom_name: String, lobby_state: Vec<u8>},

}

impl Response {
    pub async fn try_parse<R: AsyncReadExt + Unpin>(tag: &FrameEncodeTag, mut input_reader: R) -> Result<Self, &'static str> {
        todo!()
    }
    fn serialize_length(tag: &mut ResponseEncodeTag, idx: usize, length: u32) {
        for i in idx ..idx + 4 {
            tag[i] ^= (length >> ((i - idx) * 8)) as u8;
        }
    }

    fn deserialize_length(tag: &ResponseEncodeTag, idx: usize, length: &mut u32) {
        for i in idx ..idx + 4 {
            *length ^= (tag[i] as u32) << ((i - idx) * 8)
        }
    }
}

unsafe impl Send for Response {}

/// The type-length-value tag type for serializing a `Response`
type ResponseEncodeTag = [u8; 25];

impl SerializationTag for ResponseEncodeTag {}

/// The type-length-value type for deserializing a `Response
type ResponseDecodeTag = (u8, u128, u32, u32);

impl DeserializationTag for ResponseDecodeTag {}

impl SerAsBytes for Response {
    type Tag = ResponseEncodeTag;

    fn serialize(&self) -> Self::Tag {
        let mut bytes = [0u8; 25];
        match self {
            Response::ConnectionOk => bytes[0] ^= 1,
            Response::Subscribed {chatroom_name} => {
                bytes[0] ^= 2;
                Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
            }
            Response::ChatroomCreated {chatroom_name} => {
                bytes[0] ^= 3;
                Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
            }
            Response::ChatroomAlreadyExists {chatroom_name, lobby_state} => {
                bytes[0] ^= 4;
                Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
                Response::serialize_length(&mut bytes, 5, lobby_state.len() as u32);
            }
            Response::ChatroomDoesNotExist {chatroom_name, lobby_state} => {
                bytes[0] ^= 5;
                Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
                Response::serialize_length(&mut bytes, 5, lobby_state.len() as u32);
            }
            Response::ChatroomFull {chatroom_name, lobby_state} => {
                bytes[0] ^= 6;
                Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
                Response::serialize_length(&mut bytes, 5, lobby_state.len() as u32);
            }
            Response::Message {peer_id, username, msg} => {
                bytes[0] ^= 7;
                let peer_id_bytes = peer_id.as_bytes();
                bytes[1..17].copy_from_slice(peer_id_bytes);
                Response::serialize_length(&mut bytes, 17, username.len() as u32);
                Response::serialize_length(&mut bytes, 21, msg.len() as u32);
            }
            Response::UsernameOk {username, lobby_state} => {
                bytes[0] ^= 8;
                Response::serialize_length(&mut bytes, 1, username.len() as u32);
                Response::serialize_length(&mut bytes, 5, lobby_state.len() as u32);
            }
            Response::UsernameAlreadyExists {username} => {
                bytes[0] ^= 9;
                Response::serialize_length(&mut bytes, 1, username.len() as u32);
            }
            Response::Exit {chatroom_name, lobby_state} => {
                bytes[0] ^= 10;
                Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
                Response::serialize_length(&mut bytes, 5, lobby_state.len() as u32);
            }
        }
        bytes
    }
}

impl DeserAsBytes for Response {
    type TvlTag = ResponseDecodeTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag {
        let type_byte = tag[0];
        let mut name_len = 0;
        let mut data_len = 0;
        if type_byte ^ 1 == 0 {
            (1, 0u128, 0u32, 0u32)
        } else if type_byte ^ 2 == 0 {
            Response::deserialize_length(tag, 1,&mut name_len);
            (2, 0, name_len, 0)
        } else if type_byte ^ 3 == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            (3, 0, name_len, 0)
        } else if type_byte ^ 4 == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            Response::deserialize_length(tag, 5, &mut data_len);
            (4, 0, name_len, data_len)
        } else if type_byte ^ 5 == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            Response::deserialize_length(tag, 5, &mut data_len);
            (5, 0, name_len, data_len)
        } else if type_byte ^ 6 == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            Response::deserialize_length(tag, 5, &mut data_len);
            (6, 0, name_len, data_len)
        } else if type_byte ^ 7 == 0 {
            let mut id_bytes = [0u8; 16];
            id_bytes.copy_from_slice(&tag[1..17]);
            let id = Uuid::from_bytes(id_bytes).as_u128();
            Response::deserialize_length(tag, 17, &mut name_len);
            Response::deserialize_length(tag, 21, &mut data_len);
            (7, id, name_len, data_len)
        } else if type_byte ^ 8 == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            Response::deserialize_length(tag, 5, &mut data_len);
            (8, 0, name_len, data_len)
        } else if type_byte ^ 9 == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            (9, 0, name_len, 0)
        } else if type_byte ^ 10  == 0 {
            Response::deserialize_length(tag, 1, &mut name_len);
            Response::deserialize_length(tag, 5, &mut data_len);
            (10, 0, name_len, data_len)
        } else {
            panic!("invalid type flag detected")
        }
    }
}

impl AsBytes for Response {
    fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend_from_slice(&self.serialize());
        match self {
            Response::ConnectionOk => {},
            Response::Subscribed {chatroom_name} => bytes.extend_from_slice(chatroom_name.as_bytes()),
            Response::ChatroomCreated {chatroom_name} => bytes.extend_from_slice(chatroom_name.as_bytes()),
            Response::ChatroomAlreadyExists {chatroom_name, lobby_state} => {
                bytes.extend_from_slice(chatroom_name.as_bytes());
                bytes.extend_from_slice(lobby_state.as_slice());
            }
            Response::ChatroomDoesNotExist {chatroom_name, lobby_state} => {
                bytes.extend_from_slice(chatroom_name.as_bytes());
                bytes.extend_from_slice(lobby_state.as_slice());
            }
            Response::ChatroomFull {chatroom_name, lobby_state} => {
                bytes.extend_from_slice(chatroom_name.as_bytes());
                bytes.extend_from_slice(lobby_state.as_slice());
            }
            Response::Message {peer_id, username,msg} => {
                bytes.extend_from_slice(username.as_bytes());
                bytes.extend_from_slice(msg.as_bytes());
            }
            Response::UsernameOk {username, lobby_state} => {
                bytes.extend_from_slice(username.as_bytes());
                bytes.extend_from_slice(lobby_state.as_slice());
            }
            Response::UsernameAlreadyExists {username} => {
                bytes.extend_from_slice(username.as_bytes());
            }
            Response::Exit {chatroom_name, lobby_state} => {
                bytes.extend_from_slice(chatroom_name.as_bytes());
                bytes.extend_from_slice(lobby_state.as_slice());
            }
        }
        bytes
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

    #[test]
    fn test_serialize_response() {
        // Test ConnectionOk
        let response = Response::ConnectionOk;
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Subscribed
        let response = Response::Subscribed { chatroom_name: String::from("Test Chatroom") };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [2, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomCreated
        let response = Response::ChatroomCreated { chatroom_name: String::from("Test Chatroom 42") };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [3, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomAlreadyExists
        let response = Response::ChatroomAlreadyExists { chatroom_name: String::from("Test Chatroom 43"), lobby_state: vec![38, 23, 1, 1, 0] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [4, 16, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomDoesNotExist
        let response = Response::ChatroomDoesNotExist { chatroom_name: String::from("Test Chatroom 43"), lobby_state: vec![38, 23, 1, 1, 0] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [5, 16, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomFull
        let response = Response::ChatroomFull { chatroom_name: String::from("Test Chatroom 44"), lobby_state: vec![38, 23, 1, 1, 0, 0, 0, 0, 34] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [6, 16, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Message
        let peer_id = Uuid::new_v4();
        let response = Response::Message { peer_id, username: String::from("Good Username"), msg: String::from("Test message") };
        let tag = response.serialize();
        println!("{:?}", tag);
        let mut expected_tag = [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_tag[1..17].copy_from_slice(peer_id.as_bytes());
        expected_tag[17] = 13;
        expected_tag[21] = 12;
        assert_eq!(tag, expected_tag);

        // Test UsernameOk
        let response = Response::UsernameOk {username: String::from("Good test username"), lobby_state: vec![0, 0, 0] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [8, 18, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test UsernameAlreadyExists
        let response = Response::UsernameAlreadyExists {username: String::from("Good test username 42")};
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [9, 21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Exit
        let response = Response::Exit {chatroom_name: String::from("Perfect chatroom name"), lobby_state: vec![42, 42, 42, 42, 1, 0]};
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [10, 21, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn test_deserialize_response() {
        // Test ConnectionOk
        let response = Response::ConnectionOk;
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 1);
        assert_eq!(id, 0);
        assert_eq!(name_len, 0);
        assert_eq!(data_len, 0);

        // Test Subscribed
        let response = Response::Subscribed {chatroom_name: String::from("Good test chatroom name")};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 2);
        assert_eq!(id, 0);
        assert_eq!(name_len, 23);
        assert_eq!(data_len, 0);

        // Test ChatroomCreated
        let response = Response::ChatroomCreated {chatroom_name: String::from("Good test chatroom name 42")};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 3);
        assert_eq!(id, 0);
        assert_eq!(name_len, 26);
        assert_eq!(data_len, 0);

        // Test ChatroomCreated
        let response = Response::ChatroomAlreadyExists {chatroom_name: String::from("Good test chatroom name 143"), lobby_state: vec![1, 2, 3, 4]};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 4);
        assert_eq!(id, 0);
        assert_eq!(name_len, 27);
        assert_eq!(data_len, 4);

        // Test ChatroomDoesNotExist
        let response = Response::ChatroomDoesNotExist {chatroom_name: String::from("Good test chatroom"), lobby_state: vec![1, 2, 3, 4, 5]};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 5);
        assert_eq!(id, 0);
        assert_eq!(name_len, 18);
        assert_eq!(data_len, 5);

        // Test ChatroomFull
        let response = Response::ChatroomFull {chatroom_name: String::from("A Good full test chatroom"), lobby_state: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 6);
        assert_eq!(id, 0);
        assert_eq!(name_len, 25);
        assert_eq!(data_len, 11);

        // Test Message
        let peer_id = Uuid::new_v4();
        let response = Response::Message {peer_id, username: String::from("A good test username"), msg: String::from("A good test message") };
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 7);
        assert_eq!(id, peer_id.as_u128());
        assert_eq!(name_len, 20);
        assert_eq!(data_len, 19);

        // Test UsernameOk
        let response = Response::UsernameOk {username: String::from("Test Username 42"), lobby_state: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 8);
        assert_eq!(id, 0);
        assert_eq!(name_len, 16);
        assert_eq!(data_len, 11);

        // Test UsernameAlreadyExists
        let response = Response::UsernameAlreadyExists {username: String::from("Bad Test Username 42")};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 9);
        assert_eq!(id, 0);
        assert_eq!(name_len, 20);
        assert_eq!(data_len, 0);

        // Test Exit
        let response = Response::Exit {chatroom_name: String::from("Decent test chatroom name"), lobby_state: vec![0, 1, 2, 3, 4, 5]};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 10);
        assert_eq!(id, 0);
        assert_eq!(name_len, 25);
        assert_eq!(data_len, 6);
    }

    #[test]
    fn test_response_as_bytes() {
        // Test ConnectionOk
        let response = Response::ConnectionOk;
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        assert_eq!(response_bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Subscribed
        let response = Response::Subscribed { chatroom_name: String::from("Test Chatroom") };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![2, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomCreated
        let response = Response::ChatroomCreated { chatroom_name: String::from("Test Chatroom 42") };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![3, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom 42").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomAlreadyExists
        let response = Response::ChatroomAlreadyExists { chatroom_name: String::from("Test Chatroom 43"), lobby_state: vec![97, 98, 99] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![4, 16, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom 43").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99]);
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomDoesNotExist
        let response = Response::ChatroomDoesNotExist { chatroom_name: String::from("Test Chatroom 44"), lobby_state: vec![97, 98, 99, 100] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![5, 16, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom 44").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99, 100]);
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomFull
        let response = Response::ChatroomFull { chatroom_name: String::from("Another Test Chatroom 45"), lobby_state: vec![97, 98, 99, 100, 101, 103] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![6, 24, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Another Test Chatroom 45").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99, 100, 101, 103]);
        assert_eq!(response_bytes, expected_bytes);

        // Test Message
        let peer_id = Uuid::new_v4();
        let response = Response::Message { peer_id, username: String::from("A good test username"), msg: String::from("Hello World!")};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![0; 25];
        expected_bytes[0] = 7;
        expected_bytes[1..17].copy_from_slice(peer_id.as_bytes());
        expected_bytes[17] = 20;
        expected_bytes[21] = 12;
        expected_bytes.extend_from_slice(String::from("A good test username").as_bytes());
        expected_bytes.extend_from_slice(String::from("Hello World!").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test UsernameOk
        let response = Response::UsernameOk { username: String::from("My test username"), lobby_state: vec![97, 98, 99, 100, 101, 103] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![8, 16, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("My test username").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99, 100, 101, 103]);
        assert_eq!(response_bytes, expected_bytes);

        // Test UsernameAlreadyExists
        let response = Response::UsernameAlreadyExists { username: String::from("My bad test username")};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![9, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("My bad test username").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test Exit
        let response = Response::Exit { chatroom_name: String::from("Test Exit chatroom name"), lobby_state: vec![97, 97, 98, 99]};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![10, 23, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Exit chatroom name").as_bytes());
        expected_bytes.extend_from_slice(&[97, 97, 98, 99]);
        assert_eq!(response_bytes, expected_bytes);
    }
}