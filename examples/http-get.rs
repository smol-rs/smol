//! TCP echo server.
//!
//! To send messages, do:
//!
//! ```sh
//! $ nc localhost 8080
//! ```

use futures::executor::block_on;
use futures::io;
use futures::prelude::*;
use smol::Async;

fn main() -> io::Result<()> {
    block_on(async {
        let mut stream = Async::connect("www.example.com:80").await?;
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n")
            .await?;

        let mut html = String::new();
        stream.read_to_string(&mut html).await?;
        println!("{}", html);

        Ok(())
    })
}
