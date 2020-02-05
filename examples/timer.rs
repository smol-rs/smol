use std::time::{Duration, Instant};

use smol::Timer;

fn main() {
    futures::executor::block_on(async {
        let start = std::time::Instant::now();

        let dur = Duration::from_secs(1);
        Timer::after(dur).await;

        dbg!(start.elapsed());
    })
}
