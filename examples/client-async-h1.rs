//! Prints the HTML contents of http://www.example.com

use std::net::TcpStream;

use anyhow::{bail, Context, Error, Result};
use async_tls::TlsConnector;
use http_types::{Method, Request, Response};
use once_cell::sync::Lazy;
use smol::Async;
use url::Url;

async fn get(addr: &str) -> Result<Response> {
    // Parse the URL and construct a request.
    let url = Url::parse(addr)?;
    let host = url.host().context("cannot parse host")?.to_string();
    let port = url.port_or_known_default().context("cannot guess port")?;
    let addr = format!("{}:{}", host, port);
    let req = Request::new(Method::Get, url);

    // Connect to the host.
    let stream = Async::<TcpStream>::connect(addr).await?;

    // Send the request and wait for the response.
    let resp = match req.url().scheme() {
        "http" => async_h1::connect(stream, req).await.map_err(Error::msg)?,
        "https" => {
            // In case of https, establish secure TLS connection first.
            static TLS: Lazy<TlsConnector> = Lazy::new(|| TlsConnector::new());
            let stream = TLS.connect(host, stream)?.await?;
            async_h1::connect(stream, req).await.map_err(Error::msg)?
        }
        scheme => bail!("unsupported scheme: {}", scheme),
    };
    Ok(resp)
}

fn main() -> Result<()> {
    smol::run(async {
        println!("{:#?}", get("https://www.rust-lang.org").await?);
        Ok(())
    })
}
