use std::time::{Duration, Instant};

use smol::Timer;

fn main() {
    smol::run(async {
        let start = Instant::now();

        let dur = Duration::from_secs(1);
        Timer::after(dur).await;

        dbg!(start.elapsed());
    })
}
