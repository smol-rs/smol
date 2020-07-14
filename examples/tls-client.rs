//! A TCP client secured by TLS based on `async-native-tls`.
//!
//! First start a server:
//!
//! ```
//! cargo run --example tls-server
//! ```
//!
//! Then start a client:
//!
//! ```
//! cargo run --example tls-client
//! ```

use anyhow::Result;
use async_native_tls::{Certificate, TlsConnector};
use async_net::TcpStream;
use blocking::{block_on, Unblock};
use futures::io;
use futures::prelude::*;

fn main() -> Result<()> {
    // Initialize TLS with the local certificate.
    let mut builder = native_tls::TlsConnector::builder();
    builder.add_root_certificate(Certificate::from_pem(include_bytes!("certificate.pem"))?);
    let tls = TlsConnector::from(builder);

    block_on(async {
        // Create async stdin and stdout handles.
        let stdin = Unblock::new(std::io::stdin());
        let mut stdout = Unblock::new(std::io::stdout());

        // Connect to the server.
        let stream = TcpStream::connect("127.0.0.1:7001").await?;
        let stream = tls.connect("127.0.0.1", stream).await?;
        println!("Connected to {}", stream.get_ref().peer_addr()?);
        println!("Type a message and hit enter!\n");

        // Pipe messages from stdin to the server and pipe messages from the server to stdout.
        let stream = async_dup::Mutex::new(stream);
        future::try_join(
            io::copy(stdin, &mut &stream),
            io::copy(&stream, &mut stdout),
        )
        .await?;

        Ok(())
    })
}
