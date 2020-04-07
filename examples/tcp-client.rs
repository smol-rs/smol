use std::net::TcpStream;

use futures::io;
use futures::prelude::*;
use smol::Async;

fn main() -> io::Result<()> {
    smol::run(async {
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());
        let stream = Async::<TcpStream>::connect("127.0.0.1:7000").await?;
        println!("Connected to {}", stream.get_ref().peer_addr()?);

        future::try_join(
            io::copy(stdin, &mut &stream),
            io::copy(&stream, &mut stdout),
        )
        .await?;

        Ok(())
    })
}
