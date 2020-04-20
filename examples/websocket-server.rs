use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use anyhow::{Context as _, Result};
use async_native_tls::{Identity, TlsAcceptor, TlsStream};
use async_tungstenite::WebSocketStream;
use futures::prelude::*;
use smol::{Async, Task};
use tungstenite::Message;

async fn serve(mut stream: WsStream, host: String) -> Result<()> {
    println!("Serving {}", host);
    let msg = stream.next().await.context("expected a message")??;
    stream
        .send(Message::text(format!("Server echoes: {}", msg)))
        .await?;
    Ok(())
}

async fn listen(listener: Async<TcpListener>, tls: Option<TlsAcceptor>) -> Result<()> {
    let host = match &tls {
        None => format!("ws://{}", listener.get_ref().local_addr()?),
        Some(_) => format!("wss://{}", listener.get_ref().local_addr()?),
    };
    println!("Listening on {}", host);

    loop {
        let (stream, _) = listener.accept().await?;
        let host = host.clone();

        match &tls {
            None => {
                let stream = WsStream::Plain(async_tungstenite::accept_async(stream).await?);
                Task::spawn(serve(stream, host.clone())).unwrap().detach();
            }
            Some(tls) => {
                let stream = tls.accept(stream).await?;
                let stream = WsStream::Tls(async_tungstenite::accept_async(stream).await?);
                Task::spawn(serve(stream, host.clone())).unwrap().detach();
            }
        }
    }
}

fn main() -> Result<()> {
    let identity = Identity::from_pkcs12(include_bytes!("../identity.pfx"), "password")?;
    let tls = TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?);

    // Create an executor thread pool.
    for _ in 0..num_cpus::get().max(1) {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    smol::block_on(async {
        let ws = listen(Async::<TcpListener>::bind("127.0.0.1:9000")?, None);
        let wss = listen(Async::<TcpListener>::bind("127.0.0.1:9001")?, Some(tls));
        future::try_join(ws, wss).await?;
        Ok(())
    })
}

enum WsStream {
    Plain(WebSocketStream<Async<TcpStream>>),
    Tls(WebSocketStream<TlsStream<Async<TcpStream>>>),
}

impl Sink<Message> for WsStream {
    type Error = tungstenite::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &mut *self {
            WsStream::Plain(s) => Pin::new(s).poll_ready(cx),
            WsStream::Tls(s) => Pin::new(s).poll_ready(cx),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        match &mut *self {
            WsStream::Plain(s) => Pin::new(s).start_send(item),
            WsStream::Tls(s) => Pin::new(s).start_send(item),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &mut *self {
            WsStream::Plain(s) => Pin::new(s).poll_flush(cx),
            WsStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match &mut *self {
            WsStream::Plain(s) => Pin::new(s).poll_close(cx),
            WsStream::Tls(s) => Pin::new(s).poll_close(cx),
        }
    }
}

impl Stream for WsStream {
    type Item = tungstenite::Result<Message>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match &mut *self {
            WsStream::Plain(s) => Pin::new(s).poll_next(cx),
            WsStream::Tls(s) => Pin::new(s).poll_next(cx),
        }
    }
}
