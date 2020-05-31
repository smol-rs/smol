//! A small and fast async runtime.
//!
//! # Executors
//!
//! There are three executors that poll futures:
//!
//! 1. Thread-local executor for tasks created by [`Task::local()`].
//! 2. Work-stealing executor for tasks created by [`Task::spawn()`].
//! 3. Blocking executor for tasks created by [`Task::blocking()`], [`blocking!`], [`iter()`],
//!    [`reader()`] and [`writer()`].
//!
//! Blocking executor is the only one that spawns threads on its own.
//!
//! See [here](fn.run.html#examples) for how to run executors on a single thread or on a thread
//! pool.
//!
//! # Reactor
//!
//! To wait for the next I/O event, the reactor calls [epoll] on Linux/Android, [kqueue] on
//! macOS/iOS/BSD, and [wepoll] on Windows.
//!
//! The [`Async`] type registers I/O handles in the reactor and is able to convert their blocking
//! operations into async operations.
//!
//! The [`Timer`] type registers timers in the reactor that will fire at the chosen points in
//! time.
//!
//! # Running
//!
//! Function [`run()`] simultaneously runs the thread-local executor, runs the work-stealing
//! executor, and polls the reactor for I/O events and timers. At least one thread has to be
//! calling [`run()`] in order for futures waiting on I/O and timers to get notified.
//!
//! If you want a multithreaded runtime, just call [`run()`] from multiple threads. See
//! [here](fn.run.html#examples) for an example.
//!
//! There is also [`block_on()`], which blocks the current thread until a future completes, but it
//! doesn't poll the reactor or run executors. When using [`block_on()`], make sure at least one
//! thread is calling [`run()`], or else I/O and timers will not work!
//!
//! Blocking tasks run in the background on a dedicated thread pool.
//!
//! # Examples
//!
//! Connect to a HTTP website, make a GET request, and pipe the response to the standard output:
//!
//! ```no_run
//! use futures::prelude::*;
//! use smol::Async;
//! use std::net::TcpStream;
//!
//! fn main() -> std::io::Result<()> {
//!     smol::run(async {
//!         let mut stream = Async::<TcpStream>::connect("example.com:80").await?;
//!         let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";
//!         stream.write_all(req).await?;
//!
//!         let mut stdout = smol::writer(std::io::stdout());
//!         futures::io::copy(&stream, &mut stdout).await?;
//!         Ok(())
//!     })
//! }
//! ```
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
//! [epoll]: https://en.wikipedia.org/wiki/Epoll
//! [kqueue]: https://en.wikipedia.org/wiki/Kqueue
//! [wepoll]: https://github.com/piscisaureus/wepoll
//!
//! [examples]: https://github.com/stjepang/smol/tree/master/examples
//! [async-h1]: https://docs.rs/async-h1
//! [hyper]: https://docs.rs/hyper
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

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod async_io;
mod block_on;
mod blocking;
mod context;
mod io_event;
mod reactor;
mod run;
mod sys;
mod task;
mod thread_local;
mod throttle;
mod timer;
mod work_stealing;

pub use self::blocking::{iter, reader, writer};
pub use async_io::Async;
pub use block_on::block_on;
pub use run::run;
pub use task::Task;
pub use timer::Timer;
