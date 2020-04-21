//! A TCP chat client.
//!
//! First start a server:
//!
//! ```
//! cargo run --example chat-server
//! ```
//!
//! Then start clients:
//!
//! ```
//! cargo run --example chat-client
//! ```

use std::net::TcpStream;

use futures::io;
use futures::prelude::*;
use smol::Async;

fn main() -> io::Result<()> {
    for _ in 0..1 {
        std::thread::spawn(|| smol::run(future::pending::<()>()));
    }

    smol::Task::spawn(async {
        // Connect to the server and create async stdin and stdout.
        let stream = Async::<TcpStream>::connect("127.0.0.1:6000").await.unwrap();
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        // Intro messages.
        println!("Connected to {}", stream.get_ref().peer_addr().unwrap());
        println!("My nickname: {}", stream.get_ref().local_addr().unwrap());
        println!("Type a message and hit enter!\n");

        // References to `Async<T>` also implement `AsyncRead` and `AsyncWrite`.
        let stream_r = &stream;
        let mut stream_w = &stream;

        smol::Task::local(async {
            let mut cnt = 0;
            loop {
                cnt += 1;
                eprintln!("BEFORE TIMER {}", cnt);
                smol::Timer::after(std::time::Duration::from_millis(20)).await;
                eprintln!("AFTER TIMER");
            }

        }).detach();
        // Wait until the standard input is closed or the connection is closed.
        futures::select! {
            _ = io::copy(stdin, &mut stream_w).fuse() => println!("Quit!"),
            _ = io::copy(stream_r, &mut stdout).fuse() => println!("Server disconnected!"),
        }
    }).detach();

    smol::run(async {
        // Connect to the server and create async stdin and stdout.
        let stream = Async::<TcpStream>::connect("127.0.0.1:6000").await?;
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        // Intro messages.
        println!("Connected to {}", stream.get_ref().peer_addr()?);
        println!("My nickname: {}", stream.get_ref().local_addr()?);
        println!("Type a message and hit enter!\n");

        // References to `Async<T>` also implement `AsyncRead` and `AsyncWrite`.
        let stream_r = &stream;
        let mut stream_w = &stream;

        smol::Task::local(async {
            let mut cnt = 0;
            loop {
                cnt += 1;
                eprintln!("BEFORE TIMER {}", cnt);
                smol::Timer::after(std::time::Duration::from_millis(20)).await;
                eprintln!("AFTER TIMER");
            }

        }).detach();
        // Wait until the standard input is closed or the connection is closed.
        futures::select! {
            _ = io::copy(stdin, &mut stream_w).fuse() => println!("Quit!"),
            _ = io::copy(stream_r, &mut stdout).fuse() => println!("Server disconnected!"),
        }

        Ok(())
    })
}
