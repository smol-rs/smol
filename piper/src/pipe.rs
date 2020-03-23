use std::cell::Cell;
use std::io;
use std::mem;
use std::pin::Pin;
use std::slice;
use std::sync::atomic::{self, AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite};
use futures::task::AtomicWaker;

pub fn pipe(cap: usize) -> (Reader, Writer) {
    assert!(cap > 0, "capacity must be positive");

    let mut v = Vec::with_capacity(cap);
    let buffer = v.as_mut_ptr();
    mem::forget(v);

    let inner = Arc::new(Inner {
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        reader: AtomicWaker::new(),
        writer: AtomicWaker::new(),
        closed: AtomicBool::new(false),
        buffer,
        cap,
    });

    let r = Reader {
        inner: inner.clone(),
        head: Cell::new(0),
        tail: Cell::new(0),
    };

    let w = Writer {
        inner,
        head: Cell::new(0),
        tail: Cell::new(0),
    };

    (r, w)
}

// NOTE: Reader and Writer are !Clone + !Sync

pub struct Reader {
    inner: Arc<Inner>,
    head: Cell<usize>,
    tail: Cell<usize>,
}

pub struct Writer {
    inner: Arc<Inner>,
    head: Cell<usize>,
    tail: Cell<usize>,
}

unsafe impl Send for Reader {}
unsafe impl Send for Writer {}

struct Inner {
    head: AtomicUsize,
    tail: AtomicUsize,
    reader: AtomicWaker,
    writer: AtomicWaker,
    closed: AtomicBool,
    buffer: *mut u8,
    cap: usize,
}

impl Inner {
    #[inline]
    unsafe fn slot(&self, pos: usize) -> *mut u8 {
        if pos < self.cap {
            self.buffer.add(pos)
        } else {
            self.buffer.add(pos - self.cap)
        }
    }

    #[inline]
    fn advance(&self, pos: usize, by: usize) -> usize {
        if by < 2 * self.cap - pos {
            pos + by
        } else {
            pos - self.cap + by - self.cap
        }
    }

    #[inline]
    fn distance(&self, a: usize, b: usize) -> usize {
        if a <= b {
            b - a
        } else {
            2 * self.cap - a + b
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            Vec::from_raw_parts(self.buffer, 0, self.cap);
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.writer.wake();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.reader.wake();
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self).poll_read(cx, buf)
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self).poll_close(cx)
    }
}

impl AsyncRead for &Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let mut head = self.head.get();
        let mut tail = self.tail.get();

        if self.inner.distance(head, tail) == 0 {
            tail = self.inner.tail.load(Ordering::Acquire);
            self.tail.set(tail);

            if self.inner.distance(head, tail) == 0 {
                self.inner.reader.register(cx.waker());
                atomic::fence(Ordering::SeqCst);

                tail = self.inner.tail.load(Ordering::Acquire);
                self.tail.set(tail);

                if self.inner.distance(head, tail) == 0 {
                    if self.inner.closed.load(Ordering::Relaxed) {
                        return Poll::Ready(Ok(0));
                    } else {
                        return Poll::Pending;
                    }
                }

                self.inner.reader.take();
            }
        }

        let mut count = 0;

        loop {
            self.inner.writer.wake();

            let streak = if head < self.inner.cap {
                self.inner.cap - head
            } else {
                2 * self.inner.cap - head
            };
            let remaining = self.inner.distance(head, tail);
            let space = buf.len() - count;

            let n = streak.min(remaining).min(space).min(16 * 1024);
            if n == 0 {
                break;
            }

            let slice = unsafe { slice::from_raw_parts(self.inner.slot(head), n) };
            buf[count..count + n].copy_from_slice(slice);
            count += n;
            head = self.inner.advance(head, n);

            // Store the current head pointer.
            self.inner.head.store(head, Ordering::Release);
            self.head.set(head);
        }

        Poll::Ready(Ok(count))
    }
}

impl AsyncWrite for &Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() || self.inner.closed.load(Ordering::Relaxed) {
            return Poll::Ready(Ok(0));
        }

        let mut head = self.head.get();
        let mut tail = self.tail.get();

        if self.inner.distance(head, tail) == self.inner.cap {
            head = self.inner.head.load(Ordering::Acquire);
            self.head.set(head);

            if self.inner.distance(head, tail) == self.inner.cap {
                self.inner.writer.register(cx.waker());
                atomic::fence(Ordering::SeqCst);

                head = self.inner.head.load(Ordering::Acquire);
                self.head.set(head);

                if self.inner.distance(head, tail) == self.inner.cap {
                    if self.inner.closed.load(Ordering::Relaxed) {
                        return Poll::Ready(Ok(0));
                    } else {
                        return Poll::Pending;
                    }
                }

                self.inner.writer.take();
            }
        }

        let mut count = 0;

        loop {
            self.inner.reader.wake();

            let streak = if tail < self.inner.cap {
                self.inner.cap - tail
            } else {
                2 * self.inner.cap - tail
            };
            let remaining = buf.len() - count;
            let space = self.inner.cap - self.inner.distance(head, tail);

            let n = streak.min(remaining).min(space).min(16 * 1024);
            if n == 0 {
                break;
            }

            unsafe {
                self.inner
                    .slot(tail)
                    .copy_from_nonoverlapping(&buf[count], n);
            }
            count += n;
            tail = self.inner.advance(tail, n);

            // Store the current tail pointer.
            self.inner.tail.store(tail, Ordering::Release);
            self.tail.set(tail);
        }

        Poll::Ready(Ok(count))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
