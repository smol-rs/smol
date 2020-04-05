use std::fs;
use std::net::TcpStream;
use std::path::Path;

use anyhow::Result;
use futures::io;
use futures::prelude::*;
use piper::Lock;
use smol::Async;

fn main() -> Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("certificate.pem");
    let cert = native_tls::Certificate::from_pem(&fs::read(path)?)?;

    let mut builder = native_tls::TlsConnector::builder();
    builder.add_root_certificate(cert);
    let connector = async_native_tls::TlsConnector::from(builder);

    smol::run(async {
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        let stream = Async::<TcpStream>::connect("localhost:7001").await?;
        let stream = connector.connect("localhost", stream).await?;

        println!("Connected to {}", stream.get_ref().get_ref().peer_addr()?);
        let stream = Lock::new(stream);

        future::try_join(
            io::copy(stdin, &mut &stream),
            io::copy(&stream, &mut stdout),
        )
        .await?;

        Ok(())
    })
}
