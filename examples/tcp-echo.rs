//! TCP echo server.
//!
//! To send messages, do:
//!
//! ```sh
//! $ nc localhost 8080
//! ```

use futures::io;
use smol::{Async, Task};
use std::net::TcpListener;

fn main() -> io::Result<()> {
    Task::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:8080")?;
        loop {
            let (stream, _) = listener.accept().await?;
            Task::schedule(async move {
                io::copy(&stream, &mut &stream).await.expect("failed");
            });
        }
    })
}
