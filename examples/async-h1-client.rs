//! An HTTP+TLS client based on `async-h1` and `async-native-tls`.
//!
//! Run with:
//!
//! ```
//! cd examples  # make sure to be in this directory
//! cargo run --example async-h1-client
//! ```

use std::net::TcpStream;

use anyhow::{bail, Context as _, Error, Result};
use futures::prelude::*;
use http_types::{Method, Request, Response};
use smol::Async;
use url::Url;

/// Sends a request and fetches the response.
async fn fetch(req: Request) -> Result<Response> {
    // Figure out the host and the port.
    let host = req.url().host().context("cannot parse host")?.to_string();
    let port = req
        .url()
        .port_or_known_default()
        .context("cannot guess port")?;

    // Connect to the host.
    let addr = format!("{}:{}", host, port);
    let stream = Async::<TcpStream>::connect(addr).await?;

    // Send the request and wait for the response.
    let resp = match req.url().scheme() {
        "http" => async_h1::connect(stream, req).await.map_err(Error::msg)?,
        "https" => {
            // In case of HTTPS, establish a secure TLS connection first.
            let stream = async_native_tls::connect(&host, stream).await?;
            async_h1::connect(stream, req).await.map_err(Error::msg)?
        }
        scheme => bail!("unsupported scheme: {}", scheme),
    };
    Ok(resp)
}

fn main() -> Result<()> {
    smol::run(async {
        // Create a request.
        let addr = "https://www.rust-lang.org";
        let req = Request::new(Method::Get, Url::parse(addr)?);

        // Fetch the response.
        let mut resp = fetch(req).await?;
        println!("{:#?}", resp);

        // Read the message body.
        let mut body = Vec::new();
        resp.read_to_end(&mut body).await?;
        println!("{}", String::from_utf8_lossy(&body));

        Ok(())
    })
}
