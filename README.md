# Rusty Chat
## Description
An asynchronous chatroom server and client. The protocol for clients connecting to the server is implemented over Tcp. 
The client executable turns the terminal window into a simple user interface for interacting with the server. Once connected,
the server allows any client to:
1. join an existing chatroom
2. create a new chatroom

The server employs the actor model for managing shared state between asynchronous tasks. 

### Goals
Personal goals for this project were: to gain more experience with writing asynchronous Rust code and Rust's asynchronous ecosystem.

The main design goal of this project was to find an implementation that allowed distinct chatrooms to be managed as seperate tasks.
Instead of having one task that manages the sending and receiving of _all_ messages, _for all_ chatrooms, each chatroom could be it's own unique task, managing the sending and receiving of its own messages. This was achieved by having a main broker task, which manages shared state for all tasks, spawn sub-broker tasks for new chatrooms. These sub-brokers would exclusively manage the sending and receiving of messages to and from clients that had joined the chatroom. That way the main broker would not have to manage the sending and receiving of messages for any particular chatroom. As a result of this design, the main broker task only had to manage the sub-broker tasks as another instance of shared state.

Another design goal of this project was to transform the terminal window into a user interface, instead of having to build GUI. This was an interesting challenge and with the help of the `termion` crate, a simple working solution was found.

## Usage
To run this code one only has to clone the repository to their local machine, and then follow the instructions for running the server or client, respectively.

### Server
After cloning the repository one can pass commandline arguments that configure the server. For example `RUST_LOG=info cargo run --bin server -- -a 127.0.0.1 -p 8080 -b 10000 -c 1000`.
In this example, the server is listening for incoming requests at address 127.0.0.1 and on port 8080, the buffer size for the main broker's channel is set to 10000 and the capacity for
any chatroom is set to 1000. Depending on the value of `RUST_LOG` (in this example a log level of info was used), one should see 


