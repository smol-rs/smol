use smol::Timer;
use std::time::Duration;

async fn sleep(dur: Duration) {
    Timer::after(dur).await;
}

fn main() {
    smol::run(async {
        sleep(Duration::from_secs(5)).await;
    });
}
