//! Prints the HTML contents of http://www.example.com

use std::io;
use std::net::TcpStream;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{bail, Context as _, Error, Result};
use async_tls::client::TlsStream;
use async_tls::TlsConnector;
use futures::prelude::*;
use http::Uri;
use hyper::{Body, Client, Request, Response};
use once_cell::sync::Lazy;
use smol::{Async, Task};

async fn receive(req: Request<Body>) -> Result<Response<Body>> {
    Ok(Client::builder()
        .executor(SmolExecutor)
        .build::<_, Body>(SmolConnector)
        .request(req)
        .await?)
}

fn main() -> Result<()> {
    smol::run(async {
        let req = Request::get("https://www.rust-lang.org").body(Body::empty())?;
        let resp = receive(req).await?;
        println!("{:#?}", resp);

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

#[derive(Clone)]
struct SmolExecutor;

impl<F: Future + Send + 'static> hyper::rt::Executor<F> for SmolExecutor {
    fn execute(&self, fut: F) {
        Task::spawn(async { drop(fut.await) }).forget();
    }
}

#[derive(Clone)]
struct SmolConnector;

impl hyper::service::Service<Uri> for SmolConnector {
    type Response = SmolStream;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

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
                    Ok(SmolStream::Http(stream))
                }
                Some("https") => {
                    // In case of https, establish secure TLS connection first.
                    static TLS: Lazy<TlsConnector> = Lazy::new(|| TlsConnector::new());

                    let addr = format!("{}:{}", uri.host().unwrap(), uri.port_u16().unwrap_or(443));
                    let stream = Async::<TcpStream>::connect(addr).await?;
                    let stream = TLS.connect(host.to_string(), stream)?.await?;
                    Ok(SmolStream::Https(stream))
                }
                scheme => bail!("unsupported scheme: {:?}", scheme),
            }
        })
    }
}

enum SmolStream {
    Http(Async<TcpStream>),
    Https(TlsStream<Async<TcpStream>>),
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
            SmolStream::Http(s) => Pin::new(s).poll_read(cx, buf),
            SmolStream::Https(s) => Pin::new(s).poll_read(cx, buf),
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
            SmolStream::Http(s) => Pin::new(s).poll_write(cx, buf),
            SmolStream::Https(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Http(s) => Pin::new(s).poll_flush(cx),
            SmolStream::Https(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Http(s) => s.get_ref().shutdown(std::net::Shutdown::Write)?,
            SmolStream::Https(_) => {},
        }
        Poll::Ready(Ok(()))
    }
}
