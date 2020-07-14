//! A TCP server.
//!
//! First start a server:
//!
//! ```
//! cargo run --example tcp-server
//! ```
//!
//! Then start a client:
//!
//! ```
//! cargo run --example tcp-client
//! ```

use async_net::{TcpListener, TcpStream};
use blocking::block_on;
use futures::io;

/// Echoes messages from the client back to it.
async fn echo(mut stream: TcpStream) -> io::Result<()> {
    io::copy(stream.clone(), &mut stream).await?;
    Ok(())
}

fn main() -> io::Result<()> {
    block_on(async {
        // Create a listener.
        let listener = TcpListener::bind("127.0.0.1:7000").await?;
        println!("Listening on {}", listener.local_addr()?);
        println!("Now start a TCP client.");

        // Accept clients in a loop.
        loop {
            let (stream, peer_addr) = listener.accept().await?;
            println!("Accepted client: {}", peer_addr);

            // Spawn a task that echoes messages from the client back to it.
            smol::spawn(echo(stream)).detach();
        }
    })
}
