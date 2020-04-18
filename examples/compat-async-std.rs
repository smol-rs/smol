use std::time::{Instant, Duration};

use async_std::task::sleep;

fn main() {
    smol::run(async {
        let start = Instant::now();
        println!("Sleeping...");
        sleep(Duration::from_secs(1)).await;
        println!("Woke up after {:?}", start.elapsed());
    })
}
