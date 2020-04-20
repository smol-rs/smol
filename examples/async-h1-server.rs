//! An HTTP+TLS server based on `async-h1` and `async-native-tls`.
//!
//! Run with:
//!
//! ```
//! cargo run --example async-h1-server
//! ```
//!
//! Open in the browser any of these addresses:
//!
//! - http://localhost:8000/
//! - https://localhost:8001/ (you'll need to import the TLS certificate first!)
//!
//! Refer to `README.md` to see how to import or generate the TLS certificate.

use std::net::TcpListener;
use std::thread;

use anyhow::Result;
use async_native_tls::{Identity, TlsAcceptor};
use futures::prelude::*;
use http_types::{Request, Response, StatusCode};
use piper::{Lock, Shared};
use smol::{Async, Task};

/// Serves a request and returns a response.
async fn serve(req: Request) -> http_types::Result<Response> {
    println!("Serving {}", req.url());

    let mut res = Response::new(StatusCode::Ok);
    res.insert_header("Content-Type", "text/plain")?;
    res.set_body("Hello from async-h1!");
    Ok(res)
}

/// Listens for incoming connections and serves them.
async fn listen(listener: Async<TcpListener>, tls: Option<TlsAcceptor>) -> Result<()> {
    // Format the full host address.
    let host = match &tls {
        None => format!("http://{}", listener.get_ref().local_addr()?),
        Some(_) => format!("https://{}", listener.get_ref().local_addr()?),
    };
    println!("Listening on {}", host);

    loop {
        // Accept the next connection.
        let (stream, _) = listener.accept().await?;
        let host = host.clone();

        // Spawn a background task serving this connection.
        let task = match &tls {
            None => {
                let stream = Shared::new(stream);
                Task::spawn(async move { async_h1::accept(&host, stream, serve).await })
            }
            Some(tls) => {
                // In case of HTTPS, establish a secure TLS connection first.
                let stream = tls.accept(stream).await?;
                let stream = Shared::new(Lock::new(stream));
                Task::spawn(async move { async_h1::accept(&host, stream, serve).await })
            }
        };

        // Detach the task to let it run in the background.
        task.unwrap().detach();
    }
}

fn main() -> Result<()> {
    // Initialize TLS with the local certificate, private key, and password.
    let identity = Identity::from_pkcs12(include_bytes!("../identity.pfx"), "password")?;
    let tls = TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?);

    // Create an executor thread pool.
    let num_threads = num_cpus::get().max(1);
    for _ in 0..num_threads {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    // Start HTTP and HTTPS servers.
    smol::block_on(async {
        let http = listen(Async::<TcpListener>::bind("127.0.0.1:8000")?, None);
        let https = listen(Async::<TcpListener>::bind("127.0.0.1:8001")?, Some(tls));
        future::try_join(http, https).await?;
        Ok(())
    })
}
