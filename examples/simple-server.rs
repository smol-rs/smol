// To access the HTTPS version, import the certificate into Chrome/Firefox:
// 1. Open settings and go to the certificate 'Authorities' list
// 2. Click 'Import' and select certificate.pem
// 3. Enable 'Trust this CA to identify websites' and click OK
// 4. Restart the browser and go to https://localhost:8001
//
// The certificate was generated using minica and openssl:
// 1. minica --domains localhost -ip-addresses 127.0.0.1 -ca-cert certificate.pem
// 2. openssl pkcs12 -export -out identity.pfx -inkey localhost/key.pem -in localhost/cert.pem

use std::fs;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;

use anyhow::Result;
use async_native_tls::TlsAcceptor;
use futures::prelude::*;
use smol::{Async, Task, blocking};

const RESPONSE: &[u8] = br#"
HTTP/1.1 200 OK
Content-Type: text/html

<!DOCTYPE html><html><body>Hello!</body></html>
"#;

async fn serve(mut stream: Async<TcpStream>, tls: Option<TlsAcceptor>) -> Result<()> {
    match tls {
        None => {
            println!("Serving http://{}", stream.get_ref().local_addr()?);
            stream.write_all(RESPONSE).await?;
            stream.get_ref().shutdown(Shutdown::Both)?;
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

fn main() -> Result<()> {
    smol::run(async {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("identity.pfx");
        let identity = blocking!(fs::read(path))?;
        let tls = TlsAcceptor::new(&identity[..], "password").await?;

        let http = Async::<TcpListener>::bind("127.0.0.1:8000")?;
        let https = Async::<TcpListener>::bind("127.0.0.1:8001")?;
        println!("Listening on http://{}", http.get_ref().local_addr()?);
        println!("Listening on https://{}", https.get_ref().local_addr()?);

        loop {
            futures::select! {
                res = http.accept().fuse() => {
                    let (stream, _) = res?;
                    Task::spawn(serve(stream, None)).unwrap().forget();
                }
                res = https.accept().fuse() => {
                    let (stream, _) = res?;
                    Task::spawn(serve(stream, Some(tls.clone()))).unwrap().forget();
                }
            }
        }
    })
}
