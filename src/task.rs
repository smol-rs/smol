//! The task system.
//!
//! A [`Task`] handle represents a spawned future that is run by the executor.

use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

#[cfg(target_arch = "wasm32")]
use core::marker::PhantomData;

#[cfg(not(target_arch = "wasm32"))]
use crate::blocking::BlockingExecutor;
#[cfg(target_arch = "wasm32")]
use crate::web::WebExecutor;
#[cfg(not(target_arch = "wasm32"))]
use crate::thread_local::ThreadLocalExecutor;
#[cfg(not(target_arch = "wasm32"))]
use crate::work_stealing::WorkStealingExecutor;

/// A runnable future, ready for execution.
///
/// When a future is internally spawned using `async_task::spawn()` or `async_task::spawn_local()`,
/// we get back two values:
///
/// 1. an `async_task::Task<()>`, which we refer to as a `Runnable`
/// 2. an `async_task::JoinHandle<T, ()>`, which is wrapped inside a `Task<T>`
///
/// Once a `Runnable` is run, it "vanishes" and only reappears when its future is woken. When it's
/// woken up, its schedule function is called; in smol, that means that `Runnable` will be pushed
/// onto a queue in an executor.
pub(crate) type Runnable = async_task::Task<()>;

/// A spawned future.
///
/// Tasks are also futures themselves and yield the output of the spawned future.
///
/// When a task is dropped, its gets canceled and won't be polled again. To cancel a task a bit
/// more gracefully and wait until it stops running, use the [`cancel()`][Task::cancel()] method.
///
/// Tasks that panic get immediately canceled. Awaiting a canceled task also causes a panic.
///
/// If the future panics, the panic will be unwound into the [`run()`] invocation that polled it.
/// However, this does not apply to the blocking executor - it will simply ignore panics and
/// continue running.
///
/// # Examples
///
/// ```
/// use smol::Task;
///
/// # smol::run(async {
/// // Spawn a task onto the work-stealing executor.
/// let task = Task::spawn(async {
///     println!("Hello from a task!");
///     1 + 2
/// });
///
/// // Wait for the task to complete.
/// assert_eq!(task.await, 3);
/// # });
/// ```
///
/// [`run()`]: crate::run()
#[must_use = "tasks get canceled when dropped, use `.detach()` to run them in the background"]
#[derive(Debug)]
#[cfg(not(target_arch = "wasm32"))]
pub struct Task<T>(
    pub(crate) Option<async_task::JoinHandle<T, ()>>
);

#[must_use = "tasks get canceled when dropped, use `.detach()` to run them in the background"]
#[derive(Debug)]
#[cfg(target_arch = "wasm32")]
pub struct Task<T>(
    pub(crate) PhantomData<T>
);

impl<T: 'static> Task<T> {
    /// Spawns a future onto the thread-local executor.
    ///
    /// Panics if the current thread is not inside an invocation of [`run()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Task;
    ///
    /// # smol::run(async {
    /// let task = Task::local(async { 1 + 2 });
    /// assert_eq!(task.await, 3);
    /// # })
    /// ```
    ///
    /// [`run()`]: crate::run()
    #[cfg(not(target_arch = "wasm32"))]
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T> {
        ThreadLocalExecutor::spawn(future)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T> {
        WebExecutor::spawn(future)
    }
}

impl<T: Send + 'static> Task<T> {
    /// Spawns a future onto the work-stealing executor.
    ///
    /// This future may be stolen and polled by any thread calling [`run()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Task;
    ///
    /// # smol::run(async {
    /// let task = Task::spawn(async { 1 + 2 });
    /// assert_eq!(task.await, 3);
    /// # });
    /// ```
    ///
    /// [`run()`]: crate::run()
    #[cfg(not(target_arch = "wasm32"))]
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        WorkStealingExecutor::get().spawn(future)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        Task::local(future)
    }

    /// Spawns a future onto the blocking executor.
    ///
    /// This future is allowed to block for an indefinite length of time.
    ///
    /// For convenience, there is also the [`blocking!`] macro that spawns a blocking tasks and
    /// immediately awaits it.
    ///
    /// # Examples
    ///
    /// Read a line from the standard input:
    ///
    /// ```no_run
    /// use smol::Task;
    /// use std::io::stdin;
    ///
    /// # smol::block_on(async {
    /// let line = Task::blocking(async {
    ///     let mut line = String::new();
    ///     std::io::stdin().read_line(&mut line).unwrap();
    ///     line
    /// })
    /// .await;
    /// # });
    /// ```
    ///
    /// See also examples for [`blocking!`], [`iter()`], [`reader()`], and [`writer()`].
    ///
    /// [`iter()`]: `crate::iter()`
    /// [`reader()`]: `crate::reader()`
    /// [`writer()`]: `crate::writer()`
    #[cfg(not(target_arch = "wasm32"))]
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        BlockingExecutor::get().spawn(future)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn blocking(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        Task::local(future)
    }
}

impl<T, E> Task<Result<T, E>>
where
    T: Send + 'static,
    E: Debug + Send + 'static,
{
    /// Spawns a new task that awaits and unwraps the result.
    ///
    /// The new task will panic if the original task results in an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::{Async, Task};
    /// use std::net::TcpStream;
    ///
    /// # smol::run(async {
    /// let stream = Task::spawn(async {
    ///     Async::<TcpStream>::connect("example.com:80").await
    /// })
    /// .unwrap()
    /// .await;
    /// # })
    /// ```
    #[cfg(not(target_arch="wasm32"))]
    pub fn unwrap(self) -> Task<T> {
        Task::spawn(async { self.await.unwrap() })
    }

    /// Spawns a new task that awaits and unwraps the result.
    ///
    /// The new task will panic with the provided message if the original task results in an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::{Async, Task};
    /// use std::net::TcpStream;
    ///
    /// # smol::run(async {
    /// let stream = Task::spawn(async {
    ///     Async::<TcpStream>::connect("example.com:80").await
    /// })
    /// .expect("cannot connect")
    /// .await;
    /// # })
    /// ```
    #[cfg(not(target_arch="wasm32"))]
    pub fn expect(self, msg: &str) -> Task<T> {
        let msg = msg.to_owned();
        Task::spawn(async move { self.await.expect(&msg) })
    }
}

impl Task<()> {
    /// Detaches the task to let it keep running in the background.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::{Task, Timer};
    /// use std::time::Duration;
    ///
    /// # smol::run(async {
    /// Task::spawn(async {
    ///     loop {
    ///         println!("I'm a daemon task looping forever.");
    ///         Timer::after(Duration::from_secs(1)).await;
    ///     }
    /// })
    /// .detach();
    /// # })
    /// ```
    #[cfg(not(target_arch="wasm32"))]
    pub fn detach(mut self) {
        self.0.take().unwrap();
    }

    #[cfg(target_arch="wasm32")]
    pub fn detach(self) { }
}

impl<T> Task<T> {
    /// Cancels the task and waits for it to stop running.
    ///
    /// Returns the task's output if it was completed just before it got canceled, or `None` if it
    /// didn't complete.
    ///
    /// While it's possible to simply drop the [`Task`] to cancel it, this is a cleaner way of
    /// canceling because it also waits for the task to stop running.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::{Task, Timer};
    /// use std::time::Duration;
    ///
    /// # smol::run(async {
    /// let task = Task::spawn(async {
    ///     loop {
    ///         println!("Even though I'm in an infinite loop, you can still cancel me!");
    ///         Timer::after(Duration::from_secs(1)).await;
    ///     }
    /// });
    ///
    /// Timer::after(Duration::from_secs(3)).await;
    /// task.cancel().await;
    /// # })
    /// ```
    #[cfg(not(target_arch="wasm32"))]
    pub async fn cancel(self) -> Option<T> {
        // There's a bug in rustdoc causing it to render `mut self` as `__arg0: Self`, so we just
        // do `{ self }` here to avoid marking `self` as mutable.
        let handle = { self }.0.take().unwrap();
        handle.cancel();
        handle.await
    }
}

#[cfg(not(target_arch="wasm32"))]
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.cancel();
        }
    }
}

#[cfg(not(target_arch="wasm32"))]
impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0.as_mut().unwrap()).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(output) => Poll::Ready(output.expect("task has failed")),
        }
    }
}

#[cfg(not(target_arch="wasm32"))]
impl<T> Into<async_task::JoinHandle<T, ()>> for Task<T> {
    fn into(mut self) -> async_task::JoinHandle<T, ()> {
        self.0
            .take()
            .expect("task was already canceled or has failed")
    }
}
