use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::reactor::Reactor;

/// Fires at the chosen point in time.
///
/// # Examples
///
/// Sleep for 1 second:
///
/// ```
/// use smol::Timer;
/// use std::time::Duration;
///
/// async fn sleep(dur: Duration) {
///     Timer::after(dur).await;
/// }
///
/// # smol::run(async {
/// sleep(Duration::from_secs(1)).await;
/// # });
/// ```
#[derive(Debug)]
pub struct Timer {
    /// A unique ID for this timer.
    ///
    /// When this field is set to `None`, this timer is not registered in the reactor.
    id: Option<u64>,

    /// When this timer fires.
    when: Instant,
}

impl Timer {
    /// Fires after the specified duration of time.
    ///
    /// TODO
    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }

    /// Fires at the specified instant in time.
    ///
    /// TODO
    pub fn at(when: Instant) -> Timer {
        let id = None;
        Timer { id, when }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // Deregister the timer from the reactor.
            Reactor::get().remove_timer(self.when, id);
        }
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Check if the timer has already fired.
        if Instant::now() >= self.when {
            if let Some(id) = self.id.take() {
                // Deregister the timer from the reactor.
                Reactor::get().remove_timer(self.when, id);
            }
            Poll::Ready(self.when)
        } else {
            if self.id.is_none() {
                // Register the timer in the reactor.
                let waker = cx.waker().clone();
                self.id = Some(Reactor::get().insert_timer(self.when, waker));
            }
            Poll::Pending
        }
    }
}
