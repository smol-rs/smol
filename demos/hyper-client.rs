//! An HTTP+TLS client based on `hyper` and `async-native-tls`.
//!
//! Run with:
//!
//! ```
//! cargo run --bin hyper-client
//! ```

use std::io;
use std::net::{Shutdown, TcpStream};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{bail, Context as _, Error, Result};
use async_native_tls::TlsStream;
use futures::prelude::*;
use http::Uri;
use hyper::{Body, Client, Request, Response};
use smol::{Async, Task};

/// Sends a request and fetches the response.
async fn fetch(req: Request<Body>) -> Result<Response<Body>> {
    Ok(Client::builder()
        .executor(SmolExecutor)
        .build::<_, Body>(SmolConnector)
        .request(req)
        .await?)
}

fn main() -> Result<()> {
    smol::run(async {
        // Create a request.
        let req = Request::get("https://www.rust-lang.org").body(Body::empty())?;

        // Fetch the response.
        let resp = fetch(req).await?;
        println!("{:#?}", resp);

        // Read the message body.
        let body = resp
            .into_body()
            .try_fold(Vec::new(), |mut body, chunk| async move {
                body.extend_from_slice(&chunk);
                Ok(body)
            })
            .await?;
        println!("{}", String::from_utf8_lossy(&body));

        Ok(())
    })
}

/// Spawns futures.
#[derive(Clone)]
struct SmolExecutor;

impl<F: Future + Send + 'static> hyper::rt::Executor<F> for SmolExecutor {
    fn execute(&self, fut: F) {
        Task::spawn(async { drop(fut.await) }).detach();
    }
}

/// Connects to URLs.
#[derive(Clone)]
struct SmolConnector;

impl hyper::service::Service<Uri> for SmolConnector {
    type Response = SmolStream;
    type Error = Error;
    type Future = future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        Box::pin(async move {
            let host = uri.host().context("cannot parse host")?;

            match uri.scheme_str() {
                Some("http") => {
                    let addr = format!("{}:{}", uri.host().unwrap(), uri.port_u16().unwrap_or(80));
                    let stream = Async::<TcpStream>::connect(addr).await?;
                    Ok(SmolStream::Plain(stream))
                }
                Some("https") => {
                    // In case of HTTPS, establish a secure TLS connection first.
                    let addr = format!("{}:{}", uri.host().unwrap(), uri.port_u16().unwrap_or(443));
                    let stream = Async::<TcpStream>::connect(addr).await?;
                    let stream = async_native_tls::connect(host, stream).await?;
                    Ok(SmolStream::Tls(stream))
                }
                scheme => bail!("unsupported scheme: {:?}", scheme),
            }
        })
    }
}

/// A TCP or TCP+TLS connection.
enum SmolStream {
    /// A plain TCP connection.
    Plain(Async<TcpStream>),

    /// A TCP connection secured by TLS.
    Tls(TlsStream<Async<TcpStream>>),
}

impl hyper::client::connect::Connection for SmolStream {
    fn connected(&self) -> hyper::client::connect::Connected {
        hyper::client::connect::Connected::new()
    }
}

impl tokio::io::AsyncRead for SmolStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            SmolStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            SmolStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for SmolStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            SmolStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            SmolStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Plain(s) => Pin::new(s).poll_flush(cx),
            SmolStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Plain(s) => {
                s.get_ref().shutdown(Shutdown::Write)?;
                Poll::Ready(Ok(()))
            }
            SmolStream::Tls(s) => Pin::new(s).poll_close(cx),
        }
    }
}
