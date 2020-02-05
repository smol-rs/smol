//! TCP echo server.
//!
//! To send messages, do:
//!
//! ```sh
//! $ nc localhost 8080
//! ```

use std::net::{TcpListener, TcpStream};

use futures::io;
use smol::{Async, Task};

async fn process(mut stream: Async<TcpStream>) -> io::Result<()> {
    println!("Peer: {}", stream.source().peer_addr()?);
    io::copy(stream.clone(), &mut stream).await?;
    Ok(())
}

fn main() -> io::Result<()> {
    futures::executor::block_on(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:8080")?;
        println!("Local: {}", listener.source().local_addr()?);

        loop {
            let (stream, _) = listener.accept().await?;
            Task::schedule(process(stream));
        }
    })
}
