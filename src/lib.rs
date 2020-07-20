//! A small and fast async runtime.
//!
//! This library provides:
//!
//! * Tools for working with [`future`]s, [`stream`]s, and async [I/O][`io`].
//! * Hooks into epoll/kqueue/wepoll for [`Async`] I/O and [`Timer`]s.
//! * Glue between async and blocking code: [`block_on()`], [`BlockOn`], [`unblock()`], [`Unblock`].
//! * An executor for spawning [`Task`]s.
//!
//! The whole implementation is a trivial amount of code - it mostly reexports types and functions
//! from other small and independent crates.
//!
//! The focus of the crate is on simplicity
//!
//! # Examples
//!
//! A simple TCP server that prints messages received from clients:
//!
//! ```no_run
//! use smol::{block_on, io, Async, Task, Unblock};
//! use std::net::TcpListener;
//!
//! fn main() -> io::Result<()> {
//!     block_on(async {
//!         // Start listening on port 9000.
//!         let listener = Async::<TcpListener>::bind(([127, 0, 0, 1], 9000))?;
//!
//!         loop {
//!             // Accept a new client.
//!             let (stream, _) = listener.accept().await?;
//!
//!             // Spawn a task handling this client.
//!             let task = Task::spawn(async move {
//!                 // Create an async stdio handle.
//!                 let mut stdout = Unblock::new(std::io::stdout());
//!
//!                 // Copy data received from the client into stdout.
//!                 io::copy(&stream, &mut stdout).await
//!             });
//!
//!             // Keep running the task in the background.
//!             task.detach();
//!         }
//!     })
//! }
//! ```
//!
//! To interact with the server, run `nc 127.0.0.1 9000` and type a few lines of text.
//!
//! ### More examples
//!
//! Look inside the [examples] directory for more:
//! a [web crawler][web-crawler],
//! a [Ctrl-C handler][ctrl-c],
//! a TCP [client][tcp-client]/[server][tcp-server],
//! a TCP chat [client][chat-client]/[server][chat-server],
//! a TLS [client][tls-client]/[server][tls-server],
//! an HTTP+TLS [client][simple-client]/[server][simple-server],
//! an [async-h1] [client][async-h1-client]/[server][async-h1-server],
//! a [hyper] [client][hyper-client]/[server][hyper-server],
//! and a WebSocket+TLS [client][websocket-client]/[server][websocket-server].
//!
//! It's also possible to plug non-async libraries into the runtime: see
//! [inotify], [timerfd], [signal-hook], and [uds_windows].
//!
//! Finally, there's an [example][other-runtimes] showing how to use smol with
//! [async-std], [tokio], [surf], and [reqwest].
//!
//! [examples]: https://github.com/stjepang/smol/tree/master/examples/!
//! [async-h1]: https://docs.rs/async-h1
//! [hyper]: https://docs.rs/hyper
//! [hyper]: https://docs.rs/tokio
//! [async-std]: https://docs.rs/async-std
//! [tokio]: https://docs.rs/tokio
//! [surf]: https://docs.rs/surf
//! [reqwest]: https://docs.rs/reqwest
//!
//! [async-h1-client]: https://github.com/stjepang/smol/blob/master/examples/async-h1-client.rs
//! [async-h1-server]: https://github.com/stjepang/smol/blob/master/examples/async-h1-server.rs
//! [chat-client]: https://github.com/stjepang/smol/blob/master/examples/chat-client.rs
//! [chat-server]: https://github.com/stjepang/smol/blob/master/examples/chat-server.rs
//! [ctrl-c]: https://github.com/stjepang/smol/blob/master/examples/ctrl-c.rs
//! [hyper-client]: https://github.com/stjepang/smol/blob/master/examples/hyper-client.rs
//! [hyper-server]: https://github.com/stjepang/smol/blob/master/examples/hyper-server.rs
//! [inotify]: https://github.com/stjepang/smol/blob/master/examples/linux-inotify.rs
//! [other-runtimes]: https://github.com/stjepang/smol/blob/master/examples/other-runtimes.rs
//! [signal-hook]: https://github.com/stjepang/smol/blob/master/examples/unix-signal.rs
//! [simple-client]: https://github.com/stjepang/smol/blob/master/examples/simple-client.rs
//! [simple-server]: https://github.com/stjepang/smol/blob/master/examples/simple-server.rs
//! [tcp-client]: https://github.com/stjepang/smol/blob/master/examples/tcp-client.rs
//! [tcp-server]: https://github.com/stjepang/smol/blob/master/examples/tcp-server.rs
//! [timerfd]: https://github.com/stjepang/smol/blob/master/examples/linux-timerfd.rs
//! [tls-client]: https://github.com/stjepang/smol/blob/master/examples/tls-client.rs
//! [tls-server]: https://github.com/stjepang/smol/blob/master/examples/tls-server.rs
//! [uds_windows]: https://github.com/stjepang/smol/blob/master/examples/windows-uds.rs
//! [web-crawler]: https://github.com/stjepang/smol/blob/master/examples/web-crawler.rs
//! [websocket-client]: https://github.com/stjepang/smol/blob/master/examples/websocket-client.rs
//! [websocket-server]: https://github.com/stjepang/smol/blob/master/examples/websocket-server.rs

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use std::future::Future;
use std::panic::catch_unwind;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use multitask::Executor;
use once_cell::sync::Lazy;

pub use {
    async_io::Async,
    async_io::Timer,
    blocking::{block_on, BlockOn},
    blocking::{unblock, Unblock},
    futures_lite::{future, io, stream},
    futures_lite::{pin, ready},
};

/// A spawned future.
///
/// Tasks are also futures themselves and yield the output of the spawned future.
///
/// When a task is dropped, its gets canceled and won't be polled again. To cancel a task a bit
/// more gracefully and wait until it stops running, use the [`cancel()`][Task::cancel()] method.
///
/// Tasks that panic get immediately canceled. Awaiting a canceled task also causes a panic.
///
/// # Examples
///
/// ```
/// use smol::Task;
///
/// # blocking::block_on(async {
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
#[must_use = "tasks get canceled when dropped, use `.detach()` to run them in the background"]
#[derive(Debug)]
pub struct Task<T>(multitask::Task<T>);

impl<T> Task<T> {
    /// Spawns a future.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Task;
    ///
    /// # blocking::block_on(async {
    /// let task = Task::spawn(async { 1 + 2 });
    /// assert_eq!(task.await, 3);
    /// # });
    /// ```
    pub fn spawn<F>(future: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        static EXECUTOR: Lazy<Executor> = Lazy::new(|| {
            for _ in 0..num_cpus::get().max(1) {
                thread::spawn(|| {
                    enter(|| {
                        let (p, u) = async_io::parking::pair();
                        let ticker = EXECUTOR.ticker(move || u.unpark());

                        loop {
                            if let Ok(false) = catch_unwind(|| ticker.tick()) {
                                p.park();
                            }
                        }
                    })
                });
            }

            Executor::new()
        });

        Task(EXECUTOR.spawn(future))
    }

    /// Detaches the task to let it keep running in the background.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use async_io::Timer;
    /// use smol::Task;
    /// use std::time::Duration;
    ///
    /// # blocking::block_on(async {
    /// Task::spawn(async {
    ///     loop {
    ///         println!("I'm a daemon task looping forever.");
    ///         Timer::new(Duration::from_secs(1)).await;
    ///     }
    /// })
    /// .detach();
    /// # })
    /// ```
    pub fn detach(self) {
        self.0.detach();
    }

    /// Cancels the task and waits for it to stop running.
    ///
    /// Returns the task's output if it was completed just before it got canceled, or [`None`] if
    /// it didn't complete.
    ///
    /// While it's possible to simply drop the [`Task`] to cancel it, this is a cleaner way of
    /// canceling because it also waits for the task to stop running.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_io::Timer;
    /// use smol::Task;
    /// use std::time::Duration;
    ///
    /// # blocking::block_on(async {
    /// let task = Task::spawn(async {
    ///     loop {
    ///         println!("Even though I'm in an infinite loop, you can still cancel me!");
    ///         Timer::new(Duration::from_secs(1)).await;
    ///     }
    /// });
    ///
    /// Timer::new(Duration::from_secs(3)).await;
    /// task.cancel().await;
    /// # })
    /// ```
    pub async fn cancel(self) -> Option<T> {
        self.0.cancel().await
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// Enters the tokio context if the `tokio` feature is enabled.
fn enter<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(not(feature = "tokio02"))]
    return f();

    #[cfg(feature = "tokio02")]
    {
        use std::cell::Cell;
        use tokio::runtime::Runtime;

        thread_local! {
            /// The level of nested `enter` calls we are in, to ensure that the outermost always
            /// has a runtime spawned.
            static NESTING: Cell<usize> = Cell::new(0);
        }

        /// The global tokio runtime.
        static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("cannot initialize tokio"));

        NESTING.with(|nesting| {
            let res = if nesting.get() == 0 {
                nesting.replace(1);
                RT.enter(f)
            } else {
                nesting.replace(nesting.get() + 1);
                f()
            };
            nesting.replace(nesting.get() - 1);
            res
        })
    }
}
