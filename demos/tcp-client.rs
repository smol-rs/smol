//! A TCP client.
//!
//! First start a server:
//!
//! ```
//! cargo run --bin tcp-server
//! ```
//!
//! Then start a client:
//!
//! ```
//! cargo run --bin tcp-client
//! ```

use std::net::TcpStream;

use futures::io;
use futures::prelude::*;
use smol::Async;

fn main() -> io::Result<()> {
    smol::run(async {
        // Create async stdin and stdout handles.
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        // Connect to the server.
        let stream = Async::<TcpStream>::connect("127.0.0.1:7000").await?;
        println!("Connected to {}", stream.get_ref().peer_addr()?);
        println!("Type a message and hit enter!\n");

        // Pipe messages from stdin to the server and pipe messages from the server to stdout.
        future::try_join(
            io::copy(stdin, &mut &stream),
            io::copy(&stream, &mut stdout),
        )
        .await?;

        Ok(())
    })
}
