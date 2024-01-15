# Rusty Chat
## Description
An asynchronous chatroom server and client. The protocol for clients connecting to the server is implemented over Tcp. 
The client executable turns the terminal window into a simple user interface for interacting with the server. Once connected,
the server allows any client to:
1. join an existing chatroom
2. create a new chatroom

The server employs the actor model for managing shared state between asynchronous tasks. 

### Design Goals
The main goal of this project was to find an implementation that allowed distinct chatrooms to be managed as seperate tasks, in order to (hopefully) avoid bottlenecks.
In otherwords instead of having one task that manages the sending and receiving of _all_ messages, _for all_ chatrooms, each chatroom could be it's own unique task managing the sending and receiving of its own messages. This was achieved by having a main broker task, which managed shared state for all tasks, spawn new sub-broker tasks for new chatrooms. These sub-brokers would exclusively manage the sending and receiving of messages to and from clients that had joined the chatroom. That way the main broker would not have to manage the sending and receiving of messages for any particular chatroom. As a result of this design, any sub-broker task became another instance of shared state the main broker had to manage and liberated the main broker from having to send or receive any chat messages.
