//! Implementation of [`Timer`].
//!
//! Timers are futures that fire at a predefined point in time.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use crate::reactor::Reactor;

/// Fires at the chosen point in time.
///
/// Timers are futures that output the [`Instant`] at which they fired.
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
///
/// Set a timeout on an I/O operation:
///
/// ```
/// use futures::future::Either;
/// use futures::io::{self, BufReader};
/// use futures::prelude::*;
/// use smol::Timer;
/// use std::time::Duration;
///
/// async fn timeout<T>(
///     dur: Duration,
///     f: impl Future<Output = io::Result<T>>,
/// ) -> io::Result<T> {
///     futures::pin_mut!(f);
///     match future::select(f, Timer::after(dur)).await {
///         Either::Left((out, _)) => out,
///         Either::Right(_) => Err(io::ErrorKind::TimedOut.into()),
///     }
/// }
///
/// # smol::run(async {
/// // Create a buffered stdin reader.
/// let mut stdin = BufReader::new(smol::reader(std::io::stdin()));
///
/// // Read a line within 5 seconds.
/// let mut line = String::new();
/// timeout(Duration::from_secs(5), stdin.read_line(&mut line)).await?;
/// # io::Result::Ok(()) });
/// ```
#[derive(Debug)]
pub struct Timer {
    /// This timer's registration in the reactor.
    registration: Option<(usize, Waker)>,

    /// When this timer fires.
    when: Instant,
}

impl Timer {
    /// Fires after the specified duration of time.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Timer;
    /// use std::time::Duration;
    ///
    /// # smol::run(async {
    /// Timer::after(Duration::from_secs(1)).await;
    /// # });
    /// ```
    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }

    /// Fires at the specified instant in time.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Timer;
    /// use std::time::{Duration, Instant};
    ///
    /// # smol::run(async {
    /// let now = Instant::now();
    /// let when = now + Duration::from_secs(1);
    /// Timer::at(when).await;
    /// # });
    /// ```
    pub fn at(when: Instant) -> Timer {
        let registration = None;
        Timer { registration, when }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some((id, _)) = self.registration.take() {
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
            if let Some((id, _)) = self.registration.take() {
                // Deregister the timer from the reactor.
                Reactor::get().remove_timer(self.when, id);
            }
            Poll::Ready(self.when)
        } else {
            match self.registration {
                None => {
                    // Register the timer in the reactor.
                    let waker = cx.waker();
                    self.registration = Some((
                        Reactor::get().insert_new_timer(self.when, waker),
                        waker.clone(),
                    ));
                }
                Some((id, ref old_waker)) => {
                    let new_waker = cx.waker();
                    if !old_waker.will_wake(new_waker) {
                        // Reregister the timer to the new waker.
                        Reactor::get().insert_timer(self.when, id, new_waker);
                        self.registration = Some((id, new_waker.clone()));
                    }
                }
            }
            Poll::Pending
        }
    }
}
