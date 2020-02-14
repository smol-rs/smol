use std::env;
use std::net::TcpStream;

use futures::io;
use smol::Async;

fn main() -> io::Result<()> {
    let mut args = env::args().skip(1);
    let addr = args.next().expect("missing address argument");
    let port = args.next().expect("missing port argument");

    smol::run(async {
        let mut input = smol::reader(std::io::stdin());
        let mut stream = Async::<TcpStream>::connect(format!("{}:{}", addr, port)).await?;

        io::copy(&mut input, &mut stream).await?;
        Ok(())
    })
}
