// TODO: document
use std::time::{Duration, Instant};

use smol::Timer;

async fn sleep(dur: Duration) {
    Timer::after(dur).await;
}

fn main() {
    smol::run(async {
        let start = Instant::now();
        println!("Sleeping...");
        sleep(Duration::from_secs(1)).await;
        println!("Woke up after {:?}", start.elapsed());
    })
}
