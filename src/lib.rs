
use uuid::Uuid;

use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::{Read, Cursor};
use std::sync::Arc;
use async_std::net;
pub use async_std::channel::{Receiver as AsyncStdReceiver, Sender as AsyncStdSender};
pub use tokio::sync::broadcast::{Sender as TokioBroadcastSender, Receiver as TokioBroadcastReceiver};
use futures::AsyncReadExt;
// pub mod room;

pub mod prelude {
    pub use super::*;
}

/// Models a single chatroom that is hosted by the server.
#[derive(Debug)]
pub struct Chatroom {
    /// The unique id of the chatroom.
    pub id: Uuid,

    /// The name of the chatroom.
    pub name: Arc<String>,

    /// A sending half of the broadcast channel that the chatroom-sub-broker task
    /// will use to broadcast messages to all subscribing clients. Used for creating new subscriptions,
    /// whenever a new client wants to join.
    pub client_subscriber: TokioBroadcastSender<Response>,

    /// The sending half of the channel that can be cloned and sent to any client's read task,
    /// that way new client's can take a clone of the sending half of this channel and start sending events.
    pub client_read_sender: AsyncStdSender<Event>,

    /// The sending half of a channel used for synchronizing shutdown ie, dropping this channel (when it is 'Some')
    /// will initiate a graceful shutdown procedure for the current chatroom-sub-broker task. May or may not be set.
    pub shutdown: Option<AsyncStdSender<Null>>,

    /// The capacity of the chatroom ie number of clients it can serve.
    pub capacity: usize,

    /// The current number of clients connected with the chatroom.
    pub num_clients: usize,
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

type ChatroomEncodeTag = [u8; 12];

impl SerializationTag for ChatroomEncodeTag {}

type ChatroomDecodeTag = (u32, u32, u32);

impl DeserializationTag for ChatroomDecodeTag {}

impl SerAsBytes for Chatroom {
    type Tag = ChatroomEncodeTag;

    fn serialize(&self) -> Self::Tag {
        let mut tag = [0u8; 12];
        self.serialize_name_length(&mut tag);
        self.serialize_capacity(&mut tag);
        self.serialize_num_clients(&mut tag);
        tag
    }
}

impl DeserAsBytes for Chatroom {
    type TvlTag = ChatroomDecodeTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag {
        // let inner = tag;
        let mut name_len = 0;
        for i in 0..4 {
            name_len ^= (tag[i] as u32) << (i * 8);
        }
        let mut capacity = 0;
        for i in 4..8 {
            capacity ^= (tag[i] as u32) << ((i % 4) * 8);
        }
        let mut num_clients = 0;
        for i in 8..12 {
            num_clients ^= (tag[i] as u32) << ((i % 4) * 8);
        }
        (name_len, capacity, num_clients)
    }
}

impl AsBytes for Chatroom {
    fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = self.serialize().to_vec();
        bytes.extend_from_slice(self.name.as_bytes());
        bytes
    }
}

#[derive(Debug)]
pub struct ChatroomFrame {
    pub name: String,
    pub capacity: usize,
    pub num_clients: usize,
}

impl ChatroomFrame {
    pub fn try_parse<R: Read>(mut reader: R) -> Result<ChatroomFrame, &'static str> {
        let mut tag = [0u8; 12];
        reader.read_exact(&mut tag)
            .map_err(|_| "error reading 'ChatroomFrame' tag from reader")?;
        let (name_len, capacity, num_clients) = <Chatroom as DeserAsBytes>::deserialize(&tag);
        let mut name_bytes = vec![0; name_len as usize];
        reader.read_exact(name_bytes.as_mut_slice())
            .map_err(|_| "error reading name bytes for 'ChatroomFrame' from reader")?;
        let name = String::from_utf8(name_bytes)
            .map_err(|_| "error parsing 'ChatroomFrame' name bytes as valid utf")?;

        Ok(ChatroomFrame { name, capacity: capacity as usize, num_clients: num_clients as usize} )
    }
}

#[derive(Debug)]
pub struct ChatroomFrames {
    pub frames: Vec<ChatroomFrame>,
}

impl TryFrom<Vec<u8>> for ChatroomFrames {
    type Error = &'static str;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let len = bytes.len() as u64;
        let mut cursor = std::io::Cursor::new(bytes);
        let mut frames = vec![];
        loop {
            let frame = ChatroomFrame::try_parse(&mut cursor)?;
            frames.push(frame);
            if cursor.position() == len {
                break;
            }
        }
        Ok(ChatroomFrames { frames })
    }
}

/// An enum that represents all possible responses that can be sent from the server back to the client
///
/// The `Response` is generated by both the main broker and the chatroom broker tasks, in response
/// to `Event`s triggered by client input. `Response` is used by each clients writing task to
/// decide what logic to execute to update the state of the writing task, as well as informing the client.
#[derive(Debug, Clone, PartialEq)]
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
    Message { peer_id: Uuid, msg: String },

    /// A response informing the client they have successfully created a valid username
    UsernameOk { username: String, lobby_state: Vec<u8> },

    /// A response informing the client that the username they entered already exists
    UsernameAlreadyExists { username: String },

    /// A response informing the client they have successfully exited the chatroom
    ExitChatroom { chatroom_name: String},

    /// A response informing the client they have rejoined the lobby
    Lobby {lobby_state: Vec<u8>},

    /// A response informing the client they have exited the lobby
    ExitLobby,

    /// A response informing the client that their read-task is synced a new chatroom-sub-broker
    ReadSync,
}

impl Response {
    /// Attempts to parse a `Response` from the `input_reader`.
    ///
    /// #Panics
    /// If an invalid type flag is read form `input_reader`, the method will panic.
    pub async fn try_parse<R: AsyncReadExt + Unpin>(mut input_reader: R) -> Result<Self, &'static str> {
        // Create tag, and attempt to read from reader
        let mut tag = [0u8; 22];
        input_reader.read_exact(&mut tag)
            .await
            .map_err(|_| "unable to read tag from reader")?;

        // Deserialize tag
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);

        if type_byte ^ 1 == 0 {
            Ok(Response::ConnectionOk)

        } else if type_byte ^ 2 == 0 {
            let mut chatroom_name_bytes = vec![0; name_len as usize];
            input_reader.read_exact(chatroom_name_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read chatroom name bytes from reader")?;
            let chatroom_name = String::from_utf8(chatroom_name_bytes)
                .map_err(|_| "unable to parse chatroom name as valid utf8")?;
            Ok(Response::Subscribed {chatroom_name})

        } else if type_byte ^ 3 == 0 {
            let mut chatroom_name_bytes = vec![0; name_len as usize];
            input_reader.read_exact(chatroom_name_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read chatroom name bytes from reader")?;
            let chatroom_name = String::from_utf8(chatroom_name_bytes)
                .map_err(|_| "unable to parse chatroom name as valid utf8")?;
            Ok(Response::ChatroomCreated {chatroom_name})

        } else if type_byte ^ 4 == 0 {
            let mut chatroom_name_bytes = vec![0; name_len as usize];
            let mut lobby_state_bytes = vec![0; data_len as usize];
            input_reader.read_exact(chatroom_name_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read chatroom name bytes from reader")?;
            let chatroom_name = String::from_utf8(chatroom_name_bytes)
                .map_err(|_| "unable to parse chatroom name as valid utf8")?;
            input_reader.read_exact(lobby_state_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read lobby state bytes from reader")?;
            Ok(Response::ChatroomAlreadyExists {chatroom_name, lobby_state: lobby_state_bytes})

        } else if type_byte ^ 5 == 0 {
            let mut chatroom_name_bytes = vec![0; name_len as usize];
            let mut lobby_state_bytes = vec![0; data_len as usize];
            input_reader.read_exact(chatroom_name_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read chatroom name bytes from reader")?;
            let chatroom_name = String::from_utf8(chatroom_name_bytes)
                .map_err(|_| "unable to parse chatroom name as valid utf8")?;
            input_reader.read_exact(lobby_state_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read lobby state bytes from reader")?;
            Ok(Response::ChatroomDoesNotExist {chatroom_name, lobby_state: lobby_state_bytes})

        } else if type_byte ^ 6 == 0 {
            let mut chatroom_name_bytes = vec![0; name_len as usize];
            let mut lobby_state_bytes = vec![0; data_len as usize];
            input_reader.read_exact(chatroom_name_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read chatroom name bytes from reader")?;
            let chatroom_name = String::from_utf8(chatroom_name_bytes)
                .map_err(|_| "unable to parse chatroom name as valid utf8")?;
            input_reader.read_exact(lobby_state_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read lobby state bytes from reader")?;
            Ok(Response::ChatroomFull {chatroom_name, lobby_state: lobby_state_bytes})

        } else if type_byte ^ 7 == 0 {
            // let mut username_bytes = vec![0; name_len as usize];
            let mut msg_bytes = vec![0; data_len as usize];
            // input_reader.read_exact(username_bytes.as_mut_slice())
            //     .await
            //     .map_err(|_| "unable to read username bytes from reader")?;
            input_reader.read_exact(msg_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read message bytes from reader")?;
            // let username = String::from_utf8(username_bytes)
            //     .map_err(|_| "unable to parse username bytes as valid utf8")?;
            let msg = String::from_utf8(msg_bytes)
                .map_err(|_| "unable to parse message as valid utf8")?;
            Ok(Response::Message {peer_id: Uuid::from_u128(id), msg})

        } else if type_byte ^ 8 == 0 {
            let mut username_bytes = vec![0; name_len as usize];
            let mut lobby_state_bytes = vec![0; data_len as usize];
            input_reader.read_exact(username_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read username bytes from reader")?;
            input_reader.read_exact(lobby_state_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read lobby state bytes from reader")?;
            let username = String::from_utf8(username_bytes)
                .map_err(|_| "unable to parse username bytes as valid utf8")?;

            Ok(Response::UsernameOk {username, lobby_state: lobby_state_bytes})
        } else if type_byte ^ 9 == 0 {
            let mut username_bytes = vec![0; name_len as usize];
            input_reader.read_exact(username_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read username bytes from reader")?;
            let username = String::from_utf8(username_bytes)
                .map_err(|_| "unable to parse username bytes as valid utf8")?;
            Ok(Response::UsernameAlreadyExists {username})

        } else if type_byte ^ 10 == 0 {
            let mut chatroom_name_bytes = vec![0; name_len as usize];
            input_reader.read_exact(chatroom_name_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read chatroom name bytes from reader")?;
            let chatroom_name = String::from_utf8(chatroom_name_bytes)
                .map_err(|_| "unable to parse chatroom name bytes as valid utf8")?;

            Ok(Response::ExitChatroom{chatroom_name})
        } else if type_byte ^ 11 == 0 {
            let mut lobby_state_bytes = vec![0; data_len as usize];
            input_reader.read_exact(lobby_state_bytes.as_mut_slice())
                .await
                .map_err(|_| "unable to read lobby state bytes from reader")?;
            Ok(Response::Lobby {lobby_state: lobby_state_bytes})
        } else if type_byte ^ 12 == 0 {
            Ok(Response::ExitLobby)
        } else if type_byte ^ 13 == 0 {
            Ok(Response::ReadSync)
        } else {
            panic!("invalid type byte detected");
        }
    }

    pub fn is_exit_chatroom(&self) -> bool {
        match self {
            Response::ExitChatroom {chatroom_name} => true,
            _ => false,
        }
    }

    pub fn is_subscribed(&self) -> bool {
        match self {
            Response::Subscribed {chatroom_name} => true,
            _ => false
        }
    }

    pub fn is_chatroom_created(&self) -> bool {
        match self {
            Response::ChatroomCreated {chatroom_name} => true,
            _ => false
        }
    }

    pub fn is_message(&self) -> bool {
        match self {
            Response::Message {msg, peer_id} => true,
            _ => false
        }
    }

    pub fn is_connection_ok(&self) -> bool {
        match self {
            Response::ConnectionOk => true,
            _ => false
        }
    }

    // /// Helper method to reduce code duplication when serializing a `Response`
    // ///
    // /// Many `Response` variants have fields which have a length property that needs to be serialized.
    // /// This method provides the functionality to do this.
    // /// Takes `tag`, the tag that we are serializing the `Response` into, `idx` the starting position
    // /// from which to start serializing a length value and `length`, the value that represents the length
    // /// we wish to serialize
    // fn serialize_name_length(tag: &mut ResponseEncodeTag, idx: usize, length: u8) {
    //     for i in idx ..idx + 4 {
    //         tag[i] ^= (length >> ((i - idx) * 8)) as u8;
    //     }
    // }

    /// Helper method to reduce code duplication when serializing a `Response`
    ///
    /// Many `Response` variants have fields which have a length property that needs to be serialized.
    /// This method provides the functionality to do this.
    /// Takes `tag`, the tag that we are serializing the `Response` into, `idx` the starting position
    /// from which to start serializing a length value and `length`, the value that represents the length
    /// we wish to serialize
    fn serialize_data_length(tag: &mut ResponseEncodeTag, idx: usize, length: u32) {
        for i in idx ..idx + 4 {
            tag[i] ^= (length >> ((i - idx) * 8)) as u8;
        }
    }

    /// Helper method to reduce code duplication when deserializing a `Response` tag.
    ///
    /// Many `Response` variants have fields which have a length property that needs to be deserialized.
    /// This method provides the functionality to do this.
    /// Takes `tag`, the tag that we are deserializing `length` from, and the starting index `idx`,
    /// that is the position in the `tag` we should start deserializing bytes from.
    fn deserialize_data_length(tag: &ResponseEncodeTag, idx: usize, length: &mut u32) {
        for i in idx ..idx + 4 {
            *length ^= (tag[i] as u32) << ((i - idx) * 8)
        }
    }
}

unsafe impl Send for Response {}

/// The type-length-value tag type for serializing a `Response`
type ResponseEncodeTag = [u8; 22];

impl SerializationTag for ResponseEncodeTag {}

/// The type-length-value type for deserializing a `Response
type ResponseDecodeTag = (u8, u128, u8, u32);

impl DeserializationTag for ResponseDecodeTag {}

impl SerAsBytes for Response {
    type Tag = ResponseEncodeTag;

    fn serialize(&self) -> Self::Tag {
        let mut bytes = [0u8; 22];
        match self {
            Response::ConnectionOk => bytes[0] ^= 1,
            Response::Subscribed {chatroom_name} => {
                bytes[0] ^= 2;
                bytes[1] ^= chatroom_name.len() as u8;
                // Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
            }
            Response::ChatroomCreated {chatroom_name} => {
                bytes[0] ^= 3;
                bytes[1] ^= chatroom_name.len() as u8;
                // Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
            }
            Response::ChatroomAlreadyExists {chatroom_name, lobby_state} => {
                bytes[0] ^= 4;
                bytes[1] ^= chatroom_name.len() as u8;
                Response::serialize_data_length(&mut bytes, 2, lobby_state.len() as u32);
                // Response::serialize_length(&mut bytes, 5, lobby_state.len() as u32);
            }
            Response::ChatroomDoesNotExist {chatroom_name, lobby_state} => {
                bytes[0] ^= 5;
                bytes[1] ^= chatroom_name.len() as u8;
                // Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
                Response::serialize_data_length(&mut bytes, 2, lobby_state.len() as u32);
            }
            Response::ChatroomFull {chatroom_name, lobby_state} => {
                bytes[0] ^= 6;
                bytes[1] ^= chatroom_name.len() as u8;
                // Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
                Response::serialize_data_length(&mut bytes, 2, lobby_state.len() as u32);
            }
            Response::Message {peer_id,  msg} => {
                bytes[0] ^= 7;
                let peer_id_bytes = peer_id.as_bytes();
                bytes[1..17].copy_from_slice(peer_id_bytes);
                // Response::serialize_length(&mut bytes, 17, username.len() as u32);
                Response::serialize_data_length(&mut bytes, 17, msg.len() as u32);
            }
            Response::UsernameOk {username, lobby_state} => {
                bytes[0] ^= 8;
                bytes[1] ^= username.len() as u8;
                // Response::serialize_length(&mut bytes, 1, username.len() as u32);
                Response::serialize_data_length(&mut bytes, 2, lobby_state.len() as u32);
            }
            Response::UsernameAlreadyExists {username} => {
                bytes[0] ^= 9;
                bytes[1] ^= username.len() as u8;
                // Response::serialize_length(&mut bytes, 1, username.len() as u32);
            }
            Response::ExitChatroom {chatroom_name} => {
                bytes[0] ^= 10;
                bytes[1] ^= chatroom_name.len() as u8;
                // Response::serialize_length(&mut bytes, 1, chatroom_name.len() as u32);
            }
            Response::Lobby {lobby_state} => {
                bytes[0] ^= 11;
                Response::serialize_data_length(&mut bytes, 1, lobby_state.len() as u32);
            }
            Response::ExitLobby => bytes[0] ^= 12,
            Response::ReadSync => bytes[0] ^= 13,
        }
        bytes
    }
}

impl DeserAsBytes for Response {
    type TvlTag = ResponseDecodeTag;

    fn deserialize(tag: &Self::Tag) -> Self::TvlTag {
        let type_byte = tag[0];
        let mut name_len = 0u8;
        let mut data_len = 0;
        if type_byte ^ 1 == 0 {
            (1, 0u128, 0u8, 0u32)
        } else if type_byte ^ 2 == 0 {
            // Response::deserialize_length(tag, 1,&mut name_len);
            name_len ^= tag[1];
            (2, 0, name_len, 0)
        } else if type_byte ^ 3 == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            (3, 0, name_len, 0)
        } else if type_byte ^ 4 == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            Response::deserialize_data_length(tag, 2, &mut data_len);
            (4, 0, name_len, data_len)
        } else if type_byte ^ 5 == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            Response::deserialize_data_length(tag, 2, &mut data_len);
            (5, 0, name_len, data_len)
        } else if type_byte ^ 6 == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            Response::deserialize_data_length(tag, 2, &mut data_len);
            (6, 0, name_len, data_len)
        } else if type_byte ^ 7 == 0 {
            let mut id_bytes = [0u8; 16];
            id_bytes.copy_from_slice(&tag[1..17]);
            let id = Uuid::from_bytes(id_bytes).as_u128();
            Response::deserialize_data_length(tag, 17, &mut data_len);
            (7, id, name_len, data_len)
        } else if type_byte ^ 8 == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            Response::deserialize_data_length(tag, 2, &mut data_len);
            (8, 0, name_len, data_len)
        } else if type_byte ^ 9 == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            (9, 0, name_len, 0)
        } else if type_byte ^ 10  == 0 {
            // Response::deserialize_length(tag, 1, &mut name_len);
            name_len ^= tag[1];
            (10, 0, name_len, data_len)
        } else if type_byte ^ 11 == 0 {
            Response::deserialize_data_length(tag, 1, &mut data_len);
            (11, 0, name_len, data_len)
        } else if type_byte ^ 12 == 0 {
            (12, 0, 0, 0)
        } else if type_byte ^ 13 == 0 {
            (13, 0, 0, 0)
        } else {
            panic!("invalid type flag detected");
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
            Response::Message {peer_id,msg} => {
                // bytes.extend_from_slice(username.as_bytes());
                bytes.extend_from_slice(msg.as_bytes());
            }
            Response::UsernameOk {username, lobby_state} => {
                bytes.extend_from_slice(username.as_bytes());
                bytes.extend_from_slice(lobby_state.as_slice());
            }
            Response::UsernameAlreadyExists {username} => {
                bytes.extend_from_slice(username.as_bytes());
            }
            Response::ExitChatroom {chatroom_name} => {
                bytes.extend_from_slice(chatroom_name.as_bytes());
            }
            Response::Lobby {lobby_state} => {
                bytes.extend_from_slice(lobby_state.as_slice());
            }
            Response::ExitLobby => {}
            Response::ReadSync => {}
        }
        bytes
    }
}

#[derive(Debug)]
pub enum Event {
    Quit {peer_id: Uuid},
    Lobby {peer_id: Uuid},
    ReadSync {peer_id: Uuid},
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

impl Event {
    pub fn is_quit(&self) -> bool {
        match self {
            Event::Quit {peer_id} => true,
            _ => false
        }
    }
}


// TODO: implement parsing to and from bytes for message events
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Quit,

    Lobby,

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
    /// Helper method to reduce code duplication when serializing a `Frame`
    ///
    /// Many `Frame` variants have fields which have a length property that needs to be serialized.
    /// This method provides the functionality to do this.
    /// Takes `tag`, the tag that we are serializing the `Response` into, `idx` the starting position
    /// from which to start serializing a length value and `length`, the value that represents the length
    /// we wish to serialize
    fn serialize_len(tag: &mut FrameEncodeTag, idx: usize, length: u32) {
        for i in idx ..idx + 4 {
            tag[i] ^= ((length >> ((i - idx) * 8)) & 0xff) as u8;
        }
    }

    /// Helper method to reduce code duplication when deserializing a `Frame` tag.
    ///
    /// Many `Frame` variants have fields which have a length property that needs to be deserialized.
    /// This method provides the functionality to do this.
    /// Takes `tag`, the tag that we are deserializing `length` from, and the starting index `idx`,
    /// that is the position in the `tag` we should start deserializing bytes from.
    fn deserialize_len(tag: &FrameEncodeTag, idx: usize, length: &mut u32) {
        for i in idx .. idx + 4 {
            *length ^= (tag[i] as u32) << ((i - idx) * 8);
        }
    }

    /// Attempts to parse the `Frame` from `input_reader`.
    ///
    /// #Panics
    /// If an invalid type byte is read from `input_reader` (i.e. a byte that does not represent
    /// a valid type of `Frame`) then the method panics.
    pub async fn try_parse<R: AsyncReadExt + Unpin>(mut input_reader: R) -> Result<Self, &'static str> {
        // Read tag from reader
        let mut tag = [0u8; 5];
        input_reader.read_exact(&mut tag)
            .await
            .map_err(|_| "unable to read frame tag bytes from reader")?;

        // deserialize tag
        let (type_byte, length) = Frame::deserialize(&tag);

        // Attempt to parse the frame
        if type_byte ^ 1 == 0 {
            Ok(Frame::Quit)
        } else if type_byte ^ 2 == 0 {
            Ok(Frame::Lobby)
        } else if type_byte ^ 3 == 0 {
            let mut chatroom_name_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut chatroom_name_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Join {chatroom_name: String::from_utf8(chatroom_name_bytes).map_err(|_| "unable to parse chatroom name as valid utf8")?})
        } else if type_byte ^ 4 == 0 {
            let mut chatroom_name_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut chatroom_name_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Create {chatroom_name: String::from_utf8(chatroom_name_bytes).map_err(|_| "unable to parse chatroom name as valid utf8")?})
        } else if type_byte ^ 5 == 0 {
            let mut new_username_bytes = vec![0; length as usize];
            input_reader.read_exact(&mut new_username_bytes)
                .await
                .map_err(|_| "unable to read bytes from reader")?;
            Ok(Frame::Username {new_username: String::from_utf8(new_username_bytes).map_err(|_| "unable to parse new username as valid utf8")?})
        } else if type_byte ^ 6 == 0 {
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
            Frame::Lobby => tag[0] ^= 2,
            Frame::Join {chatroom_name} => {
                tag[0] ^= 3;
                Frame::serialize_len(&mut tag, 1, chatroom_name.len() as u32);
            }
            Frame::Create {chatroom_name} => {
                tag[0] ^= 4;
                Frame::serialize_len(&mut tag, 1, chatroom_name.len() as u32);
            }
            Frame::Username {new_username} => {
                tag[0] ^= 5;
                Frame::serialize_len(&mut tag, 1,new_username.len() as u32);
            }
            Frame::Message { message} => {
                tag[0] ^= 6;
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
            Frame::Lobby => {},
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
        if tag[0] ^ 1 == 0 {
            (1, 0)
        } else if tag[0] ^ 2 == 0 {
            (2, 0)
        } else if tag[0] ^ 3 == 0 {
            let mut chatroom_name_len = 0u32;
            Frame::deserialize_len(&tag, 1, &mut chatroom_name_len);
            (3, chatroom_name_len)
        } else if tag[0] ^ 4 == 0 {
            let mut chatroom_name_len = 0u32;
            Frame::deserialize_len(&tag, 1, &mut chatroom_name_len);
            (4, chatroom_name_len)
        } else if tag[0] ^ 5 == 0 {
            let mut username_len = 0;
            Frame::deserialize_len(&tag,1, &mut username_len);
            (5, username_len)
        } else if tag[0] ^ 6 == 0 {
            let mut message_len = 0;
            Frame::deserialize_len(&tag, 1, &mut message_len);
            (6, message_len)
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

        let frame = Frame::Lobby;
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [2, 0, 0, 0, 0]);

        let frame = Frame::Join { chatroom_name: String::from("Test Chatroom 1") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [3, 15, 0, 0, 0]);

        let frame = Frame::Create { chatroom_name: String::from("Chatroom 1") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [4, 10, 0, 0, 0]);

        let frame = Frame::Username { new_username: String::from("My new username") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [5, 15, 0, 0, 0]);

        let frame = Frame::Message { message: String::from("My test message") };
        let frame_tag = frame.serialize();

        println!("{:?}", frame_tag);
        assert_eq!(frame_tag, [6, 15, 0, 0, 0]);
    }

    #[test]
    fn test_deserialize_frame() {
        let frame = Frame::Quit;
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 1);
        assert_eq!(length, 0);

        let frame = Frame::Lobby;
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 2);
        assert_eq!(length, 0);

        let frame = Frame::Join { chatroom_name: String::from("Test Chatroom 1") };
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 3);
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
        assert_eq!(type_byte, 5);
        assert_eq!(length, 15);

        let frame = Frame::Message { message: String::from("My test message") };
        let frame_tag = frame.serialize();
        let (type_byte, length) = Frame::deserialize(&frame_tag);

        println!("type: {}, length: {}", type_byte, length);
        assert_eq!(type_byte, 6);
        assert_eq!(length, 15);
    }

    #[test]
    fn test_frame_as_bytes() {
        let frame = Frame::Quit;
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        assert_eq!(frame_bytes, vec![1, 0, 0, 0, 0]);

        let frame = Frame::Lobby;
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        assert_eq!(frame_bytes, vec![2, 0, 0, 0, 0]);

        let frame = Frame::Join { chatroom_name: String::from("Test Chatroom 1") };
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);

        let mut res_bytes = vec![3, 15, 0, 0, 0];
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
        let mut res_bytes = vec![5, 15, 0, 0, 0];
        res_bytes.extend_from_slice("My new username".as_bytes());
        assert_eq!(frame_bytes, res_bytes);

        let frame = Frame::Message { message: String::from("My test message") };
        let frame_bytes = frame.as_bytes();

        println!("{:?}", frame_bytes);
        let mut res_bytes = vec![6, 15, 0, 0, 0];
        res_bytes.extend_from_slice("My test message".as_bytes());
        assert_eq!(frame_bytes, res_bytes);
    }

    #[test]
    fn test_frame_try_parse() {
        use async_std::io::Cursor;
        use async_std::task::block_on;

        // Test quit
        let frame = Frame::Quit;
        let mut input_stream = Cursor::new(frame.as_bytes());

        let parsed_frame_res = block_on(Frame::try_parse(&mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test quit
        let frame = Frame::Lobby;
        let mut input_stream = Cursor::new(frame.as_bytes());

        let parsed_frame_res = block_on(Frame::try_parse(&mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test join
        let frame = Frame::Join {chatroom_name: String::from("Testing testing... Chatroom good name")};
        let mut input_stream = Cursor::new(frame.as_bytes());

        let parsed_frame_res = block_on(Frame::try_parse( &mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test create
        let frame = Frame::Create {chatroom_name: String::from("Testing testing... another testing Chatroom good name")};
        let mut input_stream = Cursor::new(frame.as_bytes());

        let parsed_frame_res = block_on(Frame::try_parse(&mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test username
        let frame = Frame::Username {new_username: String::from("A good testing username")};
        let mut input_stream = Cursor::new(frame.as_bytes());

        let parsed_frame_res = block_on(Frame::try_parse(&mut input_stream));

        println!("{:?}", parsed_frame_res);
        assert!(parsed_frame_res.is_ok());

        let parsed_frame = parsed_frame_res.unwrap();

        assert_eq!(frame, parsed_frame);

        // Test message
        let frame = Frame::Message {message: String::from("Testing testing... another test message")};
        let mut input_stream = Cursor::new(frame.as_bytes());

        let parsed_frame_res = block_on(Frame::try_parse(&mut input_stream));

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
        assert_eq!(tag, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Subscribed
        let response = Response::Subscribed { chatroom_name: String::from("Test Chatroom") };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [2, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomCreated
        let response = Response::ChatroomCreated { chatroom_name: String::from("Test Chatroom 42") };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [3, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomAlreadyExists
        let response = Response::ChatroomAlreadyExists { chatroom_name: String::from("Test Chatroom 43"), lobby_state: vec![38, 23, 1, 1, 0] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [4, 16, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomDoesNotExist
        let response = Response::ChatroomDoesNotExist { chatroom_name: String::from("Test Chatroom 43"), lobby_state: vec![38, 23, 1, 1, 0] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [5, 16, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ChatroomFull
        let response = Response::ChatroomFull { chatroom_name: String::from("Test Chatroom 44"), lobby_state: vec![38, 23, 1, 1, 0, 0, 0, 0, 34] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [6, 16, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Message
        let peer_id = Uuid::new_v4();
        let response = Response::Message { peer_id,  msg: String::from("Test message") };
        let tag = response.serialize();
        println!("{:?}", tag);
        let mut expected_tag = [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_tag[1..17].copy_from_slice(peer_id.as_bytes());
        // expected_tag[17] = 13;
        expected_tag[17] = 12;
        assert_eq!(tag, expected_tag);

        // Test UsernameOk
        let response = Response::UsernameOk {username: String::from("Good test username"), lobby_state: vec![0, 0, 0] };
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [8, 18, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test UsernameAlreadyExists
        let response = Response::UsernameAlreadyExists {username: String::from("Good test username 42")};
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [9, 21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ExitChatroom
        let response = Response::ExitChatroom {chatroom_name: String::from("Perfect chatroom name")};
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [10, 21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Lobby
        let response = Response::Lobby {lobby_state: vec![42, 42, 42, 42, 42, 42, 10, 1]};
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [11, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ExitLobby
        let response = Response::ExitLobby;
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test ReadSync
        let response = Response::ReadSync;
        let tag = response.serialize();
        println!("{:?}", tag);
        assert_eq!(tag, [13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
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
        let response = Response::Message {peer_id, msg: String::from("A good test message") };
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 7);
        assert_eq!(id, peer_id.as_u128());
        assert_eq!(name_len, 0);
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

        // Test ExitChatroom
        let response = Response::ExitChatroom {chatroom_name: String::from("Decent test chatroom name")};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 10);
        assert_eq!(id, 0);
        assert_eq!(name_len, 25);
        assert_eq!(data_len, 0);

        // Test Lobby
        let response = Response::Lobby {lobby_state: vec![42, 42, 42, 42, 42, 42, 10, 1]};
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 11);
        assert_eq!(id, 0);
        assert_eq!(name_len, 0);
        assert_eq!(data_len, 8);

        // Test ExitLobby
        let response = Response::ExitLobby;
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 12);
        assert_eq!(id, 0);
        assert_eq!(name_len, 0);
        assert_eq!(data_len, 0);

        // Test ReadSync
        let response = Response::ReadSync;
        let tag = response.serialize();
        let (type_byte, id, name_len, data_len) = Response::deserialize(&tag);
        println!("{:?}", (type_byte, id, name_len, data_len));
        assert_eq!(type_byte, 13);
        assert_eq!(id, 0);
        assert_eq!(name_len, 0);
        assert_eq!(data_len, 0);
    }

    #[test]
    fn test_response_as_bytes() {
        // Test ConnectionOk
        let response = Response::ConnectionOk;
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        assert_eq!(response_bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        // Test Subscribed
        let response = Response::Subscribed { chatroom_name: String::from("Test Chatroom") };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![2, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomCreated
        let response = Response::ChatroomCreated { chatroom_name: String::from("Test Chatroom 42") };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![3, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom 42").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomAlreadyExists
        let response = Response::ChatroomAlreadyExists { chatroom_name: String::from("Test Chatroom 43"), lobby_state: vec![97, 98, 99] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![4, 16, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom 43").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99]);
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomDoesNotExist
        let response = Response::ChatroomDoesNotExist { chatroom_name: String::from("Test Chatroom 44"), lobby_state: vec![97, 98, 99, 100] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![5, 16, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Chatroom 44").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99, 100]);
        assert_eq!(response_bytes, expected_bytes);

        // Test ChatroomFull
        let response = Response::ChatroomFull { chatroom_name: String::from("Another Test Chatroom 45"), lobby_state: vec![97, 98, 99, 100, 101, 103] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![6, 24, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Another Test Chatroom 45").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99, 100, 101, 103]);
        assert_eq!(response_bytes, expected_bytes);

        // Test Message
        let peer_id = Uuid::new_v4();
        let response = Response::Message { peer_id,  msg: String::from("Hello World!")};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![0; 22];
        expected_bytes[0] = 7;
        expected_bytes[1..17].copy_from_slice(peer_id.as_bytes());
        expected_bytes[17] = 12;
        // expected_bytes[21] = 12;
        // expected_bytes.extend_from_slice(String::from("A good test username").as_bytes());
        expected_bytes.extend_from_slice(String::from("Hello World!").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test UsernameOk
        let response = Response::UsernameOk { username: String::from("My test username"), lobby_state: vec![97, 98, 99, 100, 101, 103] };
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![8, 16, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("My test username").as_bytes());
        expected_bytes.extend_from_slice(&[97, 98, 99, 100, 101, 103]);
        assert_eq!(response_bytes, expected_bytes);

        // Test UsernameAlreadyExists
        let response = Response::UsernameAlreadyExists { username: String::from("My bad test username")};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![9, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("My bad test username").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test ExitChatroom
        let response = Response::ExitChatroom { chatroom_name: String::from("Test Exit chatroom name")};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![10, 23, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(String::from("Test Exit chatroom name").as_bytes());
        assert_eq!(response_bytes, expected_bytes);

        // Test Lobby
        let response = Response::Lobby { lobby_state: vec![97, 98, 99, 100, 101, 103]};
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![11, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        expected_bytes.extend_from_slice(&[97, 98, 99, 100, 101, 103]);
        assert_eq!(response_bytes, expected_bytes);

        // Test ExitLobby
        let response = Response::ExitLobby;
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(response_bytes, expected_bytes);

        // Test ReadSync
        let response = Response::ReadSync;
        let response_bytes = response.as_bytes();
        println!("{:?}", response_bytes);
        let mut expected_bytes = vec![13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(response_bytes, expected_bytes);

    }

    #[test]
    fn test_try_parse_response() {
        use async_std::io::Cursor;
        use async_std::task::block_on;

        // Test ConnectionOk
        let response = Response::ConnectionOk;
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test Subscribed
        let response = Response::Subscribed {chatroom_name: String::from("A good test chatroom name")};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ChatroomCreated
        let response = Response::ChatroomCreated {chatroom_name: String::from("A good test chatroom name 432134")};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ChatroomAlreadyExists
        let response = Response::ChatroomAlreadyExists {chatroom_name: String::from("A good test chatroom name 432134"), lobby_state: vec![0, 1, 2, 3, 4]};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ChatroomDoesNotExist
        let response = Response::ChatroomDoesNotExist {chatroom_name: String::from("A good test chatroom name 666"), lobby_state: vec![6, 6, 6]};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ChatroomFull
        let response = Response::ChatroomFull {chatroom_name: String::from("Another good test chatroom name 666"), lobby_state: vec![6, 6, 6, 6, 6, 6]};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test Message
        let peer_id = Uuid::new_v4();
        // let username = String::from("A good test username");
        let msg = String::from("A good test message");
        let response = Response::Message {peer_id,  msg};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test UsernameOk
        let response = Response::UsernameOk {username: String::from("Another good test username 666"), lobby_state: vec![6, 6, 6]};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test UsernameAlreadyExists
        let response = Response::UsernameAlreadyExists {username: String::from("Another bad test username 666 already taken")};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ExitChatroom
        let response = Response::ExitChatroom {chatroom_name: String::from("A good test chatroom name for exiting")};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test Lobby
        let response = Response::Lobby {lobby_state: vec![6, 6, 6]};
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ExitLobby
        let response = Response::ExitLobby;
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);

        // Test ReadSync
        let response = Response::ReadSync;
        let mut input_reader = Cursor::new(response.as_bytes());

        let parsed_response_res = block_on(Response::try_parse(&mut input_reader));
        println!("{:?}", parsed_response_res);
        assert!(parsed_response_res.is_ok());

        let parsed_response = parsed_response_res.unwrap();
        assert_eq!(parsed_response, response);
    }



    #[test]
    fn test_chatroom_serialize() {
        use tokio::sync::broadcast;
        use async_std::channel;

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
        use tokio::sync::broadcast;
        use async_std::channel;

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
        use tokio::sync::broadcast;
        use async_std::channel;

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

    #[test]
    fn test_try_parse_chatroom_frame() {
        use tokio::sync::broadcast;
        use async_std::channel;
        use std::io::Cursor;

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
        let mut cursor = Cursor::new(chatroom_bytes);

        // Attempt to parse the frame
        let chatroom_frame_res = ChatroomFrame::try_parse(&mut cursor);

        println!("{:?}", chatroom_frame_res);

        let chatroom_frame = chatroom_frame_res.unwrap();

        println!("{:?}", chatroom_frame);

        assert_eq!(chatroom_frame.name.as_str(), chatroom.name.as_str());
        assert_eq!(chatroom_frame.capacity, chatroom.capacity);
        assert_eq!(chatroom_frame.num_clients, chatroom.num_clients);
    }

    #[test]
    fn test_try_from_chatroom_frames() {
        use tokio::sync::broadcast;
        use async_std::channel;
        use std::io::Cursor;

        let id = Uuid::new_v4();
        let (broadcast_sender, _) = broadcast::channel::<Response>(1);
        let (chat_sender, _) = channel::unbounded::<Event>();
        let name = Arc::new(String::from("Test chatroom 666"));

        let chatroom1 = Chatroom {
            id,
            name: name.clone(),
            client_subscriber:  broadcast_sender,
            client_read_sender: chat_sender,
            shutdown: None,
            capacity: 4798,
            num_clients: 2353,
        };

        let id = Uuid::new_v4();
        let (broadcast_sender, _) = broadcast::channel::<Response>(1);
        let (chat_sender, _) = channel::unbounded::<Event>();
        let name = Arc::new(String::from("Test chatroom 667"));

        let chatroom2 = Chatroom {
            id,
            name: name.clone(),
            client_subscriber:  broadcast_sender,
            client_read_sender: chat_sender,
            shutdown: None,
            capacity: 4798,
            num_clients: 2353,
        };

        let id = Uuid::new_v4();
        let (broadcast_sender, _) = broadcast::channel::<Response>(1);
        let (chat_sender, _) = channel::unbounded::<Event>();
        let name = Arc::new(String::from("Test chatroom 668"));

        let chatroom3 = Chatroom {
            id,
            name: name.clone(),
            client_subscriber:  broadcast_sender,
            client_read_sender: chat_sender,
            shutdown: None,
            capacity: 4798,
            num_clients: 2353,
        };

        // Create simulated lobby state
        let mut lobby_state = vec![];
        lobby_state.append(&mut chatroom1.as_bytes());
        lobby_state.append(&mut chatroom2.as_bytes());
        lobby_state.append(&mut chatroom3.as_bytes());

        let chatroom_frames_res = ChatroomFrames::try_from(lobby_state);

        println!("{:?}", chatroom_frames_res);

        assert!(chatroom_frames_res.is_ok());

        let chatroom_frames = chatroom_frames_res.unwrap();

        println!("{:?}", chatroom_frames);

        assert_eq!(chatroom_frames.frames[0].name.as_str(), chatroom1.name.as_str());
        assert_eq!(chatroom_frames.frames[0].capacity, chatroom1.capacity);
        assert_eq!(chatroom_frames.frames[0].num_clients, chatroom1.num_clients);

        assert_eq!(chatroom_frames.frames[1].name.as_str(), chatroom2.name.as_str());
        assert_eq!(chatroom_frames.frames[1].capacity, chatroom2.capacity);
        assert_eq!(chatroom_frames.frames[1].num_clients, chatroom2.num_clients);

        assert_eq!(chatroom_frames.frames[2].name.as_str(), chatroom3.name.as_str());
        assert_eq!(chatroom_frames.frames[2].capacity, chatroom3.capacity);
        assert_eq!(chatroom_frames.frames[2].num_clients, chatroom3.num_clients);
    }
}