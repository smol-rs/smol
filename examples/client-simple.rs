//! Prints the HTML contents of http://www.example.com

use std::net::TcpStream;

use anyhow::{bail, Context, Result};
use async_tls::TlsConnector;
use futures::prelude::*;
use once_cell::sync::Lazy;
use smol::Async;
use url::Url;

async fn get(addr: &str) -> Result<String> {
    // Parse the URL and construct a request.
    let url = Url::parse(addr)?;
    let host = url.host().context("cannot parse host")?;
    let port = url.port_or_known_default().context("cannot guess port")?;
    let path = url.path().to_string();
    let query = match url.query() {
        Some(q) => format!("?{}", q),
        None => String::new(),
    };
    let addr = format!("{}:{}", host, port);
    let req = format!(
        "GET {}{} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        path, query, host,
    );

    // Connect to the host.
    let mut stream = Async::<TcpStream>::connect(addr).await?;
    let mut resp = Vec::new();

    // Send the request and wait for the response.
    match url.scheme() {
        "http" => {
            stream.write_all(req.as_bytes()).await?;
            stream.read_to_end(&mut resp).await?;
        }
        "https" => {
            // In case of https, establish secure TLS connection first.
            static TLS: Lazy<TlsConnector> = Lazy::new(|| TlsConnector::new());
            let mut stream = TLS.connect(host.to_string(), stream)?.await?;

            stream.write_all(req.as_bytes()).await?;
            stream.read_to_end(&mut resp).await?;
        }
        scheme => bail!("unsupported scheme: {}", scheme),
    }
    Ok(String::from_utf8_lossy(&resp).into())
}

fn main() -> Result<()> {
    smol::run(async {
        println!("{}", get("https://www.rust-lang.org").await?);
        Ok(())
    })
}
