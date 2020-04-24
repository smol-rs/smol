use async_std::sync::Mutex;
use futures::channel::mpsc;
use futures::stream::StreamExt;
use std::sync::Arc;

#[test]
fn contention() {
    smol::block_on(async {
        let (tx, mut rx) = mpsc::unbounded();

        let tx = Arc::new(tx);
        let mutex = Arc::new(Mutex::new(0));
        let num_tasks = 10; //000;

        for _ in 0..num_tasks {
            let tx = tx.clone();
            let mutex = mutex.clone();

            dbg!("spawn");
            smol::Task::spawn(async move {
                let mut lock = mutex.lock().await;
                *lock += 1;
                tx.unbounded_send(()).unwrap();
                drop(lock);
            })
            .detach();
        }

        for i in 0..num_tasks {
            dbg!(i);
            rx.next().await.unwrap();
        }

        dbg!("wait");

        let lock = mutex.lock().await;
        assert_eq!(num_tasks, *lock);
    });
}
