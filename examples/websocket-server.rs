#![recursion_limit = "1024"]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use anyhow::{Context as _, Result};
use async_native_tls::{TlsAcceptor, TlsStream};
use async_tungstenite::WebSocketStream;
use futures::prelude::*;
use smol::{blocking, Async, Task};
use tungstenite::Message;

async fn serve(mut stream: WsStream, host: String) -> Result<()> {
    println!("Serving {}", host);
    let msg = stream.next().await.context("expected a message")??;
    stream.send(Message::text(format!("Server echoes: {}", msg))).await?;
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

        let ws = Async::<TcpListener>::bind("127.0.0.1:9000")?;
        let wss = Async::<TcpListener>::bind("127.0.0.1:9001")?;
        let ws_host = format!("ws://{}", ws.get_ref().local_addr()?);
        let wss_host = format!("wss://{}", wss.get_ref().local_addr()?);
        println!("Listening on {}", ws_host);
        println!("Listening on {}", wss_host);

        loop {
            futures::select! {
                res = ws.accept().fuse() => {
                    let (stream, _) = res?;
                    let stream = WsStream::Plain(async_tungstenite::accept_async(stream).await?);
                    let ws_host = ws_host.clone();
                    Task::spawn(serve(stream, ws_host)).unwrap().detach();
                }
                res = wss.accept().fuse() => {
                    let (stream, _) = res?;
                    let stream = tls.accept(stream).await?;
                    let stream = WsStream::Tls(async_tungstenite::accept_async(stream).await?);
                    let wss_host = wss_host.clone();
                    Task::spawn(serve(stream, wss_host)).unwrap().detach();
                }
            }
        }
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
