use std::time::{Duration, Instant};

use futures::channel::oneshot;
use futures::poll;
use smol::{self, Task, Timer};

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
fn timer_across_tasks() {
    let before = smol::run(async {
        let now = Instant::now();
        let (sender, receiver) = oneshot::channel();

        let task1 = Task::spawn(async move {
            let mut timer = Timer::after(Duration::from_secs(1));
            assert!(poll!(&mut timer).is_pending());
            sender.send(timer).unwrap();
        });

        let task2 = Task::spawn(async move {
            let timer = receiver.await.unwrap();
            timer.await;
        });

        task1.detach();
        task2.await;
        now
    });

    assert!(before.elapsed() >= Duration::from_secs(1));
}
