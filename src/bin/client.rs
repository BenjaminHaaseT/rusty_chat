mod interface;

enum UserError {
    ParseResponse(&'static str),
    InternalServerError(&'static str),
    ParseLobby(&'static str),
    ReadInput(&'static str),
    RawOutput(&'static str),
    WriteError(String),
    SendError(&'static str),
}

fn main() {
    todo!()
}