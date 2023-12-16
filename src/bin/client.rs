mod interface;

enum UserError {
    ParseResponse(&'static str),
    InternalServerError(&'static str),
    ParseLobby(&'static str),
}

fn main() {
    todo!()
}