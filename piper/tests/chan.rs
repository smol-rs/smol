#![cfg(feature = "unstable")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rand::{thread_rng, Rng};

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[test]
fn smoke() {
    task::block_on(async {
        let (s, r) = piper::chan(1);

        s.send(7).await;
        assert_eq!(r.recv().await, Some(7));

        s.send(8).await;
        assert_eq!(r.recv().await, Some(8));

        drop(s);
        assert_eq!(r.recv().await, None);
    });

    task::block_on(async {
        let (s, r) = piper::chan(10);
        drop(r);
        s.send(1).await;
    });
}

#[test]
fn capacity() {
    for i in 1..10 {
        let (s, r) = channel::<()>(i);
        assert_eq!(s.capacity(), i);
        assert_eq!(r.capacity(), i);
    }
}

#[test]
fn len_empty_full() {
    #![allow(clippy::cognitive_complexity)]
    task::block_on(async {
        let (s, r) = piper::chan(2);

        assert_eq!(s.len(), 0);
        assert_eq!(s.is_empty(), true);
        assert_eq!(s.is_full(), false);
        assert_eq!(r.len(), 0);
        assert_eq!(r.is_empty(), true);
        assert_eq!(r.is_full(), false);

        s.send(()).await;

        assert_eq!(s.len(), 1);
        assert_eq!(s.is_empty(), false);
        assert_eq!(s.is_full(), false);
        assert_eq!(r.len(), 1);
        assert_eq!(r.is_empty(), false);
        assert_eq!(r.is_full(), false);

        s.send(()).await;

        assert_eq!(s.len(), 2);
        assert_eq!(s.is_empty(), false);
        assert_eq!(s.is_full(), true);
        assert_eq!(r.len(), 2);
        assert_eq!(r.is_empty(), false);
        assert_eq!(r.is_full(), true);

        r.recv().await;

        assert_eq!(s.len(), 1);
        assert_eq!(s.is_empty(), false);
        assert_eq!(s.is_full(), false);
        assert_eq!(r.len(), 1);
        assert_eq!(r.is_empty(), false);
        assert_eq!(r.is_full(), false);
    })
}

#[test]
fn recv() {
    task::block_on(async {
        let (s, r) = piper::chan(100);

        task::spawn(async move {
            assert_eq!(r.recv().await, Some(7));
            task::sleep(ms(1000)).await;
            assert_eq!(r.recv().await, Some(8));
            task::sleep(ms(1000)).await;
            assert_eq!(r.recv().await, Some(9));
            assert_eq!(r.recv().await, None);
        });

        task::sleep(ms(1500)).await;
        s.send(7).await;
        s.send(8).await;
        s.send(9).await;
    })
}

#[test]
fn send() {
    task::block_on(async {
        let (s, r) = piper::chan(1);

        task::spawn(async move {
            s.send(7).await;
            task::sleep(ms(1000)).await;
            s.send(8).await;
            task::sleep(ms(1000)).await;
            s.send(9).await;
            task::sleep(ms(1000)).await;
            s.send(10).await;
        });

        task::sleep(ms(1500)).await;
        assert_eq!(r.recv().await, Some(7));
        assert_eq!(r.recv().await, Some(8));
        assert_eq!(r.recv().await, Some(9));
    })
}

#[test]
fn recv_after_disconnect() {
    task::block_on(async {
        let (s, r) = piper::chan(100);

        s.send(1).await;
        s.send(2).await;
        s.send(3).await;

        drop(s);

        assert_eq!(r.recv().await, Some(1));
        assert_eq!(r.recv().await, Some(2));
        assert_eq!(r.recv().await, Some(3));
        assert_eq!(r.recv().await, None);
    })
}

#[test]
fn len() {
    const COUNT: usize = 25_000;
    const CAP: usize = 1000;

    task::block_on(async {
        let (s, r) = piper::chan(CAP);

        assert_eq!(s.len(), 0);
        assert_eq!(r.len(), 0);

        for _ in 0..CAP / 10 {
            for i in 0..50 {
                s.send(i).await;
                assert_eq!(s.len(), i + 1);
            }

            for i in 0..50 {
                r.recv().await;
                assert_eq!(r.len(), 50 - i - 1);
            }
        }

        assert_eq!(s.len(), 0);
        assert_eq!(r.len(), 0);

        for i in 0..CAP {
            s.send(i).await;
            assert_eq!(s.len(), i + 1);
        }

        for _ in 0..CAP {
            r.recv().await.unwrap();
        }

        assert_eq!(s.len(), 0);
        assert_eq!(r.len(), 0);

        let child = task::spawn({
            let r = r.clone();
            async move {
                for i in 0..COUNT {
                    assert_eq!(r.recv().await, Some(i));
                    let len = r.len();
                    assert!(len <= CAP);
                }
            }
        });

        for i in 0..COUNT {
            s.send(i).await;
            let len = s.len();
            assert!(len <= CAP);
        }

        child.await;

        assert_eq!(s.len(), 0);
        assert_eq!(r.len(), 0);
    })
}

#[test]
fn disconnect_wakes_receiver() {
    task::block_on(async {
        let (s, r) = channel::<()>(1);

        let child = task::spawn(async move {
            assert_eq!(r.recv().await, None);
        });

        task::sleep(ms(1000)).await;
        drop(s);

        child.await;
    })
}

#[test]
fn spsc() {
    const COUNT: usize = 100_000;

    task::block_on(async {
        let (s, r) = piper::chan(3);

        let child = task::spawn(async move {
            for i in 0..COUNT {
                assert_eq!(r.recv().await, Some(i));
            }
            assert_eq!(r.recv().await, None);
        });

        for i in 0..COUNT {
            s.send(i).await;
        }
        drop(s);

        child.await;
    })
}

#[test]
fn mpmc() {
    const COUNT: usize = 25_000;
    const TASKS: usize = 4;

    task::block_on(async {
        let (s, r) = channel::<usize>(3);
        let v = (0..COUNT).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();
        let v = Arc::new(v);

        let mut tasks = Vec::new();

        for _ in 0..TASKS {
            let r = r.clone();
            let v = v.clone();
            tasks.push(task::spawn(async move {
                for _ in 0..COUNT {
                    let n = r.recv().await.unwrap();
                    v[n].fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for _ in 0..TASKS {
            let s = s.clone();
            tasks.push(task::spawn(async move {
                for i in 0..COUNT {
                    s.send(i).await;
                }
            }));
        }

        for t in tasks {
            t.await;
        }

        for c in v.iter() {
            assert_eq!(c.load(Ordering::SeqCst), TASKS);
        }
    });
}

#[test]
fn oneshot() {
    const COUNT: usize = 10_000;

    task::block_on(async {
        for _ in 0..COUNT {
            let (s, r) = piper::chan(1);

            let c1 = task::spawn(async move { r.recv().await.unwrap() });
            let c2 = task::spawn(async move { s.send(0).await });

            c1.await;
            c2.await;
        }
    })
}

#[test]
fn drops() {
    const RUNS: usize = 100;

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, PartialEq)]
    struct DropCounter;

    impl Drop for DropCounter {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mut rng = thread_rng();

    for _ in 0..RUNS {
        task::block_on(async {
            let steps = rng.gen_range(0, 10_000);
            let additional = rng.gen_range(0, 50);

            DROPS.store(0, Ordering::SeqCst);
            let (s, r) = channel::<DropCounter>(50);

            let child = task::spawn({
                let r = r.clone();
                async move {
                    for _ in 0..steps {
                        r.recv().await.unwrap();
                    }
                }
            });

            for _ in 0..steps {
                s.send(DropCounter).await;
            }

            child.await;

            for _ in 0..additional {
                s.send(DropCounter).await;
            }

            assert_eq!(DROPS.load(Ordering::SeqCst), steps);
            drop(s);
            drop(r);
            assert_eq!(DROPS.load(Ordering::SeqCst), steps + additional);
        })
    }
}
