//! Prints the HTML contents of http://www.example.com

use std::net::TcpStream;

use anyhow::{bail, Context as _, Result};
use futures::prelude::*;
use smol::Async;
use url::Url;

async fn fetch(addr: &str) -> Result<Vec<u8>> {
    // Parse the URL.
    let url = Url::parse(addr)?;
    let host = url.host().context("cannot parse host")?.to_string();
    let port = url.port_or_known_default().context("cannot guess port")?;
    let path = url.path().to_string();
    let query = match url.query() {
        Some(q) => format!("?{}", q),
        None => String::new(),
    };

    // Construct a request.
    let req = format!(
        "GET {}{} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        path, query, host,
    );

    // Connect to the host.
    let mut stream = Async::<TcpStream>::connect(format!("{}:{}", host, port)).await?;
    let mut resp = Vec::new();

    // Send the request and wait for the response.
    match url.scheme() {
        "http" => {
            stream.write_all(req.as_bytes()).await?;
            stream.read_to_end(&mut resp).await?;
        }
        "https" => {
            // In case of HTTPS, establish secure TLS connection first.
            let mut stream = async_native_tls::connect(&host, stream).await?;
            stream.write_all(req.as_bytes()).await?;
            stream.read_to_end(&mut resp).await?;
        }
        scheme => bail!("unsupported scheme: {}", scheme),
    }

    Ok(resp)
}

fn main() -> Result<()> {
    smol::run(async {
        let addr = "https://www.rust-lang.org";
        let resp = fetch(addr).await?;
        println!("{}", String::from_utf8_lossy(&resp));
        Ok(())
    })
}
