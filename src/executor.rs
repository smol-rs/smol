use std::future::Future;
use std::panic::catch_unwind;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use crossbeam::channel;
use once_cell::sync::Lazy;

// Runs the future to completion on a new worker (have Mutex<Vec<Stealer>>)
// there will be no hidden threadpool!!
pub fn run<F, R>(future: F) -> JoinHandle<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let handle = spawn(future);
    todo!("run tasks from the queue until handle completes")

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

/// A queue that holds scheduled tasks.
static QUEUE: Lazy<channel::Sender<Task>> = Lazy::new(|| {
    // Create a queue.
    let (sender, receiver) = channel::unbounded::<Task>();

    // Spawn executor threads the first time the queue is created.
    for _ in 0..num_cpus::get().max(1) {
        let receiver = receiver.clone();
        thread::spawn(move || {
            receiver.iter().for_each(|task| {
                let _ = catch_unwind(|| task.run());
            })
        });
    }

    sender
});

/// A spawned future and its current state.
type Task = async_task::Task<()>;

/// Spawns a future on the executor.
pub fn spawn<F, R>(future: F) -> JoinHandle<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    // Create a task and schedule it for execution.
    let (task, handle) = async_task::spawn(future, |t| QUEUE.send(t).unwrap(), ());
    task.schedule();

    // Return a join handle that retrieves the output of the future.
    JoinHandle(handle)
}

/// Awaits the output of a spawned future.
pub struct JoinHandle<R>(async_task::JoinHandle<R, ()>);

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task failed")),
        }
    }
}
