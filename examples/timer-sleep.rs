use std::time::{Duration, Instant};

use smol::Timer;

async fn sleep(dur: Duration) {
    Timer::after(dur).await;
}

fn main() {
    smol::run(async {
        let start = Instant::now();
        sleep(Duration::from_secs(1)).await;
        dbg!(start.elapsed());
    })
}
