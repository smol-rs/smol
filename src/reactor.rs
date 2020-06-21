//! The reactor notifying [`Async`][`crate::Async`] and [`Timer`][`crate::Timer`].
//!
//! There is a single global reactor that contains all registered I/O handles and timers. The
//! reactor is polled by the executor, i.e. the [`run()`][`crate::run()`] function.

#[cfg(not(any(
    target_os = "linux",     // epoll
    target_os = "android",   // epoll
    target_os = "illumos",   // epoll
    target_os = "macos",     // kqueue
    target_os = "ios",       // kqueue
    target_os = "freebsd",   // kqueue
    target_os = "netbsd",    // kqueue
    target_os = "openbsd",   // kqueue
    target_os = "dragonfly", // kqueue
    target_os = "windows",   // wepoll
)))]
compile_error!("reactor does not support this target OS");

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::io;
use std::mem;
#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(windows)]
use std::os::windows::io::RawSocket;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use concurrent_queue::ConcurrentQueue;
use futures_util::future;
use once_cell::sync::Lazy;
use slab::Slab;

/// The reactor.
///
/// Every async I/O handle and every timer is registered here. Invocations of
/// [`run()`][`crate::run()`] poll the reactor to check for new events every now and then.
///
/// There is only one global instance of this type, accessible by [`Reactor::get()`].
pub(crate) struct Reactor {
    /// Raw bindings to epoll/kqueue/wepoll.
    sys: sys::Reactor,

    /// Ticker bumped before polling.
    ticker: AtomicUsize,

    /// Registered sources.
    sources: Mutex<Slab<Arc<Source>>>,

    /// Temporary storage for I/O events when polling the reactor.
    events: Mutex<sys::Events>,

    /// An ordered map of registered timers.
    ///
    /// Timers are in the order in which they fire. The `usize` in this type is a timer ID used to
    /// distinguish timers that fire at the same time. The `Waker` represents the task awaiting the
    /// timer.
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,

    /// A queue of timer operations (insert and remove).
    ///
    /// When inserting or removing a timer, we don't process it immediately - we just push it into
    /// this queue. Timers actually get processed when the queue fills up or the reactor is polled.
    timer_ops: ConcurrentQueue<TimerOp>,
}

impl Reactor {
    /// Returns a reference to the reactor.
    pub fn get() -> &'static Reactor {
        static REACTOR: Lazy<Reactor> = Lazy::new(|| Reactor {
            sys: sys::Reactor::new().expect("cannot initialize I/O event notification"),
            ticker: AtomicUsize::new(0),
            sources: Mutex::new(Slab::new()),
            events: Mutex::new(sys::Events::new()),
            timers: Mutex::new(BTreeMap::new()),
            timer_ops: ConcurrentQueue::bounded(1000),
        });
        &REACTOR
    }

    /// Notifies the thread blocked on the reactor.
    pub fn notify(&self) {
        self.sys.notify().expect("failed to notify reactor");
    }

    /// Registers an I/O source in the reactor.
    pub fn insert_io(
        &self,
        #[cfg(unix)] raw: RawFd,
        #[cfg(windows)] raw: RawSocket,
    ) -> io::Result<Arc<Source>> {
        let mut sources = self.sources.lock().unwrap();
        let vacant = sources.vacant_entry();

        // Create a source and register it.
        let key = vacant.key();
        self.sys.register(raw, key)?;

        let source = Arc::new(Source {
            raw,
            key,
            wakers: Mutex::new(Wakers {
                tick_readable: 0,
                tick_writable: 0,
                readers: Vec::new(),
                writers: Vec::new(),
            }),
        });
        Ok(vacant.insert(source).clone())
    }

    /// Deregisters an I/O source from the reactor.
    pub fn remove_io(&self, source: &Source) -> io::Result<()> {
        let mut sources = self.sources.lock().unwrap();
        sources.remove(source.key);
        self.sys.deregister(source.raw)
    }

    /// Registers a timer in the reactor.
    ///
    /// Returns the inserted timer's ID.
    pub fn insert_timer(&self, when: Instant, waker: &Waker) -> usize {
        // Generate a new timer ID.
        static ID_GENERATOR: AtomicUsize = AtomicUsize::new(1);
        let id = ID_GENERATOR.fetch_add(1, Ordering::Relaxed);

        // Push an insert operation.
        while self
            .timer_ops
            .push(TimerOp::Insert(when, id, waker.clone()))
            .is_err()
        {
            // Fire timers to drain the queue.
            self.fire_timers();
        }

        // Notify that a timer was added.
        self.notify();

        id
    }

    /// Deregisters a timer from the reactor.
    pub fn remove_timer(&self, when: Instant, id: usize) {
        // Push a remove operation.
        while self.timer_ops.push(TimerOp::Remove(when, id)).is_err() {
            // Fire timers to drain the queue.
            self.fire_timers();
        }
    }

    /// Attempts to lock the reactor.
    pub fn try_lock(&self) -> Option<ReactorLock<'_>> {
        self.events.try_lock().ok().map(|events| {
            let reactor = self;
            ReactorLock { reactor, events }
        })
    }

    /// Fires ready timers.
    ///
    /// Returns the duration until the next timer before this method was called.
    fn fire_timers(&self) -> Option<Duration> {
        let mut timers = self.timers.lock().unwrap();

        // Process timer operations, but no more than the queue capacity because otherwise we could
        // keep popping operations forever.
        for _ in 0..self.timer_ops.capacity().unwrap() {
            match self.timer_ops.pop() {
                Ok(TimerOp::Insert(when, id, waker)) => {
                    timers.insert((when, id), waker);
                }
                Ok(TimerOp::Remove(when, id)) => {
                    timers.remove(&(when, id));
                }
                Err(_) => break,
            }
        }

        let now = Instant::now();

        // Split timers into ready and pending timers.
        let pending = timers.split_off(&(now, 0));
        let ready = mem::replace(&mut *timers, pending);

        // Calculate the duration until the next event.
        let dur = if ready.is_empty() {
            // Duration until the next timer.
            timers
                .keys()
                .next()
                .map(|(when, _)| when.saturating_duration_since(now))
        } else {
            // Timers are about to fire right now.
            Some(Duration::from_secs(0))
        };

        // Drop the lock before waking.
        drop(timers);

        // Wake up tasks waiting on timers.
        for (_, waker) in ready {
            waker.wake();
        }

        dur
    }
}

/// A lock on the reactor.
pub(crate) struct ReactorLock<'a> {
    reactor: &'a Reactor,
    events: MutexGuard<'a, sys::Events>,
}

impl ReactorLock<'_> {
    /// Processes new events, blocking until the first event or the timeout.
    pub fn react(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        // Fire timers.
        let next_timer = self.reactor.fire_timers();

        // compute the timeout for blocking on I/O events.
        let timeout = match (next_timer, timeout) {
            (None, None) => None,
            (Some(t), None) | (None, Some(t)) => Some(t),
            (Some(a), Some(b)) => Some(a.min(b)),
        };

        // Bump the ticker before polling I/O.
        let tick = self
            .reactor
            .ticker
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);

        // Block on I/O events.
        match self.reactor.sys.wait(&mut self.events, timeout) {
            // No I/O events occurred.
            Ok(0) => {
                if timeout != Some(Duration::from_secs(0)) {
                    // The non-zero timeout was hit so fire ready timers.
                    self.reactor.fire_timers();
                }
                Ok(())
            }

            // At least one I/O event occurred.
            Ok(_) => {
                // Iterate over sources in the event list.
                let sources = self.reactor.sources.lock().unwrap();
                let mut ready = Vec::new();

                for ev in self.events.iter() {
                    // Check if there is a source in the table with this key.
                    if let Some(source) = sources.get(ev.key) {
                        let mut wakers = source.wakers.lock().unwrap();

                        // Wake readers if a readability event was emitted.
                        if ev.readable {
                            wakers.tick_readable = tick;
                            ready.append(&mut wakers.readers);
                        }

                        // Wake writers if a writability event was emitted.
                        if ev.writable {
                            wakers.tick_writable = tick;
                            ready.append(&mut wakers.writers);
                        }

                        // Re-register if there are still writers or
                        // readers. The can happen if e.g. we were
                        // previously interested in both readability and
                        // writability, but only one of them was emitted.
                        if !(wakers.writers.is_empty() && wakers.readers.is_empty()) {
                            self.reactor.sys.reregister(
                                source.raw,
                                source.key,
                                !wakers.readers.is_empty(),
                                !wakers.writers.is_empty(),
                            )?;
                        }
                    }
                }

                // Drop the lock before waking.
                drop(sources);

                // Wake up tasks waiting on I/O.
                for waker in ready {
                    waker.wake();
                }

                Ok(())
            }

            // The syscall was interrupted.
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(()),

            // An actual error occureed.
            Err(err) => Err(err),
        }
    }
}

/// A single timer operation.
enum TimerOp {
    Insert(Instant, usize, Waker),
    Remove(Instant, usize),
}

/// A registered source of I/O events.
#[derive(Debug)]
pub(crate) struct Source {
    /// Raw file descriptor on Unix platforms.
    #[cfg(unix)]
    pub(crate) raw: RawFd,

    /// Raw socket handle on Windows.
    #[cfg(windows)]
    pub(crate) raw: RawSocket,

    /// The key of this source obtained during registration.
    key: usize,

    /// Tasks interested in events on this source.
    wakers: Mutex<Wakers>,
}

/// Tasks interested in events on a source.
#[derive(Debug)]
struct Wakers {
    /// Last reactor tick that delivered a readability event.
    tick_readable: usize,

    /// Last reactor tick that delivered a writability event.
    tick_writable: usize,

    /// Tasks waiting for the next readability event.
    readers: Vec<Waker>,

    /// Tasks waiting for the next writability event.
    writers: Vec<Waker>,
}

impl Source {
    /// Waits until the I/O source is readable.
    pub(crate) async fn readable(&self) -> io::Result<()> {
        let mut ticks = None;

        future::poll_fn(|cx| {
            let mut wakers = self.wakers.lock().unwrap();

            // Check if the reactor has delivered a readability event.
            if let Some((a, b)) = ticks {
                // If `tick_readable` has changed to a value other than the old reactor tick, that
                // means a newer reactor tick has delivered a readability event.
                if wakers.tick_readable != a && wakers.tick_readable != b {
                    return Poll::Ready(Ok(()));
                }
            }

            // If there are no other readers, re-register in the reactor.
            if wakers.readers.is_empty() {
                Reactor::get().sys.reregister(
                    self.raw,
                    self.key,
                    true,
                    !wakers.writers.is_empty(),
                )?;
            }

            // Register the current task's waker if not present already.
            if wakers.readers.iter().all(|w| !w.will_wake(cx.waker())) {
                wakers.readers.push(cx.waker().clone());
            }

            // Remember the current ticks.
            if ticks.is_none() {
                ticks = Some((
                    Reactor::get().ticker.load(Ordering::SeqCst),
                    wakers.tick_readable,
                ));
            }

            Poll::Pending
        })
        .await
    }

    /// Waits until the I/O source is writable.
    pub(crate) async fn writable(&self) -> io::Result<()> {
        let mut ticks = None;

        future::poll_fn(|cx| {
            let mut wakers = self.wakers.lock().unwrap();

            // Check if the reactor has delivered a writability event.
            if let Some((a, b)) = ticks {
                // If `tick_writable` has changed to a value other than the old reactor tick, that
                // means a newer reactor tick has delivered a writability event.
                if wakers.tick_writable != a && wakers.tick_writable != b {
                    return Poll::Ready(Ok(()));
                }
            }

            // If there are no other writers, re-register in the reactor.
            if wakers.writers.is_empty() {
                Reactor::get().sys.reregister(
                    self.raw,
                    self.key,
                    !wakers.readers.is_empty(),
                    true,
                )?;
            }

            // Register the current task's waker if not present already.
            if wakers.writers.iter().all(|w| !w.will_wake(cx.waker())) {
                wakers.writers.push(cx.waker().clone());
            }

            // Remember the current ticks.
            if ticks.is_none() {
                ticks = Some((
                    Reactor::get().ticker.load(Ordering::SeqCst),
                    wakers.tick_writable,
                ));
            }

            Poll::Pending
        })
        .await
    }
}

/// Raw bindings to epoll (Linux, Android, illumos).
#[cfg(any(target_os = "linux", target_os = "android", target_os = "illumos"))]
mod sys {
    use std::convert::TryInto;
    use std::io;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    use crate::sys::epoll::{
        epoll_create1, epoll_ctl, epoll_wait, EpollEvent, EpollFlags, EpollOp,
    };

    macro_rules! syscall {
        ($fn:ident $args:tt) => {{
            let res = unsafe { libc::$fn $args };
            if res == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(res)
            }
        }};
    }

    pub struct Reactor {
        epoll_fd: RawFd,
        event_fd: RawFd,
    }
    impl Reactor {
        pub fn new() -> io::Result<Reactor> {
            let epoll_fd = epoll_create1()?;
            let event_fd = syscall!(eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK))?;
            let reactor = Reactor { epoll_fd, event_fd };
            reactor.register(event_fd, !0)?;
            reactor.reregister(event_fd, !0, true, false)?;
            Ok(reactor)
        }
        pub fn register(&self, fd: RawFd, key: usize) -> io::Result<()> {
            let flags = syscall!(fcntl(fd, libc::F_GETFL))?;
            syscall!(fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
            let ev = &mut EpollEvent::new(0, key as u64);
            epoll_ctl(self.epoll_fd, EpollOp::EpollCtlAdd, fd, Some(ev))
        }
        pub fn reregister(&self, fd: RawFd, key: usize, read: bool, write: bool) -> io::Result<()> {
            let mut flags = libc::EPOLLONESHOT;
            if read {
                flags |= read_flags();
            }
            if write {
                flags |= write_flags();
            }
            let ev = &mut EpollEvent::new(flags, key as u64);
            epoll_ctl(self.epoll_fd, EpollOp::EpollCtlMod, fd, Some(ev))
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            epoll_ctl(self.epoll_fd, EpollOp::EpollCtlDel, fd, None)
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms = timeout
                .map(|t| {
                    if t == Duration::from_millis(0) {
                        t
                    } else {
                        t.max(Duration::from_millis(1))
                    }
                })
                .and_then(|t| t.as_millis().try_into().ok())
                .unwrap_or(-1);
            events.len = epoll_wait(self.epoll_fd, &mut events.list, timeout_ms)?;

            let mut buf = [0u8; 8];
            let _ = syscall!(read(
                self.event_fd,
                &mut buf[0] as *mut u8 as *mut libc::c_void,
                buf.len()
            ));
            self.reregister(self.event_fd, !0, true, false)?;

            Ok(events.len)
        }
        pub fn notify(&self) -> io::Result<()> {
            let buf: [u8; 8] = 1u64.to_ne_bytes();
            let _ = syscall!(write(
                self.event_fd,
                &buf[0] as *const u8 as *const libc::c_void,
                buf.len()
            ));
            Ok(())
        }
    }
    fn read_flags() -> EpollFlags {
        libc::EPOLLIN | libc::EPOLLRDHUP | libc::EPOLLHUP | libc::EPOLLERR | libc::EPOLLPRI
    }
    fn write_flags() -> EpollFlags {
        libc::EPOLLOUT | libc::EPOLLHUP | libc::EPOLLERR
    }

    pub struct Events {
        list: Box<[EpollEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            let list = vec![EpollEvent::empty(); 1000].into_boxed_slice();
            let len = 0;
            Events { list, len }
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.list[..self.len].iter().map(|ev| Event {
                readable: (ev.events() & read_flags()) != 0,
                writable: (ev.events() & write_flags()) != 0,
                key: ev.data() as usize,
            })
        }
    }
    pub struct Event {
        pub readable: bool,
        pub writable: bool,
        pub key: usize,
    }
}

/// Raw bindings to kqueue (macOS, iOS, FreeBSD, NetBSD, OpenBSD, DragonFly BSD).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
mod sys {
    use std::io::{self, Read, Write};
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use crate::sys::event::{kevent_ts, kqueue, KEvent};

    macro_rules! syscall {
        ($fn:ident $args:tt) => {{
            let res = unsafe { libc::$fn $args };
            if res == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(res)
            }
        }};
    }

    pub struct Reactor {
        kqueue_fd: RawFd,
        read_stream: UnixStream,
        write_stream: UnixStream,
    }
    impl Reactor {
        pub fn new() -> io::Result<Reactor> {
            let kqueue_fd = kqueue()?;
            syscall!(fcntl(kqueue_fd, libc::F_SETFD, libc::FD_CLOEXEC))?;
            let (read_stream, write_stream) = UnixStream::pair()?;
            read_stream.set_nonblocking(true)?;
            write_stream.set_nonblocking(true)?;
            let reactor = Reactor {
                kqueue_fd,
                read_stream,
                write_stream,
            };
            reactor.reregister(reactor.read_stream.as_raw_fd(), !0, true, false)?;
            Ok(reactor)
        }
        pub fn register(&self, fd: RawFd, _key: usize) -> io::Result<()> {
            let flags = syscall!(fcntl(fd, libc::F_GETFL))?;
            syscall!(fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
            Ok(())
        }
        pub fn reregister(&self, fd: RawFd, key: usize, read: bool, write: bool) -> io::Result<()> {
            let mut read_flags = libc::EV_ONESHOT | libc::EV_RECEIPT;
            let mut write_flags = libc::EV_ONESHOT | libc::EV_RECEIPT;
            if read {
                read_flags |= libc::EV_ADD;
            } else {
                read_flags |= libc::EV_DELETE;
            }
            if write {
                write_flags |= libc::EV_ADD;
            } else {
                write_flags |= libc::EV_DELETE;
            }
            let udata = key as _;
            let changelist = [
                KEvent::new(fd as _, libc::EVFILT_READ, read_flags, 0, 0, udata),
                KEvent::new(fd as _, libc::EVFILT_WRITE, write_flags, 0, 0, udata),
            ];
            let mut eventlist = changelist;
            kevent_ts(self.kqueue_fd, &changelist, &mut eventlist, None)?;
            for ev in &eventlist {
                // Explanation for ignoring EPIPE: https://github.com/tokio-rs/mio/issues/582
                let (flags, data) = (ev.flags(), ev.data());
                if (flags & libc::EV_ERROR) == 1
                    && data != 0
                    && data != libc::ENOENT as _
                    && data != libc::EPIPE as _
                {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            let flags = libc::EV_DELETE | libc::EV_RECEIPT;
            let changelist = [
                KEvent::new(fd as _, libc::EVFILT_WRITE, flags, 0, 0, 0),
                KEvent::new(fd as _, libc::EVFILT_READ, flags, 0, 0, 0),
            ];
            let mut eventlist = changelist;
            kevent_ts(self.kqueue_fd, &changelist, &mut eventlist, None)?;
            for ev in &eventlist {
                let (flags, data) = (ev.flags(), ev.data());
                if (flags & libc::EV_ERROR == 1) && data != 0 && data != libc::ENOENT as _ {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout = timeout.map(|t| libc::timespec {
                tv_sec: t.as_secs() as libc::time_t,
                tv_nsec: t.subsec_nanos() as libc::c_long,
            });
            events.len = kevent_ts(self.kqueue_fd, &[], &mut events.list, timeout)?;

            while (&self.read_stream).read(&mut [0; 64]).is_ok() {}
            self.reregister(self.read_stream.as_raw_fd(), !0, true, false)?;

            Ok(events.len)
        }
        pub fn notify(&self) -> io::Result<()> {
            let _ = (&self.write_stream).write(&[1]);
            Ok(())
        }
    }

    pub struct Events {
        list: Box<[KEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            let flags = 0;
            let event = KEvent::new(0, 0, flags, 0, 0, 0);
            let list = vec![event; 1000].into_boxed_slice();
            let len = 0;
            Events { list, len }
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            // On some platforms, closing the read end of a pipe wakes up writers, but the
            // event is reported as EVFILT_READ with the EV_EOF flag.
            //
            // https://github.com/golang/go/commit/23aad448b1e3f7c3b4ba2af90120bde91ac865b4
            self.list[..self.len].iter().map(|ev| Event {
                readable: ev.filter() == libc::EVFILT_READ,
                writable: ev.filter() == libc::EVFILT_WRITE
                    || (ev.filter() == libc::EVFILT_READ && (ev.flags() & libc::EV_EOF) != 0),
                key: ev.udata() as usize,
            })
        }
    }
    pub struct Event {
        pub readable: bool,
        pub writable: bool,
        pub key: usize,
    }
}

/// Raw bindings to wepoll (Windows).
#[cfg(target_os = "windows")]
mod sys {
    use std::convert::TryInto;
    use std::io;
    use std::os::windows::io::{AsRawSocket, RawSocket};
    use std::time::Duration;

    use wepoll_sys_stjepang as we;
    use winapi::um::winsock2;

    macro_rules! syscall {
        ($fn:ident $args:tt) => {{
            let res = unsafe { we::$fn $args };
            if res == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(res)
            }
        }};
    }

    pub struct Reactor {
        handle: we::HANDLE,
    }
    unsafe impl Send for Reactor {}
    unsafe impl Sync for Reactor {}
    impl Reactor {
        pub fn new() -> io::Result<Reactor> {
            let handle = unsafe { we::epoll_create1(0) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Reactor { handle })
        }
        pub fn register(&self, sock: RawSocket, key: usize) -> io::Result<()> {
            unsafe {
                let mut nonblocking = true as libc::c_ulong;
                let res = winsock2::ioctlsocket(
                    sock as winsock2::SOCKET,
                    winsock2::FIONBIO,
                    &mut nonblocking,
                );
                if res != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let mut ev = we::epoll_event {
                events: 0,
                data: we::epoll_data { u64: key as u64 },
            };
            syscall!(epoll_ctl(
                self.handle,
                we::EPOLL_CTL_ADD as libc::c_int,
                sock as we::SOCKET,
                &mut ev,
            ))?;
            Ok(())
        }
        pub fn reregister(
            &self,
            sock: RawSocket,
            key: usize,
            read: bool,
            write: bool,
        ) -> io::Result<()> {
            let mut flags = we::EPOLLONESHOT;
            if read {
                flags |= READ_FLAGS;
            }
            if write {
                flags |= WRITE_FLAGS;
            }
            let mut ev = we::epoll_event {
                events: flags as u32,
                data: we::epoll_data { u64: key as u64 },
            };
            syscall!(epoll_ctl(
                self.handle,
                we::EPOLL_CTL_MOD as libc::c_int,
                sock as we::SOCKET,
                &mut ev,
            ))?;
            Ok(())
        }
        pub fn deregister(&self, sock: RawSocket) -> io::Result<()> {
            syscall!(epoll_ctl(
                self.handle,
                we::EPOLL_CTL_DEL as libc::c_int,
                sock as we::SOCKET,
                0 as *mut we::epoll_event,
            ))?;
            Ok(())
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms = match timeout {
                None => -1,
                Some(t) => {
                    if t == Duration::from_millis(0) {
                        0
                    } else {
                        t.max(Duration::from_millis(1))
                            .as_millis()
                            .try_into()
                            .unwrap_or(libc::c_int::max_value())
                    }
                }
            };
            events.len = syscall!(epoll_wait(
                self.handle,
                events.list.as_mut_ptr(),
                events.list.len() as libc::c_int,
                timeout_ms,
            ))? as usize;
            Ok(events.len)
        }
        pub fn notify(&self) -> io::Result<()> {
            unsafe {
                // This errors if a notification has already been posted, but that's okay.
                winapi::um::ioapiset::PostQueuedCompletionStatus(
                    self.handle as winapi::um::winnt::HANDLE,
                    0,
                    0,
                    0 as *mut _,
                );
            }
            Ok(())
        }
    }
    struct As(RawSocket);
    impl AsRawSocket for As {
        fn as_raw_socket(&self) -> RawSocket {
            self.0
        }
    }
    const READ_FLAGS: u32 =
        we::EPOLLIN | we::EPOLLRDHUP | we::EPOLLHUP | we::EPOLLERR | we::EPOLLPRI;
    const WRITE_FLAGS: u32 = we::EPOLLOUT | we::EPOLLHUP | we::EPOLLERR;

    pub struct Events {
        list: Box<[we::epoll_event]>,
        len: usize,
    }
    unsafe impl Send for Events {}
    unsafe impl Sync for Events {}
    impl Events {
        pub fn new() -> Events {
            let ev = we::epoll_event {
                events: 0,
                data: we::epoll_data { u64: 0 },
            };
            Events {
                list: vec![ev; 1000].into_boxed_slice(),
                len: 0,
            }
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.list[..self.len].iter().map(|ev| Event {
                readable: (ev.events & READ_FLAGS) != 0,
                writable: (ev.events & WRITE_FLAGS) != 0,
                key: unsafe { ev.data.u64 } as usize,
            })
        }
    }
    pub struct Event {
        pub readable: bool,
        pub writable: bool,
        pub key: usize,
    }
}
