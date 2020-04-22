//! Throttle tasks if they poll too many I/O operations without yielding.
//!
//! TODO

use std::cell::Cell;
use std::task::{Context, Poll};

use scoped_tls_hkt::scoped_thread_local;

// Number of times the current task is allowed to poll I/O operations.
//
// When this budget is used up, I/O operations will wake the current task and return
// `Poll::Pending`.
//
// This thread-local is set before running any task.
scoped_thread_local!(static BUDGET: Cell<u32>);

/// Sets an I/O budget for polling a future.
///
/// Once this budget is exceeded, polled I/O operations will always wake the current task and
/// return `Poll::Pending`.
///
/// We throttle I/O this way in order to prevent futures from running for
/// too long and thus starving other futures.
pub(crate) fn setup<T>(poll: impl FnOnce() -> T) -> T {
    // This is a fairly arbitrary number that seems to work well in practice.
    BUDGET.set(&Cell::new(200), poll)
}

/// Returns `Poll::Pending` if the I/O budget has been used up.
pub(crate) fn poll(cx: &mut Context<'_>) -> Poll<()> {
    // Decrement the budget and check if it was zero.
    if BUDGET.is_set() && BUDGET.with(|b| b.replace(b.get().saturating_sub(1))) == 0 {
        // Make sure to wake the current task. The task is not *really* pending, we're just
        // artificially throttling it to let other tasks be run.
        cx.waker().wake_by_ref();
        return Poll::Pending;
    }
    Poll::Ready(())
}
