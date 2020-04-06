use std::net::{TcpListener, TcpStream};

use anyhow::Result;
use async_native_tls::{Identity, TlsAcceptor, TlsStream};
use futures::io;
use piper::Lock;
use smol::{Async, Task};

async fn echo(stream: TlsStream<Async<TcpStream>>) -> Result<()> {
    println!("Copying");
    let stream = Lock::new(stream);
    io::copy(&stream, &mut &stream).await?;
    Ok(())
}

fn main() -> Result<()> {
    let identity = Identity::from_pkcs12(include_bytes!("../identity.pfx"), "password")?;
    let tls = TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?);

    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:7001")?;
        println!("Listening on {}", listener.get_ref().local_addr()?);

        loop {
            let (stream, _) = listener.accept().await?;
            let stream = tls.accept(stream).await?;
            Task::spawn(echo(stream)).unwrap().detach();
        }
    })
}
