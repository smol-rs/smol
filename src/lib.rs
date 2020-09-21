//! A small and fast async runtime.
//!
//! This crate simply re-exports other smaller async crates (see the source).
//!
//! To use tokio-based libraries with smol, apply the [`async-compat`] adapter to futures and I/O
//! types.
//!
//! # Examples
//!
//! Connect to an HTTP website, make a GET request, and pipe the response to the standard output:
//!
//! ```
//! use smol::{io, net, prelude::*, Unblock};
//!
//! fn main() -> io::Result<()> {
//!     smol::block_on(async {
//!         let mut stream = net::TcpStream::connect("example.com:80").await?;
//!         let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";
//!         stream.write_all(req).await?;
//!
//!         let mut stdout = Unblock::new(std::io::stdout());
//!         io::copy(stream, &mut stdout).await?;
//!         Ok(())
//!     })
//! }
//! ```
//!
//! There's a lot more in the [examples] directory.
//!
//! [`async-compat`]: https://docs.rs/async-compat
//! [examples]: https://github.com/stjepang/smol/tree/master/examples
//! [get-request]: https://github.com/stjepang/smol/blob/master/examples/get-request.rs

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

#[cfg(doctest)]
doc_comment::doctest!("../README.md");

use std::future::Future;
use std::panic::catch_unwind;
use std::thread;

use once_cell::sync::Lazy;

#[doc(inline)]
pub use {
    async_executor::{Executor, LocalExecutor, Task},
    async_io::{block_on, Async, Timer},
    blocking::{unblock, Unblock},
    futures_lite::{future, io, stream},
    futures_lite::{pin, ready},
};

#[doc(inline)]
pub use {
    async_channel as channel, async_fs as fs, async_lock as lock, async_net as net,
    async_process as process,
};

pub mod prelude {
    //! Traits [`Future`], [`Stream`], [`AsyncRead`], [`AsyncWrite`], [`AsyncBufRead`],
    //! [`AsyncSeek`], and their extensions.
    //!
    //! # Examples
    //!
    //! ```
    //! use smol::prelude::*;
    //! ```

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

/// Spawns a task onto the global executor (single-threaded by default).
///
/// There is a global executor that gets lazily initialized on first use. It is included in this
/// library for convenience when writing unit tests and small programs, but it is otherwise
/// more advisable to create your own [`Executor`].
///
/// By default, the global executor is run by a single background thread, but you can also
/// configure the number of threads by setting the `SMOL_THREADS` environment variable.
///
/// # Examples
///
/// ```
/// let task = smol::spawn(async {
///     1 + 2
/// });
///
/// smol::block_on(async {
///     assert_eq!(task.await, 3);
/// });
/// ```
pub fn spawn<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
    static GLOBAL: Lazy<Executor<'_>> = Lazy::new(|| {
        let num_threads = {
            // Parse SMOL_THREADS or default to 1.
            std::env::var("SMOL_THREADS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1)
        };

        for n in 1..=num_threads {
            thread::Builder::new()
                .name(format!("smol-{}", n))
                .spawn(|| {
                    loop {
                        catch_unwind(|| async_io::block_on(GLOBAL.run(future::pending::<()>())))
                            .ok();
                    }
                })
                .expect("cannot spawn executor thread");
        }

        Executor::new()
    });

    GLOBAL.spawn(future)
}
