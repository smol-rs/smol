use std::time::{Duration, Instant};

use smol::Async;

fn main() {
    futures::executor::block_on(async {
        let start = Instant::now();

        let dur = Duration::from_secs(1);
        Async::timer(dur).await;

        dbg!(start.elapsed());
    })
}
