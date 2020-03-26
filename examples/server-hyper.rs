use std::fs::File;
use std::io::prelude::*;
use std::io::{self, BufReader};
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::thread;

use anyhow::{Error, Result};
use async_tls::server::TlsStream;
use async_tls::TlsAcceptor;
use futures::prelude::*;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use rustls::{Certificate, NoClientAuth, PrivateKey, ServerConfig};
use smol::{Async, Task};
use tokio_util::compat::*;

async fn serve(_req: Request<Body>) -> Result<Response<Body>> {
    Ok(Response::new(Body::from("Hello from hyper!")))
}

fn main() -> Result<()> {
    // Create a thread pool.
    for _ in 0..num_cpus::get_physical().max(1) {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    let tls = setup_tls()?;

    smol::block_on(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:8000")?;
        println!("Listening on http://{}", listener.get_ref().local_addr()?);

        Server::builder(SmolListener {
            listener,
            tls: None,
        })
        .executor(SmolExecutor)
        .serve(make_service_fn(|_| async {
            Ok::<_, Error>(service_fn(serve))
        }))
        .await?;

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

struct SmolListener {
    listener: Async<TcpListener>,
    tls: Option<TlsAcceptor>,
}

impl hyper::server::accept::Accept for SmolListener {
    type Conn = SmolStream;
    type Error = Error;

    fn poll_accept(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let poll = Pin::new(&mut self.listener.incoming()).poll_next(cx);
        let stream = futures::ready!(poll).unwrap()?;
        let tls = self.tls.clone().unwrap();
        Poll::Ready(Some(Ok(SmolStream::Handshake(Box::pin(async move {
            tls.accept(stream).await
        })))))
    }
}

enum SmolStream {
    Http(Async<TcpStream>),
    Https(TlsStream<Async<TcpStream>>),
    Handshake(Pin<Box<dyn Future<Output = io::Result<TlsStream<Async<TcpStream>>>> + Send>>),
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
        loop {
            match &mut *self {
                SmolStream::Http(s) => return Pin::new(s).poll_read(cx, buf),
                SmolStream::Https(s) => return Pin::new(s).poll_read(cx, buf),
                SmolStream::Handshake(f) => {
                    let s = futures::ready!(f.as_mut().poll(cx))?;
                    *self = SmolStream::Https(s);
                }
            }
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
            SmolStream::Handshake(f) => todo!(),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match &mut *self {
            SmolStream::Http(s) => Pin::new(s).poll_flush(cx),
            SmolStream::Https(s) => Pin::new(s).poll_flush(cx),
            SmolStream::Handshake(f) => Poll::Ready(())
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Http(s) => s.get_ref().shutdown(std::net::Shutdown::Write)?,
            SmolStream::Https(_) => {}
            SmolStream::Handshake(f) => {}
        }
        Poll::Ready(Ok(()))
    }
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
