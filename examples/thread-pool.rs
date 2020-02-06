fn main() {
    // Start a threadpool.
    // for _ in 0..num_cpus::get().max(1) {
    //     thread::spawn(|| smol::run(future::pending()));
    // }

    // Start a stoppable threadpool.
    // let mut pool = vec![];
    // for _ in 0..num_cpus::get().max(1) {
    //     let (s, r) = oneshot::channel<()>();
    //     pool.push(s);
    //     thread::spawn(|| smol::run(async move { drop(r.await) }));
    // }
    // drop(pool); // stops the threadpool!
}
