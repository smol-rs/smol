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
//! The only exception is [tokio], which is traditionally incompatible with [futures] and crashes
//! when called from other executors. Fortunately, there are ways around it.
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
//! [futures]: https://docs.rs/futures

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use std::env;
use std::future::Future;

use async_executor::{Executor, LocalExecutor};
use cfg_if::cfg_if;
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
        future::{Future, FutureExt},
        io::{AsyncBufRead, AsyncBufReadExt},
        io::{AsyncRead, AsyncReadExt},
        io::{AsyncSeek, AsyncSeekExt},
        io::{AsyncWrite, AsyncWriteExt},
        stream::{Stream, StreamExt},
    };
}

/// Starts a thread-local executor and then runs the future.
///
/// # Examples
///
/// ```
/// use smol::Task;
///
/// smol::block_on(async {
///     let task = Task::local(async {
///         println!("Hello world");
///     });
///     task.await;
/// })
/// ```
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    let local_ex = LocalExecutor::new();

    cfg_if! {
        if #[cfg(not(feature = "tokio02"))] {
            local_ex.run(future)
        } else {
            // A minimal tokio runtime to support libraries depending on it.
            let mut rt = tokio::runtime::Builder::new()
                .enable_all()
                .basic_scheduler()
                .build()
                .expect("cannot start tokio runtime");
            let handle = rt.handle().clone();

            // A channel that coordinates shutdown when the main future completes.
            let (trigger, shutdown) = async_channel::unbounded::<()>();
            let future = async move {
                let _trigger = trigger; // Dropped at the end of this async block.
                future.await
            };

            Parallel::new()
                .add(|| rt.block_on(shutdown.recv()))
                .finish(|| handle.enter(|| local_ex.run(future)))
                .1
        }
    }
}

/// Starts a thread-local and a multi-threaded executor and then runs the future.
///
/// This function runs two executors at the same time:
///
/// 1. The current thread runs a [`LocalExecutor`] and the main `future` on it.
/// 2. A thread pool runs an [`Executor`] until the main `future` completes.
///
/// The number of spawned threads matches the number of logical CPU cores on the system, but it can
/// be overriden by setting the `SMOL_THREADS` environment variable.
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
    // A channel that coordinates shutdown when the main future completes.
    let (trigger, shutdown) = async_channel::unbounded::<()>();
    let future = async move {
        let _trigger = trigger; // Dropped at the end of this async block.
        future.await
    };

    let num_threads = {
        // Parse SMOL_THREADS or use the number of CPU cores on the system.
        env::var("SMOL_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| num_cpus::get())
    };

    let ex = Executor::new();
    let local_ex = LocalExecutor::new();

    cfg_if! {
        if #[cfg(not(feature = "tokio02"))] {
            Parallel::new()
                .each(0..num_threads, |_| ex.run(shutdown.recv()))
                .finish(|| ex.enter(|| local_ex.run(future)))
                .1
        } else {
            // A minimal tokio runtime to support libraries depending on it.
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
    }
}
