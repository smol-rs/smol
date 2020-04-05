use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};

use futures::io::{self, BufReader};
use futures::prelude::*;
use piper::{Receiver, Sender, Shared};
use smol::{Async, Task};

enum Update {
    Join(SocketAddr, Shared<Async<TcpStream>>),
    Leave(SocketAddr),
    Message(SocketAddr, String),
}

async fn dispatch(receiver: Receiver<Update>) -> io::Result<()> {
    let mut map = HashMap::new();

    while let Some(update) = receiver.recv().await {
        let output = match update {
            Update::Join(addr, stream) => {
                map.insert(addr, stream);
                format!("{} has joined\n", addr)
            }
            Update::Leave(addr) => {
                map.remove(&addr);
                format!("{} has left\n", addr)
            }
            Update::Message(addr, msg) => format!("{} says: {}\n", addr, msg),
        };

        print!("{}", output);
        for stream in map.values_mut() {
            stream.write_all(output.as_bytes()).await?;
        }
    }
    Ok(())
}

async fn client(sender: Sender<Update>, stream: Shared<Async<TcpStream>>) -> io::Result<()> {
    let addr = stream.get_ref().peer_addr()?;
    let mut lines = BufReader::new(stream).lines();

    while let Some(line) = lines.next().await {
        sender.send(Update::Message(addr, line?)).await;
    }
    Ok(())
}

fn main() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:6000")?;
        println!("Listening on {}", listener.get_ref().local_addr()?);

        let (sender, receiver) = piper::chan(100);
        Task::spawn(dispatch(receiver)).unwrap().detach();

        loop {
            let (stream, addr) = listener.accept().await?;
            let stream = Shared::new(stream);
            let sender = sender.clone();

            Task::spawn(async move {
                sender.send(Update::Join(addr, stream.clone())).await;
                let _ = client(sender.clone(), stream).await;
                sender.send(Update::Leave(addr)).await;
            })
            .detach();
        }
    })
}
