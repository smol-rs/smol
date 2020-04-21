// TODO: document
use std::io;
use std::time::Duration;

use anyhow::Result;
use futures::io::BufReader;
use futures::prelude::*;
use smol::Timer;

async fn timeout<T>(dur: Duration, f: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    futures::select! {
        out = f.fuse() => out,
        _ = Timer::after(dur).fuse() => {
            Err(io::Error::from(io::ErrorKind::TimedOut))
        }
    }
}

fn main() -> Result<()> {
    smol::run(async {
        let mut line = String::new();
        let mut stdin = BufReader::new(smol::reader(std::io::stdin()));

        timeout(Duration::from_secs(5), stdin.read_line(&mut line)).await?;
        println!("Line: {}", line);

        Ok(())
    })
}
