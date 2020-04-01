use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use anyhow::{Error, Result};
use async_native_tls::{TlsAcceptor, TlsStream};
use futures::prelude::*;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use smol::{blocking, Async, Task};

async fn serve(req: Request<Body>, host: String) -> Result<Response<Body>> {
    println!("Serving {}{}", host, req.uri());
    Ok(Response::new(Body::from("Hello from hyper!")))
}

async fn listen(listener: Async<TcpListener>, tls: Option<TlsAcceptor>) -> Result<()> {
    let host = &match tls {
        None => format!("http://{}", listener.get_ref().local_addr()?),
        Some(_) => format!("https://{}", listener.get_ref().local_addr()?),
    };
    println!("Listening on {}", host);

    Server::builder(SmolListener::new(listener, tls))
        .executor(SmolExecutor)
        .serve(make_service_fn(move |_| {
            let host = host.clone();
            async { Ok::<_, Error>(service_fn(move |req| serve(req, host.clone()))) }
        }))
        .await?;

    Ok(())
}

fn main() -> Result<()> {
    // Create a thread pool.
    for _ in 0..num_cpus::get_physical().max(1) {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    smol::block_on(async {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("identity.pfx");
        let identity = blocking!(fs::read(path))?;
        let tls = TlsAcceptor::new(&identity[..], "password").await?;

        let http = listen(Async::<TcpListener>::bind("127.0.0.1:8000")?, None);
        let https = listen(Async::<TcpListener>::bind("127.0.0.1:8001")?, Some(tls));
        future::try_join(http, https).await?;

        Ok(())
    })
}

#[derive(Clone)]
struct SmolExecutor;

impl<F: Future + Send + 'static> hyper::rt::Executor<F> for SmolExecutor {
    fn execute(&self, fut: F) {
        Task::spawn(async { drop(fut.await) }).detach();
    }
}

struct SmolListener {
    listener: Async<TcpListener>,
    tls: Option<TlsAcceptor>,
}

impl SmolListener {
    fn new(listener: Async<TcpListener>, tls: Option<TlsAcceptor>) -> Self {
        Self { listener, tls }
    }
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
        let stream = match &self.tls {
            None => SmolStream::Plain(stream),
            Some(tls) => {
                let tls = tls.clone();
                SmolStream::Handshake(Box::pin(async move {
                    tls.accept(stream)
                        .await
                        .map_err(|err| io::Error::new(io::ErrorKind::Other, Box::new(err)))
                }))
            }
        };
        Poll::Ready(Some(Ok(stream)))
    }
}

enum SmolStream {
    Plain(Async<TcpStream>),
    Tls(TlsStream<Async<TcpStream>>),
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
                SmolStream::Plain(s) => return Pin::new(s).poll_read(cx, buf),
                SmolStream::Tls(s) => return Pin::new(s).poll_read(cx, buf),
                SmolStream::Handshake(f) => {
                    let s = futures::ready!(f.as_mut().poll(cx))?;
                    *self = SmolStream::Tls(s);
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
        loop {
            match &mut *self {
                SmolStream::Plain(s) => return Pin::new(s).poll_write(cx, buf),
                SmolStream::Tls(s) => return Pin::new(s).poll_write(cx, buf),
                SmolStream::Handshake(f) => {
                    let s = futures::ready!(f.as_mut().poll(cx))?;
                    *self = SmolStream::Tls(s);
                }
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Plain(s) => Pin::new(s).poll_flush(cx),
            SmolStream::Tls(s) => Pin::new(s).poll_flush(cx),
            SmolStream::Handshake(_) => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmolStream::Plain(s) => {
                s.get_ref().shutdown(std::net::Shutdown::Write)?;
                Poll::Ready(Ok(()))
            }
            SmolStream::Tls(s) => Pin::new(s).poll_close(cx),
            SmolStream::Handshake(_) => Poll::Ready(Ok(())),
        }
    }
}
