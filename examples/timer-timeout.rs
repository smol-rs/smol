use futures::prelude::*;
use futures::io::{self, BufReader};
use smol::Timer;
use std::io::Result;
use std::time::Duration;

async fn timeout<T>(
    dur: Duration,
    f: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    futures::select! {
        t = f.fuse() => t,
        _ = Timer::after(dur).fuse() => Err(io::ErrorKind::TimedOut.into()),
    }
}

fn main() -> Result<()> {
    smol::run(async {
        // Create a buffered stdin reader.
        let mut stdin = BufReader::new(smol::reader(std::io::stdin()));

        // Read a line within 5 seconds.
        let mut line = String::new();
        timeout(Duration::from_secs(5), stdin.read_line(&mut line)).await?;
        io::Result::Ok(())
    })
}
