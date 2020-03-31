#![recursion_limit = "1024"]

use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;

use anyhow::Result;
use async_native_tls::TlsAcceptor;
use futures::prelude::*;
use http_types::{Request, Response, StatusCode};
use smol::{blocking, Async, Task};

/// Serves a request and returns a response.
async fn serve(req: Request) -> http_types::Result<Response> {
    println!("Serving {}", req.url());
    let mut res = Response::new(StatusCode::Ok);
    res.insert_header("Content-Type", "text/plain")?;
    res.set_body("Hello from async-h1!");
    Ok(res)
}

fn main() -> Result<()> {
    // Create a thread pool.
    let num_threads = num_cpus::get_physical().max(1);
    for _ in 0..num_threads {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    smol::block_on(async {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("identity.pfx");
        let identity = blocking!(fs::read(path))?;
        let tls = TlsAcceptor::new(&identity[..], "password").await?;

        let http = Async::<TcpListener>::bind("127.0.0.1:8000")?;
        let https = Async::<TcpListener>::bind("127.0.0.1:8001")?;

        let http_addr = format!("http://{}", http.get_ref().local_addr()?);
        let https_addr = format!("https://{}", https.get_ref().local_addr()?);

        println!("Listening on {}", http_addr);
        println!("Listening on {}", https_addr);

        loop {
            futures::select! {
                res = http.accept().fuse() => {
                    let (stream, _) = res?;
                    let http_addr = http_addr.clone();
                    Task::spawn(async move {
                        async_h1::accept(&http_addr, SharedIo::new(stream), serve).await
                    })
                    .unwrap()
                    .detach();
                }
                res = https.accept().fuse() => {
                    let (stream, _) = res?;
                    let stream = tls.accept(stream).await?;
                    let https_addr = https_addr.clone();
                    Task::spawn(async move {
                        async_h1::accept(&https_addr, SharedIo::new(stream), serve).await
                    })
                    .unwrap()
                    .detach();
                }
            }
        }
    })
}

struct SharedIo<T>(Arc<Mutex<T>>);

impl<T> SharedIo<T> {
    fn new(io: T) -> Self {
        SharedIo(Arc::new(Mutex::new(io)))
    }
}

impl<T> Clone for SharedIo<T> {
    fn clone(&self) -> SharedIo<T> {
        SharedIo(self.0.clone())
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for SharedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for SharedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_close(cx)
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for &SharedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for &SharedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0.lock().unwrap()).poll_close(cx)
    }
}
