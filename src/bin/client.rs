mod interface;

use interface::prelude::*;

enum UserError {
    ParseResponse(&'static str),
    InternalServerError(&'static str),
    ParseLobby(&'static str),
    ReadInput(&'static str),
    ReadError(std::io::Error),
    RawOutput(std::io::Error),
    WriteError(std::io::Error),
    SendError(&'static str),
    ConnectionError(std::io::Error),
}

fn main() {
    todo!()
}