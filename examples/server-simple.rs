// Importing the certificate for HTTPS on Google Chrome
// - Navigate to: Settings > Privacy and Security > More > Manage Certificates
// - Switch to the Authorities list
// - Click Import
// - Select examples/localhost/minica.pem
// - Enable: Trust this CA to identify websites
// - Click OK
// - Restart Chrome
// - Go to: https://localhost:8001
//
// Importing the certificate for HTTPS on Mozilla Firefox
// - Navigate to: Preferences > View Certificates > Import
// - Select examples/localhost/minica.pem
// - Enable: Trust this CA to identify websites
// - Click OK and then OK again
// - Restart Firefox
// - Go to: https://localhost:8001

use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;

use anyhow::Result;
use async_tls::TlsAcceptor;
use futures::prelude::*;
use rustls::{Certificate, NoClientAuth, PrivateKey, ServerConfig};
use smol::{Async, Task};

async fn serve(mut stream: Async<TcpStream>, tls: Option<TlsAcceptor>) -> Result<()> {
    let resp = [
        b"HTTP/1.1 200 OK\r\n".to_vec(),
        b"Content-Type: text/html\r\n\r\n".to_vec(),
        b"<!DOCTYPE html><html><body>Hello!</body></html>\r\n".to_vec(),
    ]
    .concat();

    match tls {
        None => {
            println!("Serving http://{}", stream.get_ref().local_addr()?);
            stream.write_all(&resp).await?;
            stream.get_ref().shutdown(Shutdown::Both)?;
        }
        Some(tls) => {
            println!("Serving https://{}", stream.get_ref().local_addr()?);
            let mut stream = tls.accept(stream).await?;
            stream.write_all(&resp).await?;
            stream.flush().await?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let tls = setup_tls()?;

    smol::run(async {
        let http = Async::<TcpListener>::bind("127.0.0.1:8000")?;
        let https = Async::<TcpListener>::bind("127.0.0.1:8001")?;
        println!("Listening on http://{}", http.get_ref().local_addr()?);
        println!("Listening on https://{}", https.get_ref().local_addr()?);

        loop {
            piper::select! {
                res = http.accept() => {
                    let (stream, _) = res?;
                    Task::spawn(serve(stream, None)).unwrap().forget();
                }
                res = https.accept() => {
                    let (stream, _) = res?;
                    Task::spawn(serve(stream, Some(tls.clone()))).unwrap().forget();
                }
            }
        }
    })
}

// Certificates were generated using minica: https://github.com/jsha/minica
// Command: minica --domains localhost -ip-addresses 127.0.0.1
// Useful files:
// - minica.pem
// - localhost/cert.pem
// - localhost/key.pem
fn setup_tls() -> Result<TlsAcceptor> {
    let certs = load_pem("../../tmp/localhost/cert.pem")?
        .into_iter()
        .map(Certificate)
        .collect::<Vec<_>>();

    let mut keys = load_pem("../../tmp/localhost/key.pem")?
        .into_iter()
        .map(PrivateKey)
        .collect::<Vec<_>>();

    let mut config = ServerConfig::new(NoClientAuth::new());
    config.set_single_cert(certs, keys.remove(0))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Loads PEM sections from a file.
fn load_pem(path: &str) -> Result<Vec<Vec<u8>>> {
    let mut file = BufReader::new(File::open(path)?);
    let mut sections = Vec::new();
    let mut in_section = false;
    let mut buf = String::new();

    loop {
        let mut line = String::new();
        if file.read_line(&mut line)? == 0 {
            return Ok(sections);
        }

        if line.starts_with("-----BEGIN") {
            in_section = true;
        } else if line.starts_with("-----END") {
            in_section = false;
            sections.push(base64::decode(&buf)?);
            buf.clear();
        } else if in_section {
            buf.push_str(line.trim());
        }
    }
}
