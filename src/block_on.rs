//! Implementation of [`block_on()`].
//!
//! This is equivalent to [`futures::executor::block_on()`], but slightly more efficient.
//!
//! The following blog post explains it in detail:
//! - https://stjepang.github.io/2020/01/25/build-your-own-block-on.html

use std::cell::RefCell;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use crossbeam::sync::Parker;

use crate::context;

/// Blocks on a single future.
///
/// This function polls the future in a loop, parking the current thread after each step to wait
/// until its waker is woken.
///
/// Unlike [`run`], it does not run executors or poll the reactor!
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
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    thread_local! {
        // Parker and waker associated with the current thread.
        static CACHE: RefCell<(Parker, Waker)> = {
            let parker = Parker::new();
            let unparker = parker.unparker().clone();
            let waker = async_task::waker_fn(move || unparker.unpark());
            RefCell::new((parker, waker))
        };
    }

    CACHE.with(|cache| {
        // Panic if `block_on()` is called recursively.
        let (parker, waker) = &mut *cache.try_borrow_mut().ok().expect("recursive `block_on()`");

        // If enabled, set up tokio before execution begins.
        context::enter(|| {
            futures::pin_mut!(future);
            let cx = &mut Context::from_waker(&waker);

            loop {
                match future.as_mut().poll(cx) {
                    Poll::Ready(output) => return output,
                    Poll::Pending => parker.park(),
                }
            }
        })
    })
}
