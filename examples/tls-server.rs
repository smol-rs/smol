use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::Path;

use anyhow::Result;
use async_native_tls::{TlsAcceptor, TlsStream};
use futures::io;
use piper::Lock;
use smol::{blocking, Async, Task};

async fn echo(stream: TlsStream<Async<TcpStream>>) -> Result<()> {
    println!("Copying");
    let stream = Lock::new(stream);
    io::copy(&stream, &mut &stream).await?;
    Ok(())
}

fn main() -> Result<()> {
    // TODO: use native_tls::TlsAcceptor to avoid async files; do the same in other examples

    smol::run(async {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("identity.pfx");
        let identity = blocking!(fs::read(path))?;
        let tls = TlsAcceptor::new(&identity[..], "password").await?;

        let listener = Async::<TcpListener>::bind("127.0.0.1:7001")?;
        println!("Listening on {}", listener.get_ref().local_addr()?);

        loop {
            let (stream, _) = listener.accept().await?;
            let stream = tls.accept(stream).await?;
            Task::spawn(echo(stream)).unwrap().detach();
        }
    })
}
