//! An I/O object for waking up threads blocked on the reactor.
//!
//! We use the self-pipe trick explained [here](https://cr.yp.to/docs/selfpipe.html).
//!
//! On Unix systems, the self-pipe is a pair of unnamed connected sockets. On Windows, the
//! self-pipe is a pair of TCP sockets connected over localhost.

use std::io::{self, Read, Write};
#[cfg(windows)]
use std::net::SocketAddr;
use std::sync::atomic::{self, Ordering};
use std::sync::Arc;

#[cfg(not(target_os = "linux"))]
use socket2::{Domain, Socket, Type};

use crate::async_io::Async;

#[cfg(not(target_os = "linux"))]
type Notifier = Socket;

#[cfg(target_os = "linux")]
type Notifier = linux::EventFd;

/// A self-pipe.
struct Inner {
    /// The writer side, emptied by `clear()`.
    writer: Notifier,

    /// The reader side, filled by `notify()`.
    reader: Async<Notifier>,
}

/// A flag that that triggers an I/O event whenever it is set.
#[derive(Clone)]
pub(crate) struct IoEvent(Arc<Inner>);

impl IoEvent {
    /// Creates a new `IoEvent`.
    pub fn new() -> io::Result<IoEvent> {
        let (writer, reader) = notifier()?;

        Ok(IoEvent(Arc::new(Inner {
            writer,
            reader: Async::new(reader)?,
        })))
    }

    /// Sets the flag to `true`.
    #[cfg(not(windows))]
    pub fn notify(&self) {
        // Publish all in-memory changes before setting the flag.
        atomic::fence(Ordering::SeqCst);

        // Trigger an I/O event by writing a byte into the sending socket.
        let _ = (&self.0.writer).write(&1u64.to_ne_bytes());
        let _ = (&self.0.writer).flush();

        // Re-register to wake up the poller.
        let _ = self.0.reader.reregister_io_event();
    }

    /// Sets the flag to `false`.
    #[cfg(not(windows))]
    pub fn clear(&self) {
        // Read all available bytes from the receiving socket.
        while self.0.reader.get_ref().read(&mut [0; 64]).is_ok() {}

        // Publish all in-memory changes after clearing the flag.
        atomic::fence(Ordering::SeqCst);
    }

    #[cfg(windows)]
    pub fn notify(&self) {
        use std::os::windows::io::*;
        use std::mem::*;
        use winapi::um::ioapiset::PostQueuedCompletionStatus;
        use winapi::um::minwinbase::OVERLAPPED;
        use miow::iocp::*;
        use miow::*;

        let reactor = crate::reactor::Reactor::get();
        let handle = reactor.sys.0.as_raw_handle();

        atomic::fence(Ordering::SeqCst);
        unsafe {
            let port = ManuallyDrop::new(CompletionPort::from_raw_handle(handle));
            let status = CompletionStatus::zero();
            port.post(status);

            // PostQueuedCompletionStatus(
            //     0,
            //     0,
            //     zero as *const OVERLAPPED as *mut OVERLAPPED,
            //     );
        }
    }

    #[cfg(windows)]
    pub fn clear(&self) {
        atomic::fence(Ordering::SeqCst);
    }
}

/// Creates a pair of connected sockets.
#[cfg(all(unix, not(target_os = "linux")))]
fn notifier() -> io::Result<(Socket, Socket)> {
    let (sock1, sock2) = Socket::pair(Domain::unix(), Type::stream(), None)?;
    sock1.set_nonblocking(true)?;
    sock2.set_nonblocking(true)?;

    sock1.set_send_buffer_size(1)?;
    sock2.set_recv_buffer_size(1)?;

    Ok((sock1, sock2))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::sys::eventfd::eventfd;
    use crate::sys::unistd;
    use std::os::unix::io::AsRawFd;

    pub(crate) struct EventFd(std::os::unix::io::RawFd);

    impl EventFd {
        pub fn new() -> Result<Self, std::io::Error> {
            let fd = eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK)?;
            Ok(EventFd(fd))
        }

        pub fn try_clone(&self) -> Result<EventFd, io::Error> {
            unistd::dup(self.0).map(EventFd)
        }
    }

    impl AsRawFd for EventFd {
        fn as_raw_fd(&self) -> i32 {
            self.0
        }
    }

    impl Drop for EventFd {
        fn drop(&mut self) {
            let _ = unistd::close(self.0);
        }
    }

    impl Read for &EventFd {
        #[inline]
        fn read(&mut self, buf: &mut [u8]) -> std::result::Result<usize, std::io::Error> {
            unistd::read(self.0, buf)
        }
    }

    impl Write for &EventFd {
        #[inline]
        fn write(&mut self, buf: &[u8]) -> std::result::Result<usize, std::io::Error> {
            unistd::write(self.0, buf)
        }

        #[inline]
        fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
            Ok(())
        }
    }
}

/// Creates eventfd on linux.
#[cfg(target_os = "linux")]
fn notifier() -> io::Result<(Notifier, Notifier)> {
    use linux::EventFd;
    let sock1 = EventFd::new()?;
    let sock2 = sock1.try_clone()?;
    Ok((sock1, sock2))
}

/// Creates a pair of connected sockets.
#[cfg(windows)]
fn notifier() -> io::Result<(Notifier, Notifier)> {
    // Create a temporary listener.
    let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    listener.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
    listener.listen(1)?;

    // First socket: start connecting to the listener.
    let sock1 = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    sock1.set_nonblocking(true)?;
    let _ = sock1.set_nodelay(true)?;
    let _ = sock1.connect(&listener.local_addr()?);

    // Second socket: accept a connection from the listener.
    let (sock2, _) = listener.accept()?;
    sock2.set_nonblocking(true)?;
    let _ = sock2.set_nodelay(true)?;

    sock1.set_send_buffer_size(1)?;
    sock2.set_recv_buffer_size(1)?;

    Ok((sock1, sock2))
}
