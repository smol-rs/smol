// To access the HTTPS version, import the certificate into Chrome/Firefox:
// 1. Open settings and go to the certificate 'Authorities' list
// 2. Click 'Import' and select certificate.pem
// 3. Enable 'Trust this CA to identify websites' and click OK
// 4. Restart the browser and go to https://localhost:8001
//
// The certificate was generated using minica and openssl:
// 1. minica --domains localhost -ip-addresses 127.0.0.1 -ca-cert certificate.pem
// 2. openssl pkcs12 -export -out identity.pfx -inkey localhost/key.pem -in localhost/cert.pem

use std::net::{TcpListener, TcpStream};

use anyhow::Result;
use async_native_tls::{Identity, TlsAcceptor};
use futures::prelude::*;
use smol::{Async, Task};

const RESPONSE: &[u8] = br#"
HTTP/1.1 200 OK
Content-Type: text/html
Content-Length: 47

<!DOCTYPE html><html><body>Hello!</body></html>
"#;

async fn serve(mut stream: Async<TcpStream>, tls: Option<TlsAcceptor>) -> Result<()> {
    match tls {
        None => {
            println!("Serving http://{}", stream.get_ref().local_addr()?);
            stream.write_all(RESPONSE).await?;
        }
        Some(tls) => {
            println!("Serving https://{}", stream.get_ref().local_addr()?);
            let mut stream = tls.accept(stream).await?;
            stream.write_all(RESPONSE).await?;
            stream.flush().await?;
            stream.close().await?;
        }
    }
    Ok(())
}

async fn listen(listener: Async<TcpListener>, tls: Option<TlsAcceptor>) -> Result<()> {
    match &tls {
        None => println!("Listening on http://{}", listener.get_ref().local_addr()?),
        Some(_) => println!("Listening on https://{}", listener.get_ref().local_addr()?),
    }
    loop {
        let (stream, _) = listener.accept().await?;
        Task::spawn(serve(stream, tls.clone())).unwrap().detach();
    }
}

fn main() -> Result<()> {
    let identity = Identity::from_pkcs12(include_bytes!("../identity.pfx"), "password")?;
    let tls = TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?);

    smol::run(async {
        let http = listen(Async::<TcpListener>::bind("127.0.0.1:8000")?, None);
        let https = listen(Async::<TcpListener>::bind("127.0.0.1:8001")?, Some(tls));
        future::try_join(http, https).await?;
        Ok(())
    })
}
