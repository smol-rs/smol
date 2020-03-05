use std::net::TcpStream;

use futures::io;
use futures::prelude::*;
use smol::Async;

fn main() -> io::Result<()> {
    smol::run(async {
        let mut stream = Async::<TcpStream>::connect("www.example.com:80").await?;

        let request = b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n";
        stream.write_all(request).await?;

        let mut response = String::new();
        stream.read_to_string(&mut response).await?;
        println!("{}", response);

        Ok(())
    })
}
