//! Contains needed data structures for parsing `Responses` sent by the server into
//! client facing `Frames`.


/// An enum the variants of which, contain all of the necessary user facing data sent from
/// the server in the form of responses.
pub enum ResponseFrame {
    Subscribed { chatroom_name: String },
    ChatroomCreated
}

