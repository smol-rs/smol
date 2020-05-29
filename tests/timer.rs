use futures::prelude::*;
use smol::{self, Timer};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn timer_at() {
    let before = smol::run(async {
        let now = Instant::now();
        let when = now + Duration::from_secs(1);
        Timer::at(when).await;
        now
    });

    assert!(before.elapsed() >= Duration::from_secs(1));
}

#[test]
fn timer_after() {
    let before = smol::run(async {
        let now = Instant::now();
        Timer::after(Duration::from_secs(1)).await;
        now
    });

    assert!(before.elapsed() >= Duration::from_secs(1));
}

#[test]
fn timer_reset() {
    smol::run(async move {
        let start = Instant::now();
        let t = Arc::new(Mutex::new(Timer::after(Duration::from_secs(10))));
        let t1 = t.clone();
        // Use another thread instead of smol::Task to avoid any interference
        // with the runtime.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            t1.lock().unwrap().reset_after(Duration::from_secs(1));
        });
        smol::Task::local(futures::future::poll_fn(move |cx| {
            t.lock().unwrap().poll_unpin(cx)
        }))
        .await;

        let e = start.elapsed();

        assert!(e >= Duration::from_millis(1300));
        assert!(e < Duration::from_millis(1700));
    });
}
