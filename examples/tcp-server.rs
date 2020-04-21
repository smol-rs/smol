// TODO: document
//! TCP echo server.
//!
//! To send messages, do:
//!
//! ```sh
//! $ nc 127.0.0.1 8080
//! ```

use std::net::{TcpListener, TcpStream};

use futures::io;
use smol::{Async, Task};

async fn echo(stream: Async<TcpStream>) -> io::Result<()> {
    println!("Copying");
    io::copy(&stream, &mut &stream).await?;
    Ok(())
}

fn main() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:7000")?;
        println!("Listening on {}", listener.get_ref().local_addr()?);

        loop {
            let (stream, _) = listener.accept().await?;
            Task::spawn(echo(stream)).unwrap().detach();
        }
    })
}
