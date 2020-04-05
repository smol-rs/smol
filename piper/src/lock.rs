use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use crate::signal::Signal;

use crossbeam_utils::Backoff;
use futures::io::{self, AsyncRead, AsyncWrite};

/// A lock that implements async I/O traits.
///
/// This is a blocking mutex that adds the following impls:
///
/// - `impl<T> AsyncRead for Lock<T> where &T: AsyncRead + Unpin {}`
/// - `impl<T> AsyncRead for &Lock<T> where &T: AsyncRead + Unpin {}`
/// - `impl<T> AsyncWrite for Lock<T> where &T: AsyncWrite + Unpin {}`
/// - `impl<T> AsyncWrite for &Lock<T> where &T: AsyncWrite + Unpin {}`
///
/// This lock is ensures fairness by handling lock operations in the first-in first-out order.
///
/// While primarily designed for wrapping async I/O objects, this lock can also be used as a
/// regular blocking mutex. It's not quite as efficient as [`parking_lot`], it's still a big
/// improvement over [`std::sync::Mutex`].
///
/// [`parking_lot`]: https://docs.rs/parking_lot
/// [`std::sync::Mutex`]: https://doc.rust-lang.org/std/sync/struct.Mutex.html
pub struct Lock<T> {
    /// Set to `true` when the lock is acquired by a `LockGuard`.
    locked: AtomicBool,

    /// Lock operations waiting for the mutex to get unlocked.
    lock_ops: Signal,

    /// The value inside the lock.
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    pub fn new(data: T) -> Self {
        Lock {
            locked: AtomicBool::new(false),
            lock_ops: Signal::new(),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> LockGuard<'_, T> {
        loop {
            // Try locking the lock.
            let backoff = Backoff::new();
            loop {
                if let Some(guard) = self.try_lock() {
                    return guard;
                }
                if backoff.is_completed() {
                    break;
                }
                backoff.snooze();
            }

            // Start watching for notifications and try locking again.
            let l = self.lock_ops.listen();
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            l.wait();
        }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
        if !self.locked.compare_and_swap(false, true, Ordering::Acquire) {
            Some(LockGuard(self))
        } else {
            None
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Lock<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.lock()).poll_read(cx, buf)
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for &Lock<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.lock()).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Lock<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.lock()).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.lock()).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.lock()).poll_close(cx)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for &Lock<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.lock()).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.lock()).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.lock()).poll_close(cx)
    }
}

pub struct LockGuard<'a, T>(&'a Lock<T>);

unsafe impl<T: Send> Send for LockGuard<'_, T> {}
unsafe impl<T: Sync> Sync for LockGuard<'_, T> {}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
        self.0.lock_ops.notify_one();
    }
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.data.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.data.get() }
    }
}

impl<T: fmt::Debug> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Locked;
        impl fmt::Debug for Locked {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("<locked>")
            }
        }

        match self.try_lock() {
            None => f.debug_struct("Lock").field("data", &Locked).finish(),
            Some(guard) => f.debug_struct("Lock").field("data", &&*guard).finish(),
        }
    }
}
