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
use std::net::{SocketAddr, TcpListener, TcpStream};

use futures::io::{self, BufReader};
use futures::prelude::*;
use piper::{Receiver, Sender, Shared};
use smol::{Async, Task};

type Client = Shared<Async<TcpStream>>;

enum Event {
    Join(SocketAddr, Client),
    Leave(SocketAddr),
    Message(SocketAddr, String),
}

/// Dispatches events to clients.
async fn dispatch(receiver: Receiver<Event>) -> io::Result<()> {
    // Currently active clients.
    let mut map = HashMap::<SocketAddr, Client>::new();

    // Receive incoming events.
    while let Some(event) = receiver.recv().await {
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
            stream.write_all(output.as_bytes()).await?;
        }
    }
    Ok(())
}

/// Reads messages from the client and forwards them to the dispatcher task.
async fn read_messages(sender: Sender<Event>, client: Client) -> io::Result<()> {
    let addr = client.get_ref().peer_addr()?;
    let mut lines = BufReader::new(client).lines();

    while let Some(line) = lines.next().await {
        let line = line?;
        sender.send(Event::Message(addr, line)).await;
    }
    Ok(())
}

fn main() -> io::Result<()> {
    smol::run(async {
        // Create a listener for incoming client connections.
        let listener = Async::<TcpListener>::bind("127.0.0.1:6000")?;

        // Intro messages.
        println!("Listening on {}", listener.get_ref().local_addr()?);
        println!("Start a chat client now!\n");

        // Spawn a background task that dispatches events to clients.
        let (sender, receiver) = piper::chan(100);
        Task::spawn(dispatch(receiver)).unwrap().detach();

        loop {
            // Accept the next connection.
            let (stream, addr) = listener.accept().await?;
            let client = Shared::new(stream);
            let sender = sender.clone();

            // Spawn a background task reading messages from the client.
            Task::spawn(async move {
                // Client starts with a `Join` event.
                sender.send(Event::Join(addr, client.clone())).await;

                // Read messages from the client and ignore I/O errors when the client quits.
                let _ = read_messages(sender.clone(), client).await;

                // Client ends with a `Leave` event.
                sender.send(Event::Leave(addr)).await;
            })
            .detach();
        }
    })
}
