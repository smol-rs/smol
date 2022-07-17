//! An HTTP+TLS server based on `hyper` and `async-native-tls`.
//!
//! Run with:
//!
//! ```
//! cargo run --example hyper-server
//! ```
//!
//! Open in the browser any of these addresses:
//!
//! - http://localhost:8000/
//! - https://localhost:8001/ (accept the security prompt in the browser)
//!
//! Refer to `README.md` to see how to the TLS certificate was generated.

use std::net::{Shutdown, TcpListener, TcpStream};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{Error, Result};
use async_native_tls::{Identity, TlsAcceptor, TlsStream};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use smol::{future, io, prelude::*, Async};

/// Serves a request and returns a response.
async fn serve(req: Request<Body>, host: String) -> Result<Response<Body>> {
    println!("Serving {}{}", host, req.uri());
    Ok(Response::new(Body::from("Hello from hyper!")))
}

/// Listens for incoming connections and serves them.
async fn listen(listener: Async<TcpListener>, tls: Option<TlsAcceptor>) -> Result<()> {
    // Format the full host address.
    let host = &match tls {
        None => format!("http://{}", listener.get_ref().local_addr()?),
        Some(_) => format!("https://{}", listener.get_ref().local_addr()?),
    };
    println!("Listening on {}", host);

    // Start a hyper server.
    Server::builder(SmolListener::new(&listener, tls))
        .executor(SmolExecutor)
        .serve(make_service_fn(move |_| {
            let host = host.clone();
            async { Ok::<_, Error>(service_fn(move |req| serve(req, host.clone()))) }
        }))
        .await?;

    Ok(())
}

fn main() -> Result<()> {
    // Initialize TLS with the local certificate, private key, and password.
    let identity = Identity::from_pkcs12(include_bytes!("identity.pfx"), "password")?;
    let tls = TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?);

    // Start HTTP and HTTPS servers.
    smol::block_on(async {
        let http = listen(Async::<TcpListener>::bind(([127, 0, 0, 1], 8000))?, None);
        let https = listen(
            Async::<TcpListener>::bind(([127, 0, 0, 1], 8001))?,
            Some(tls),
        );
        future::try_zip(http, https).await?;
        Ok(())
    })
}

/// Spawns futures.
#[derive(Clone)]
struct SmolExecutor;

impl<F: Future + Send + 'static> hyper::rt::Executor<F> for SmolExecutor {
    fn execute(&self, fut: F) {
        smol::spawn(async { drop(fut.await) }).detach();
    }
}

/// Listens for incoming connections.
struct SmolListener<'a> {
    tls: Option<TlsAcceptor>,
    incoming: Pin<Box<dyn Stream<Item = io::Result<Async<TcpStream>>> + Send + 'a>>,
}

impl<'a> SmolListener<'a> {
    fn new(listener: &'a Async<TcpListener>, tls: Option<TlsAcceptor>) -> Self {
        Self {
            incoming: Box::pin(listener.incoming()),
            tls,
        }
    }
}

impl hyper::server::accept::Accept for SmolListener<'_> {
    type Conn = SmolStream;
    type Error = Error;

    fn poll_accept(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let stream = smol::ready!(self.incoming.as_mut().poll_next(cx)).unwrap()?;

        let stream = match &self.tls {
            None => SmolStream::Plain(stream),
            Some(tls) => {
                // In case of HTTPS, start establishing a secure TLS connection.
                let tls = tls.clone();
                SmolStream::Handshake(Box::pin(async move {
                    tls.accept(stream).await.map_err(|err| {
                        println!("Failed to establish secure TLS connection: {:#?}", err);
                        io::Error::new(io::ErrorKind::Other, Box::new(err))
                    })
                }))
            }
        };

        Poll::Ready(Some(Ok(stream)))
    }
}

/// A TCP or TCP+TLS connection.
enum SmolStream {
    /// A plain TCP connection.
    Plain(Async<TcpStream>),

    /// A TCP connection secured by TLS.
    Tls(TlsStream<Async<TcpStream>>),

    /// A TCP connection that is in process of getting secured by TLS.
    #[allow(clippy::type_complexity)]
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
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            match &mut *self {
                SmolStream::Plain(s) => {
                    return Pin::new(s)
                        .poll_read(cx, buf.initialize_unfilled())
                        .map_ok(|size| {
                            buf.advance(size);
                        });
                }
                SmolStream::Tls(s) => {
                    return Pin::new(s)
                        .poll_read(cx, buf.initialize_unfilled())
                        .map_ok(|size| {
                            buf.advance(size);
                        });
                }
                SmolStream::Handshake(f) => {
                    let s = smol::ready!(f.as_mut().poll(cx))?;
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
                    let s = smol::ready!(f.as_mut().poll(cx))?;
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
                s.get_ref().shutdown(Shutdown::Write)?;
                Poll::Ready(Ok(()))
            }
            SmolStream::Tls(s) => Pin::new(s).poll_close(cx),
            SmolStream::Handshake(_) => Poll::Ready(Ok(())),
        }
    }
}
