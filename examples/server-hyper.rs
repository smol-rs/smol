use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use anyhow::{Error, Result};
use futures::prelude::*;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
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

    smol::block_on(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:3000")?;
        println!("Listening on http://{}", listener.get_ref().local_addr()?);

        Server::builder(SmolListener(listener))
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

struct SmolListener(Async<TcpListener>);

impl hyper::server::accept::Accept for SmolListener {
    type Conn = Compat<Async<TcpStream>>;
    type Error = Error;

    fn poll_accept(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let poll = Pin::new(&mut self.0.incoming()).poll_next(cx);
        let stream = futures::ready!(poll).unwrap()?;
        Poll::Ready(Some(Ok(stream.compat())))
    }
}
