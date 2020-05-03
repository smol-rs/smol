//! A TCP client secured by TLS based on `async-native-tls`.
//!
//! First start a server:
//!
//! ```
//! cargo run --bin tls-server
//! ```
//!
//! Then start a client:
//!
//! ```
//! cargo run --bin tls-client
//! ```

use std::net::TcpStream;

use anyhow::Result;
use async_native_tls::{Certificate, TlsConnector};
use futures::io;
use futures::prelude::*;
use piper::Mutex;
use smol::Async;

fn main() -> Result<()> {
    // Initialize TLS with the local certificate.
    let mut builder = native_tls::TlsConnector::builder();
    builder.add_root_certificate(Certificate::from_pem(include_bytes!("certificate.pem"))?);
    let tls = TlsConnector::from(builder);

    smol::run(async {
        // Create async stdin and stdout handles.
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        // Connect to the server.
        let stream = Async::<TcpStream>::connect("127.0.0.1:7001").await?;
        let stream = tls.connect("127.0.0.1", stream).await?;
        println!("Connected to {}", stream.get_ref().get_ref().peer_addr()?);
        println!("Type a message and hit enter!\n");

        // Pipe messages from stdin to the server and pipe messages from the server to stdout.
        let stream = Mutex::new(stream);
        future::try_join(
            io::copy(stdin, &mut &stream),
            io::copy(&stream, &mut stdout),
        )
        .await?;

        Ok(())
    })
}
