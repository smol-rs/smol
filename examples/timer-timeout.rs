use std::time::Duration;

use anyhow::{bail, Result};
use futures::future::{select, Either};
use futures::io::BufReader;
use futures::prelude::*;
use smol::Timer;

async fn timeout<T>(dur: Duration, f: impl Future<Output = T>) -> Result<T> {
    futures::pin_mut!(f);
    match select(f, Timer::after(dur)).await {
        Either::Left((out, _)) => Ok(out),
        Either::Right(_) => bail!("timed out"),
    }
}

fn main() -> Result<()> {
    smol::run(async {
        let mut stdin = BufReader::new(smol::reader(std::io::stdin()));
        let mut line = String::new();

        let dur = Duration::from_secs(5);
        timeout(dur, stdin.read_line(&mut line)).await??;

        println!("Line: {}", line);
        Ok(())
    })
}
