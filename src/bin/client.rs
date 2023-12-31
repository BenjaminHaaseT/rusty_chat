use std::fmt::{Display, Formatter};
use std::error::Error;
use async_std::task;
use tracing_appender;
use tracing_subscriber::fmt::MakeWriter;
use clap::Parser;

mod interface;
use interface::prelude::*;

#[derive(Debug)]
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
    ReceiveError(&'static str),
}

impl Display for UserError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            UserError::ParseResponse(s) => write!(f, "parse response error: {s}"),
            UserError::InternalServerError(s) => write!(f, "internal server error: {s}"),
            UserError::ParseLobby(s) => write!(f, "parse lobby error: {s}"),
            UserError::ReadInput(s) => write!(f, "read input error: {s}"),
            UserError::ReadError(e) => write!(f, "read error: {e}"),
            UserError::WriteError(e) => write!(f, "write error: {e}"),
            UserError::RawOutput(e) => write!(f, "raw output error: {e}"),
            UserError::SendError(s) => write!(f, "send error: {s}"),
            UserError::ConnectionError(e) => write!(f, "connection error: {e}"),
            UserError::ReceiveError(s) => write!(f, "receive error: {s}"),
        }
    }
}

impl Error for UserError {}

#[derive(Parser)]
struct CLI {
    /// The address the client wishes to connect to
    #[arg(short = 'a')]
    address: String,
    /// The tcp port that the client wishes to connect to
    #[arg(short = 'p')]
    port: u16
}

fn main() {
    let appender = tracing_appender::rolling::never("/Users/benjaminhaase/development/Personal/rusty_chat/src", "client.log");
    let (subscriber_writer, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(subscriber_writer)
        .init();
    let cli = CLI::parse();
    let res = task::block_on(Interface::run((cli.address.as_str(), cli.port)));
    if let Err(e) = res {
        eprintln!("error from client main: {e}");
    }
}