use std::time::{Duration, Instant};

use smol::{Async, Task};

fn main() {
    Task::run(async {
        let start = Instant::now();

        let dur = Duration::from_secs(1);
        Async::timer(dur).await;

        dbg!(start.elapsed());
    })
}
