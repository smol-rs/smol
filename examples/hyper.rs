use std::convert::Infallible;
use std::net::TcpListener;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use smol::Async;

async fn hello(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new(Body::from("Hello World!")))
}

pub fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    smol::run(async {
        let addr = "127.0.0.1:3000";
        let listener = Async::<TcpListener>::bind(addr)?;

        let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(hello)) });
        let server = Server::builder(compat::HyperListener(listener))
            .executor(compat::HyperExecutor)
            .serve(make_svc);

        println!("Listening on http://{}", addr);
        server.await?;
        Ok(())
    })
}

pub mod compat {
    use std::io;
    use std::net::{TcpListener, TcpStream};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures::prelude::*;
    use smol::{Async, Task};

    #[derive(Clone)]
    pub struct HyperExecutor;

    impl<F> hyper::rt::Executor<F> for HyperExecutor
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            Task::spawn(async { drop(fut.await) }).forget();
        }
    }

    pub struct HyperListener(pub Async<TcpListener>);

    impl hyper::server::accept::Accept for HyperListener {
        type Conn = HyperStream;
        type Error = io::Error;

        fn poll_accept(
            mut self: Pin<&mut Self>,
            cx: &mut Context,
        ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
            let stream =
                futures::ready!(Pin::new(&mut self.0.incoming()).poll_next(cx)).unwrap()?;
            Poll::Ready(Some(Ok(HyperStream(stream))))
        }
    }

    pub struct HyperStream(pub Async<TcpStream>);

    impl tokio::io::AsyncRead for HyperStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl tokio::io::AsyncWrite for HyperStream {
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
}
