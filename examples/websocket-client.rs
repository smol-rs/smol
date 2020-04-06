use std::net::TcpStream;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{bail, Context as _, Result};
use async_native_tls::{Certificate, TlsConnector, TlsStream};
use async_tungstenite::WebSocketStream;
use futures::prelude::*;
use smol::Async;
use tungstenite::handshake::client::Response;
use tungstenite::Message;
use url::Url;

async fn connect(addr: &str, tls: TlsConnector) -> Result<(WsStream, Response)> {
    // Parse the address.
    let url = Url::parse(addr)?;
    let host = url.host_str().context("cannot parse host")?.to_string();
    let port = url.port_or_known_default().context("cannot guess port")?;

    // Connect to the address.
    match url.scheme() {
        "ws" => {
            let stream = Async::<TcpStream>::connect(format!("{}:{}", host, port)).await?;
            let (stream, resp) = async_tungstenite::client_async(addr, stream).await?;
            Ok((WsStream::Plain(stream), resp))
        }
        "wss" => {
            let stream = Async::<TcpStream>::connect(format!("{}:{}", host, port)).await?;
            let stream = tls.connect(host, stream).await?;
            let (stream, resp) = async_tungstenite::client_async(addr, stream).await?;
            Ok((WsStream::Tls(stream), resp))
        }
        scheme => bail!("unsupported scheme: {}", scheme),
    }
}

fn main() -> Result<()> {
    // Create a TLS connector that is able to connect to wss://localhost:9001
    let mut builder = native_tls::TlsConnector::builder();
    builder.add_root_certificate(Certificate::from_pem(include_bytes!("../certificate.pem"))?);
    let tls = TlsConnector::from(builder);

    smol::run(async {
        let (mut stream, resp) = connect("wss://echo.websocket.org", tls).await?;
        dbg!(resp);

        stream.send(Message::text("Hello!")).await?;
        dbg!(stream.next().await);

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
