//! A TCP chat server.
//!
//! First start a server:
//!
//! ```
//! cargo run --example chat-server
//! ```
//!
//! Then start clients:
//!
//! ```
//! cargo run --example chat-client
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;

use async_channel::{bounded, Receiver, Sender};
use async_net::{TcpListener, TcpStream};
use blocking::block_on;
use futures::io::{self, BufReader};
use futures::prelude::*;
use smol::Task;

/// An event on the chat server.
enum Event {
    /// A client has joined.
    Join(SocketAddr, TcpStream),

    /// A client has left.
    Leave(SocketAddr),

    /// A client sent a message.
    Message(SocketAddr, String),
}

/// Dispatches events to clients.
async fn dispatch(receiver: Receiver<Event>) -> io::Result<()> {
    // Currently active clients.
    let mut map = HashMap::<SocketAddr, TcpStream>::new();

    // Receive incoming events.
    while let Ok(event) = receiver.recv().await {
        // Process the event and format a message to send to clients.
        let output = match event {
            Event::Join(addr, stream) => {
                map.insert(addr, stream);
                format!("{} has joined\n", addr)
            }
            Event::Leave(addr) => {
                map.remove(&addr);
                format!("{} has left\n", addr)
            }
            Event::Message(addr, msg) => format!("{} says: {}\n", addr, msg),
        };

        // Display the event in the server process.
        print!("{}", output);

        // Send the event to all active clients.
        for stream in map.values_mut() {
            // Ignore errors because the client might disconnect at any point.
            let _ = stream.write_all(output.as_bytes()).await;
        }
    }
    Ok(())
}

/// Reads messages from the client and forwards them to the dispatcher task.
async fn read_messages(sender: Sender<Event>, client: TcpStream) -> io::Result<()> {
    let addr = client.peer_addr()?;
    let mut lines = BufReader::new(client).lines();

    while let Some(line) = lines.next().await {
        let line = line?;
        let _ = sender.send(Event::Message(addr, line)).await;
    }
    Ok(())
}

fn main() -> io::Result<()> {
    block_on(async {
        // Create a listener for incoming client connections.
        let listener = TcpListener::bind("127.0.0.1:6000").await?;

        // Intro messages.
        println!("Listening on {}", listener.local_addr()?);
        println!("Start a chat client now!\n");

        // Spawn a background task that dispatches events to clients.
        let (sender, receiver) = bounded(100);
        Task::spawn(dispatch(receiver)).detach();

        loop {
            // Accept the next connection.
            let (stream, addr) = listener.accept().await?;
            let client = stream;
            let sender = sender.clone();

            // Spawn a background task reading messages from the client.
            Task::spawn(async move {
                // Client starts with a `Join` event.
                let _ = sender.send(Event::Join(addr, client.clone())).await;

                // Read messages from the client and ignore I/O errors when the client quits.
                let _ = read_messages(sender.clone(), client).await;

                // Client ends with a `Leave` event.
                let _ = sender.send(Event::Leave(addr)).await;
            })
            .detach();
        }
    })
}
