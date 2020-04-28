//! A TCP chat client.
//!
//! First start a server:
//!
//! ```
//! cd examples  # make sure to be in this directory
//! cargo run --example chat-server
//! ```
//!
//! Then start clients:
//!
//! ```
//! cd examples  # make sure to be in this directory
//! cargo run --example chat-client
//! ```

use std::net::TcpStream;

use futures::io;
use futures::prelude::*;
use smol::Async;

fn main() -> io::Result<()> {
    smol::run(async {
        // Connect to the server and create async stdin and stdout.
        let stream = Async::<TcpStream>::connect("127.0.0.1:6000").await?;
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        // Intro messages.
        println!("Connected to {}", stream.get_ref().peer_addr()?);
        println!("My nickname: {}", stream.get_ref().local_addr()?);
        println!("Type a message and hit enter!\n");

        // References to `Async<T>` also implement `AsyncRead` and `AsyncWrite`.
        let stream_r = &stream;
        let mut stream_w = &stream;

        // Wait until the standard input is closed or the connection is closed.
        futures::select! {
            _ = io::copy(stdin, &mut stream_w).fuse() => println!("Quit!"),
            _ = io::copy(stream_r, &mut stdout).fuse() => println!("Server disconnected!"),
        }

        Ok(())
    })
}
