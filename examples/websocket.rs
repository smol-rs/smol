use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt, TryFutureExt};
use smol::{Task, Timer};

fn main() {
    // Creating blocking pool
    for _ in 0..num_cpus::get().max(1) { std::thread::Builder::new().name("processor".into()).spawn(|| smol::run(futures::future::pending::<()>())).unwrap(); }
    // Main io
    smol::run(async {
        match websocket("mercury.deribit.com", "wss", "ws/api/v2", 443).await {
            Ok((ws, _res)) => {
                let (mut ws_sink, mut ws_rx) = ws.split();
                let _ = ws_sink.send(async_tungstenite::tungstenite::Message::Text("{\"method\":\"public/subscribe\",\"params\":{\"channels\":[ \"book.BTC-PERPETUAL.raw\"]},\"jsonrpc\":\"2.0\",\"id\": 0}".to_string())).await;
                while let Some(Ok(data)) = ws_rx.next().await {
                    match data {
                        async_tungstenite::tungstenite::Message::Text(text) => {
                            let now = Instant::now();
                            //let is_subscription = text.starts_with("{\"jsonrpc\":\"2.0\",\"method\":\"subscription\"");
                            let elapsed1 = now.elapsed().as_micros();
                            Task::blocking(async move {
                                let spawned_now = Instant::now();
                                //let parsed = serde_json::to_value(text).unwrap();
                                //let elapsed_spawned_now = now.elapsed().as_micros();
                                Task::blocking(async move {
                                    // println!("+ + SPAWNED in | {:?}micros, spawning price is {:?} micros", now.elapsed().as_micros(), spawned_now.elapsed().as_micros());
                                }).forget();
                                println!("+ SPAWNED in | {:?}micros, sub spawned task cost {:?} micros", now.elapsed().as_micros(), spawned_now.elapsed().as_micros());
                            }).forget();
                            let elapsed2 = now.elapsed().as_micros();
                            // println!("MAIN {:?}micros => {:?}micros", elapsed1, elapsed2);
                        }
                        async_tungstenite::tungstenite::Message::Binary(binary) => {}
                        async_tungstenite::tungstenite::Message::Ping(ping) => {}
                        async_tungstenite::tungstenite::Message::Pong(pong) => {}
                        async_tungstenite::tungstenite::Message::Close(frame) => {}
                    }
                }
            }
            Err(e) => {
                dbg!(e);
            }
        }
    })
}

pub async fn websocket(
    host: &str,
    protocol: &str,
    path: &str,
    port: u16,
) -> futures::io::Result<(
    async_tungstenite::WebSocketStream<
        async_tls::client::TlsStream<smol::Async<std::net::TcpStream>>,
    >,
    async_tungstenite::tungstenite::http::Response<()>,
)> {
    let mut tcp_stream =
        smol::Async::<std::net::TcpStream>::connect(format!("{}:{}", host, port)).await?;

    let tls_stream = async_tls::TlsConnector::default()
        .connect(host.to_ascii_lowercase(), tcp_stream)?
        .await?;

    let ws_request =
        url::Url::parse(&format!("{}://{}/{}", protocol, host, path)).map_err(|err| {
            futures::io::Error::new(futures::io::ErrorKind::NotFound, err.to_string())
        })?;

    async_tungstenite::client_async(ws_request, tls_stream)
        .map_err(|err| futures::io::Error::new(futures::io::ErrorKind::Other, err.to_string()))
        .await
}
