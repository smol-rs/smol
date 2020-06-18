//! Implementation of [`block_on()`].
//!
//! This is equivalent to [`futures::executor::block_on()`], but slightly more efficient.
//!
//! The implementation is explained in detail in [*Build your own block_on()*][blog-post].
//!
//! [`futures::executor::block_on()`]: https://docs.rs/futures/0.3/futures/executor/fn.block_on.html
//! [blog-post]: https://stjepang.github.io/2020/01/25/build-your-own-block-on.html

use std::future::Future;

use crate::context;

/// Blocks on a single future.
///
/// This function polls the future in a loop, parking the current thread after each step to wait
/// until its waker is woken.
///
/// Unlike [`run()`], it does not run executors or poll the reactor!
///
/// You can think of it as the easiest and most efficient way of turning an async operation into a
/// blocking operation.
///
/// # Examples
///
/// ```
/// use futures::future;
/// use smol::{Async, Timer};
/// use std::thread;
/// use std::time::Duration;
///
/// // Run executors and the reactor on a separeate thread, forever.
/// thread::spawn(|| smol::run(future::pending::<()>()));
///
/// smol::block_on(async {
///     // Sleep for a second.
///     // This timer only works because there's a thread calling `run()`.
///     Timer::after(Duration::from_secs(1)).await;
/// })
/// ```
///
/// [`run()`]: `crate::run()`
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    context::enter(|| blocking::block_on(future))
}
