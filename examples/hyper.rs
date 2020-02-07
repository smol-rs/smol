use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::prelude::*;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use smol::{Async, Task};

async fn hello(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new(Body::from("Hello World!")))
}

struct Incoming(Async<TcpListener>);

impl hyper::server::accept::Accept for Incoming {
    type Conn = Connection;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let fut = self.0.accept();
        pin_utils::pin_mut!(fut);
        let (stream, _) = futures::ready!(fut.poll(cx))?;
        Poll::Ready(Some(Ok(Connection(stream))))
    }
}

struct Connection(Async<TcpStream>);

impl tokio::io::AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

#[derive(Clone)]
struct Executor;

impl<F> hyper::rt::Executor<F> for Executor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        Task::schedule(fut);
    }
}

pub fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futures::executor::block_on(Task::schedule(async {
        // For every connection, we must make a `Service` to handle all
        // incoming HTTP requests on said connection.
        let make_svc = make_service_fn(|_conn| {
            // This is the `Service` that will handle the connection.
            // `service_fn` is a helper to convert a function that
            // returns a Response into a `Service`.
            async { Ok::<_, Infallible>(service_fn(hello)) }
        });

        let addr = "127.0.0.1:3000";
        let listener = Async::<TcpListener>::bind(addr)?;

        let server = Server::builder(Incoming(listener))
            .executor(Executor)
            .serve(make_svc);

        println!("Listening on http://{}", addr);

        server.await?;

        Ok(())
    }))
}
