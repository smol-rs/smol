//! A TCP chat client.
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

use async_net::TcpStream;
use blocking::{block_on, Unblock};
use futures::io;
use futures::prelude::*;

fn main() -> io::Result<()> {
    block_on(async {
        // Connect to the server and create async stdin and stdout.
        let stream = TcpStream::connect("127.0.0.1:6000").await?;
        let stdin = Unblock::new(std::io::stdin());
        let mut stdout = Unblock::new(std::io::stdout());

        // Intro messages.
        println!("Connected to {}", stream.peer_addr()?);
        println!("My nickname: {}", stream.local_addr()?);
        println!("Type a message and hit enter!\n");

        let reader = &stream;
        let mut writer = stream.clone();

        // Wait until the standard input is closed or the connection is closed.
        futures::select! {
            _ = io::copy(stdin, &mut writer).fuse() => println!("Quit!"),
            _ = io::copy(reader, &mut stdout).fuse() => println!("Server disconnected!"),
        }

        Ok(())
    })
}
