use std::thread;

use futures::future;
use smol::Task;

fn main() {
    // Start a thread pool.
    for _ in 0..num_cpus::get().max(1) {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    let val = smol::block_on(Task::spawn(async { 1 + 2 }));
    assert_eq!(val, 3);

    // Start a stoppable threadpool.
    // let mut pool = vec![];
    // for _ in 0..num_cpus::get().max(1) {
    //     let (s, r) = oneshot::channel<()>();
    //     pool.push(s);
    //     thread::spawn(|| smol::run(async move { drop(r.await) }));
    // }
    // drop(pool); // stops the threadpool!
}
