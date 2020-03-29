use std::time::Duration;

use anyhow::{bail, Result};
use futures::io::BufReader;
use futures::prelude::*;
use smol::Timer;

async fn timeout<T>(dur: Duration, f: impl Future<Output = T>) -> Result<T> {
    futures::select! {
        res = f.fuse() => Ok(res),
        _ = Timer::after(dur).fuse() => bail!("timed out"),
    }
}

fn main() -> Result<()> {
    smol::run(async {
        let mut stdin = BufReader::new(smol::reader(16 * 1024, std::io::stdin()));
        let mut line = String::new();

        let dur = Duration::from_secs(5);
        timeout(dur, stdin.read_line(&mut line)).await??;

        println!("Line: {}", line);
        Ok(())
    })
}
