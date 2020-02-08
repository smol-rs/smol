use std::thread;

use futures::future;
use smol::Task;

fn main() {
    // Start a thread pool.
    for _ in 0..num_cpus::get().max(1) {
        thread::spawn(|| Task::run(future::pending::<()>()));
    }

    let val = Task::run(Task::schedule(async { 1 + 2 }));
    assert_eq!(val, 3);

    // Start a stoppable threadpool.
    // let mut pool = vec![];
    // for _ in 0..num_cpus::get().max(1) {
    //     let (s, r) = oneshot::channel<()>();
    //     pool.push(s);
    //     thread::spawn(|| Task::run(async move { drop(r.await) }));
    // }
    // drop(pool); // stops the threadpool!
}
