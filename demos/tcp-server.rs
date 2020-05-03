//! A TCP server.
//!
//! First start a server:
//!
//! ```
//! cargo run --bin tcp-server
//! ```
//!
//! Then start clients:
//!
//! ```
//! cargo run --bin tcp-client
//! ```

use std::net::{TcpListener, TcpStream};

use futures::io;
use smol::{Async, Task};

/// Echoes messages from the client back to it.
async fn echo(stream: Async<TcpStream>) -> io::Result<()> {
    io::copy(&stream, &mut &stream).await?;
    Ok(())
}

fn main() -> io::Result<()> {
    smol::run(async {
        // Create a listener.
        let listener = Async::<TcpListener>::bind("127.0.0.1:7000")?;
        println!("Listening on {}", listener.get_ref().local_addr()?);
        println!("Now start a TCP client.");

        // Accept clients in a loop.
        loop {
            let (stream, peer_addr) = listener.accept().await?;
            println!("Accepted client: {}", peer_addr);

            // Spawn a task that echoes messages from the client back to it.
            Task::spawn(echo(stream)).unwrap().detach();
        }
    })
}
