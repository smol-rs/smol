//! TCP echo server.
//!
//! To send messages, do:
//!
//! ```sh
//! $ nc localhost 8080
//! ```

use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};

use futures::prelude::*;
use smol::{Async, Task};

async fn serve(mut stream: Async<TcpStream>) -> io::Result<()> {
    stream.write_all(b"HTTP/1.1 200 OK\r\n").await?;
    stream.write_all(b"Content-Type: text/html\r\n\r\n").await?;
    stream
        .write_all(b"<!DOCTYPE html><html><body>Hello!</body></html>\r\n")
        .await?;
    stream.get_ref().shutdown(Shutdown::Both);
    Ok(())
}

fn main() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:3000")?;
        println!("Listening on http://{}", listener.get_ref().local_addr()?);

        loop {
            let (stream, _) = listener.accept().await?;
            Task::spawn(serve(stream)).unwrap().forget();
        }
    })
}
