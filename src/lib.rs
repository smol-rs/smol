//! A small and fast async runtime.
//!
//! # Examples
//!
//! Connect to an HTTP website, make a GET request, and pipe the response to the standard output:
//!
//! ```
//! use async_net::TcpStream;
//! use smol::{io, prelude::*, Unblock};
//!
//! fn main() -> io::Result<()> {
//!     smol::run(async {
//!         let mut stream = TcpStream::connect("example.com:80").await?;
//!         let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";
//!         stream.write_all(req).await?;
//!
//!         let mut stdout = Unblock::new(std::io::stdout());
//!         io::copy(&stream, &mut stdout).await?;
//!         Ok(())
//!     })
//! }
//! ```
//!
//! This example uses [`async-net`] for networking, but you can also use the primitive [`Async`]
//! type. See the [full code][get-request].
//!
//! Look inside the [examples] directory for more.
//!
//! [`async-net`]: https://docs.rs/async-net
//! [examples]: https://github.com/stjepang/smol/tree/master/examples
//! [get-request]: https://github.com/stjepang/smol/blob/master/examples/get-request.rs
//!
//! # Compatibility
//!
//! All async libraries work with smol out of the box.
//!
//! However, [tokio] is generally hostile towards non-tokio libraries, insists on non-standard I/O
//! traits, and deliberately lacks documentation on integration with the larger Rust ecosystem.
//! Fortunately, there are ways around it.
//!
//! Enable the `tokio02` feature flag and [`smol::run()`][`crate::run()`] will create a minimal
//! tokio runtime for its libraries:
//!
//! ```toml
//! [dependencies]
//! smol = { version = "0.3", features = ["tokio02"] }
//! ```
//!
//! [tokio]: https://docs.rs/tokio

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use std::future::Future;

use async_executor::{Executor, LocalExecutor};
use easy_parallel::Parallel;

#[doc(inline)]
pub use {
    async_executor::Task,
    async_io::Async,
    async_io::Timer,
    blocking::{unblock, Unblock},
    futures_lite::{future, io, stream},
    futures_lite::{pin, ready},
};

/// Async traits and their extensions.
///
/// # Examples
///
/// ```
/// use smol::prelude::*;
/// ```
pub mod prelude {
    #[doc(no_inline)]
    pub use futures_lite::{
        future::Future,
        io::{AsyncBufRead, AsyncBufReadExt},
        io::{AsyncRead, AsyncReadExt},
        io::{AsyncSeek, AsyncSeekExt},
        io::{AsyncWrite, AsyncWriteExt},
        stream::{Stream, StreamExt},
    };
}

/// Starts an executor and runs the future on it.
///
/// This function runs two executors at the same time:
///
/// 1. The current thread runs a [`LocalExecutor`] and the main `future` on it.
/// 2. A thread pool runs an [`Executor`] until the main `future` completes.
///
/// The number of spawned threads matches the number of logical CPU cores on the system.
///
/// # Examples
///
/// ```
/// use smol::Task;
///
/// smol::run(async {
///     let task = Task::spawn(async {
///         println!("Hello world");
///     });
///     task.await;
/// })
/// ```
pub fn run<T>(future: impl Future<Output = T>) -> T {
    setup(num_cpus::get(), future)
}

#[cfg(not(feature = "tokio02"))]
fn setup<T>(num_threads: usize, future: impl Future<Output = T>) -> T {
    let ex = Executor::new();
    let local_ex = LocalExecutor::new();

    // A channel that coordinates shutdown when the main future completes.
    let (trigger, shutdown) = async_channel::unbounded::<()>();
    let future = async move {
        let _trigger = trigger; // Dropped at the end of this async block.
        future.await
    };

    Parallel::new()
        .each(0..num_threads, |_| ex.run(shutdown.recv()))
        .finish(|| ex.enter(|| local_ex.run(future)))
        .1
}

#[cfg(feature = "tokio02")]
fn setup<T>(num_threads: usize, future: impl Future<Output = T>) -> T {
    let ex = Executor::new();
    let local_ex = LocalExecutor::new();

    // A channel that signals shutdown to the thread pool when the main future completes.
    let (s, shutdown) = async_channel::unbounded::<()>();
    let future = async move {
        let _s = s; // Drops sender at the end of this async block.
        future.await
    };

    // A minimal tokio runtime.
    let mut rt = tokio::runtime::Builder::new()
        .enable_all()
        .basic_scheduler()
        .build()
        .expect("cannot start tokio runtime");
    let handle = rt.handle().clone();

    Parallel::new()
        .add(|| ex.enter(|| rt.block_on(shutdown.recv())))
        .each(0..num_threads, |_| handle.enter(|| ex.run(shutdown.recv())))
        .finish(|| handle.enter(|| ex.enter(|| local_ex.run(future))))
        .1
}
