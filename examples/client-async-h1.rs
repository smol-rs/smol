//! Prints the HTML contents of http://www.example.com

use std::net::TcpStream;

use anyhow::{bail, Context as _, Error, Result};
use async_tls::TlsConnector;
use futures::prelude::*;
use http_types::{Method, Request, Response};
use once_cell::sync::Lazy;
use smol::Async;
use url::Url;

async fn receive(req: Request) -> Result<Response> {
    let host = req.url().host().context("cannot parse host")?.to_string();
    let port = req
        .url()
        .port_or_known_default()
        .context("cannot guess port")?;
    let addr = format!("{}:{}", host, port);

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
        let addr = "https://www.rust-lang.org";
        let req = Request::new(Method::Get, Url::parse(addr)?);
        let mut resp = receive(req).await?;
        println!("{:#?}", resp);

        let mut body = Vec::new();
        resp.read_to_end(&mut body).await?;
        println!("{}", String::from_utf8_lossy(&body));

        Ok(())
    })
}
