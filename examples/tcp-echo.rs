//! TCP echo server.
//!
//! To send messages, do:
//!
//! ```sh
//! $ nc localhost 8080
//! ```

use std::net::TcpStream;

use futures::executor::block_on;
use futures::io;
use smol::Async;

async fn process(mut stream: Async<TcpStream>) -> io::Result<()> {
    println!("Peer: {}", stream.source().peer_addr()?);
    io::copy(stream.clone(), &mut stream).await
}

fn main() -> io::Result<()> {
    block_on(async {
        let listener = Async::bind("127.0.0.1:8080")?;
        println!("Local: {}", listener.source().local_addr()?);

        loop {
            let (stream, _) = listener.accept().await?;
            smol::spawn(async { process(stream).await.unwrap() });
        }
    })
}
