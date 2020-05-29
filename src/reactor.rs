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
use std::os::windows::io::{FromRawSocket, RawSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;
use futures_util::future;
#[cfg(unix)]
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use once_cell::sync::Lazy;
use slab::Slab;
#[cfg(windows)]
use socket2::Socket;

use crate::io_event::IoEvent;

/// The reactor.
///
/// Every async I/O handle and every timer is registered here. Invocations of
/// [`run()`][`crate::run()`] poll the reactor to check for new events every now and then.
///
/// There is only one global instance of this type, accessible by [`Reactor::get()`].
pub(crate) struct Reactor {
    /// Raw bindings to epoll/kqueue/wepoll.
    sys: sys::Reactor,

    /// Registered sources.
    sources: piper::Mutex<Slab<Arc<Source>>>,

    /// Temporary storage for I/O events when polling the reactor.
    events: piper::Lock<sys::Events>,

    /// An ordered map of registered timers.
    ///
    /// Timers are in the order in which they fire. The `usize` in this type is a timer ID used to
    /// distinguish timers that fire at the same time. The `Waker` represents the task awaiting the
    /// timer.
    timers: piper::Mutex<BTreeMap<(Instant, usize), Waker>>,

    /// A queue of timer operations (insert and remove).
    ///
    /// When inserting or removing a timer, we don't process it immediately - we just push it into
    /// this queue. Timers actually get processed when the queue fills up or the reactor is polled.
    timer_ops: ArrayQueue<TimerOp>,

    /// An I/O event that is triggered when a new timer is registered.
    ///
    /// The reason why this field is lazily created is because `IoEvent`s can be created only after
    /// the reactor is fully initialized.
    timer_event: Lazy<IoEvent>,
}

impl Reactor {
    /// Returns a reference to the reactor.
    pub fn get() -> &'static Reactor {
        static REACTOR: Lazy<Reactor> = Lazy::new(|| Reactor {
            sys: sys::Reactor::new().expect("cannot initialize I/O event notification"),
            sources: piper::Mutex::new(Slab::new()),
            events: piper::Lock::new(sys::Events::new()),
            timers: piper::Mutex::new(BTreeMap::new()),
            timer_ops: ArrayQueue::new(1000),
            timer_event: Lazy::new(|| IoEvent::new().expect("cannot create an `IoEvent`")),
        });
        &REACTOR
    }

    /// Registers an I/O source in the reactor.
    pub fn insert_io(
        &self,
        #[cfg(unix)] raw: RawFd,
        #[cfg(windows)] raw: RawSocket,
    ) -> io::Result<Arc<Source>> {
        let mut sources = self.sources.lock();
        let vacant = sources.vacant_entry();

        // Put the I/O handle in non-blocking mode.
        #[cfg(unix)]
        {
            let flags = fcntl(raw, FcntlArg::F_GETFL).map_err(io_err)?;
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(raw, FcntlArg::F_SETFL(flags)).map_err(io_err)?;
        }
        #[cfg(windows)]
        {
            let socket = unsafe { Socket::from_raw_socket(raw) };
            mem::ManuallyDrop::new(socket).set_nonblocking(true)?;
        }

        // Create a source and register it.
        let key = vacant.key();
        self.sys.register(raw, key)?;

        let source = Arc::new(Source {
            raw,
            key,
            wakers: piper::Mutex::new(Wakers {
                readers: Vec::new(),
                writers: Vec::new(),
            }),
        });
        Ok(vacant.insert(source).clone())
    }

    /// Deregisters an I/O source from the reactor.
    pub fn remove_io(&self, source: &Source) -> io::Result<()> {
        let mut sources = self.sources.lock();
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
        self.timer_event.notify();

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

    /// Reset a timer to a new Instant.
    pub fn reset_timer(&self, when: Instant, id: usize, new: Instant) {
        // Push a reset operation.
        while self.timer_ops.push(TimerOp::Reset(when, id, new)).is_err() {
            // Fire timers to drain the queue.
            self.fire_timers();
        }
        // Notify if the new instant is sooner than the original.
        if new < when {
            self.timer_event.notify();
        }
    }

    /// Attempts to lock the reactor.
    pub fn try_lock(&self) -> Option<ReactorLock<'_>> {
        self.events.try_lock().map(|events| {
            let reactor = self;
            ReactorLock { reactor, events }
        })
    }

    /// Locks the reactor.
    pub async fn lock(&self) -> ReactorLock<'_> {
        let reactor = self;
        let events = self.events.lock().await;
        ReactorLock { reactor, events }
    }

    /// Fires ready timers.
    ///
    /// Returns the duration until the next timer before this method was called.
    fn fire_timers(&self) -> Option<Duration> {
        // Clear this event because we're about to fire timers.
        self.timer_event.clear();

        let mut timers = self.timers.lock();

        // Process timer operations, but no more than the queue capacity because otherwise we could
        // keep popping operations forever.
        for _ in 0..self.timer_ops.capacity() {
            match self.timer_ops.pop() {
                Ok(TimerOp::Insert(when, id, waker)) => {
                    timers.insert((when, id), waker);
                }
                Ok(TimerOp::Remove(when, id)) => {
                    timers.remove(&(when, id));
                }
                Ok(TimerOp::Reset(when, id, new)) => {
                    if let Some(waker) = timers.remove(&(when, id)) {
                        timers.insert((new, id), waker);
                    }
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
    events: piper::LockGuard<sys::Events>,
}

impl ReactorLock<'_> {
    /// Processes ready events without blocking.
    pub fn poll(&mut self) -> io::Result<()> {
        self.react(false)
    }

    /// Blocks until at least one event is processed.
    pub fn wait(&mut self) -> io::Result<()> {
        self.react(true)
    }

    /// Processes new events, optionally blocking until the first event.
    fn react(&mut self, block: bool) -> io::Result<()> {
        // Fire timers and compute the timeout for blocking on I/O events.
        let next_timer = self.reactor.fire_timers();
        let timeout = if block {
            next_timer
        } else {
            Some(Duration::from_secs(0))
        };

        loop {
            // Block on I/O events.
            match self.reactor.sys.wait(&mut self.events, timeout) {
                // The timeout was hit so fire ready timers.
                Ok(0) => {
                    self.reactor.fire_timers();
                    return Ok(());
                }

                // At least one I/O event occured.
                Ok(_) => {
                    // Iterate over sources in the event list.
                    let sources = self.reactor.sources.lock();

                    for ev in self.events.iter() {
                        // Check if there is a source in the table with this key.
                        if let Some(source) = sources.get(ev.key) {
                            let mut wakers = source.wakers.lock();

                            // Wake readers if a readability event was emitted.
                            if ev.readable {
                                for w in wakers.readers.drain(..) {
                                    w.wake();
                                }
                            }

                            // Wake writers if a writability event was emitted.
                            if ev.writable {
                                for w in wakers.writers.drain(..) {
                                    w.wake();
                                }
                            }
                        }
                    }

                    return Ok(());
                }

                // The syscall was interrupted.
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,

                // An actual error occureed.
                Err(err) => return Err(err),
            }
        }
    }
}

/// A single timer operation.
enum TimerOp {
    Insert(Instant, usize, Waker),
    Remove(Instant, usize),
    Reset(/* original */ Instant, usize, /* new */ Instant),
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
    wakers: piper::Mutex<Wakers>,
}

/// Tasks interested in events on a source.
#[derive(Debug)]
struct Wakers {
    /// Tasks waiting for the next readability event.
    readers: Vec<Waker>,

    /// Tasks waiting for the next writability event.
    writers: Vec<Waker>,
}

impl Source {
    /// Re-registers the I/O event to wake the poller.
    pub(crate) fn reregister_io_event(&self) -> io::Result<()> {
        let wakers = self.wakers.lock();
        Reactor::get()
            .sys
            .reregister(self.raw, self.key, true, !wakers.writers.is_empty())?;
        Ok(())
    }

    /// Waits until the I/O source is readable.
    ///
    /// This function may occasionally complete even if the I/O source is not readable.
    pub(crate) async fn readable(&self) -> io::Result<()> {
        let mut polled = false;

        future::poll_fn(|cx| {
            if polled {
                Poll::Ready(Ok(()))
            } else {
                let mut wakers = self.wakers.lock();

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

                polled = true;
                Poll::Pending
            }
        })
        .await
    }

    /// Waits until the I/O source is writable.
    ///
    /// This function may occasionally complete even if the I/O source is not writable.
    pub(crate) async fn writable(&self) -> io::Result<()> {
        let mut polled = false;

        future::poll_fn(|cx| {
            if polled {
                Poll::Ready(Ok(()))
            } else {
                let mut wakers = self.wakers.lock();

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

                polled = true;
                Poll::Pending
            }
        })
        .await
    }
}

/// Converts a [`nix::Error`] into [`io::Error`].
#[cfg(unix)]
fn io_err(err: nix::Error) -> io::Error {
    match err {
        nix::Error::Sys(code) => code.into(),
        err => io::Error::new(io::ErrorKind::Other, Box::new(err)),
    }
}

/// Raw bindings to epoll (Linux, Android, illumos).
#[cfg(any(target_os = "linux", target_os = "android", target_os = "illumos"))]
mod sys {
    use std::convert::TryInto;
    use std::io;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    use nix::sys::epoll::{
        epoll_create1, epoll_ctl, epoll_wait, EpollCreateFlags, EpollEvent, EpollFlags, EpollOp,
    };

    use super::io_err;

    pub struct Reactor(RawFd);
    impl Reactor {
        pub fn new() -> io::Result<Reactor> {
            let epoll_fd = epoll_create1(EpollCreateFlags::EPOLL_CLOEXEC).map_err(io_err)?;
            Ok(Reactor(epoll_fd))
        }
        pub fn register(&self, fd: RawFd, key: usize) -> io::Result<()> {
            let ev = &mut EpollEvent::new(EpollFlags::empty(), key as u64);
            epoll_ctl(self.0, EpollOp::EpollCtlAdd, fd, Some(ev)).map_err(io_err)
        }
        pub fn reregister(&self, fd: RawFd, key: usize, read: bool, write: bool) -> io::Result<()> {
            let mut flags = EpollFlags::EPOLLONESHOT;
            if read {
                flags |= read_flags();
            }
            if write {
                flags |= write_flags();
            }
            let ev = &mut EpollEvent::new(flags, key as u64);
            epoll_ctl(self.0, EpollOp::EpollCtlMod, fd, Some(ev)).map_err(io_err)
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            epoll_ctl(self.0, EpollOp::EpollCtlDel, fd, None).map_err(io_err)
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
            events.len = epoll_wait(self.0, &mut events.list, timeout_ms).map_err(io_err)?;
            Ok(events.len)
        }
    }
    fn read_flags() -> EpollFlags {
        EpollFlags::EPOLLIN | EpollFlags::EPOLLRDHUP
    }
    fn write_flags() -> EpollFlags {
        EpollFlags::EPOLLOUT
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
                readable: ev.events().intersects(read_flags()),
                writable: ev.events().intersects(write_flags()),
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
    use std::io;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    use nix::errno::Errno;
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    use nix::libc;
    use nix::sys::event::{kevent_ts, kqueue, EventFilter, EventFlag, FilterFlag, KEvent};

    use super::io_err;

    pub struct Reactor(RawFd);
    impl Reactor {
        pub fn new() -> io::Result<Reactor> {
            let fd = kqueue().map_err(io_err)?;
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io_err)?;
            Ok(Reactor(fd))
        }
        pub fn register(&self, _fd: RawFd, _key: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn reregister(&self, fd: RawFd, key: usize, read: bool, write: bool) -> io::Result<()> {
            let mut read_flags = EventFlag::EV_ONESHOT | EventFlag::EV_RECEIPT;
            let mut write_flags = EventFlag::EV_ONESHOT | EventFlag::EV_RECEIPT;
            if read {
                read_flags |= EventFlag::EV_ADD;
            } else {
                read_flags |= EventFlag::EV_DELETE;
            }
            if write {
                write_flags |= EventFlag::EV_ADD;
            } else {
                write_flags |= EventFlag::EV_DELETE;
            }
            let udata = key as _;
            let changelist = [
                KEvent::new(
                    fd as _,
                    EventFilter::EVFILT_READ,
                    read_flags,
                    FFLAGS,
                    0,
                    udata,
                ),
                KEvent::new(
                    fd as _,
                    EventFilter::EVFILT_WRITE,
                    write_flags,
                    FFLAGS,
                    0,
                    udata,
                ),
            ];
            let mut eventlist = changelist;
            kevent_ts(self.0, &changelist, &mut eventlist, None).map_err(io_err)?;
            for ev in &eventlist {
                // Explanation for ignoring EPIPE: https://github.com/tokio-rs/mio/issues/582
                let (flags, data) = (ev.flags(), ev.data());
                if flags.contains(EventFlag::EV_ERROR)
                    && data != 0
                    && data != Errno::ENOENT as _
                    && data != Errno::EPIPE as _
                {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            let flags = EventFlag::EV_RECEIPT | EventFlag::EV_DELETE;
            let changelist = [
                KEvent::new(fd as _, EventFilter::EVFILT_WRITE, flags, FFLAGS, 0, 0),
                KEvent::new(fd as _, EventFilter::EVFILT_READ, flags, FFLAGS, 0, 0),
            ];
            let mut eventlist = changelist;
            kevent_ts(self.0, &changelist, &mut eventlist, None).map_err(io_err)?;
            for ev in &eventlist {
                let (flags, data) = (ev.flags(), ev.data());
                if flags.contains(EventFlag::EV_ERROR) && data != 0 && data != Errno::ENOENT as _ {
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
            events.len = kevent_ts(self.0, &[], &mut events.list, timeout).map_err(io_err)?;
            Ok(events.len)
        }
    }
    const FFLAGS: FilterFlag = FilterFlag::empty();

    pub struct Events {
        list: Box<[KEvent]>,
        len: usize,
    }
    impl Events {
        pub fn new() -> Events {
            let flags = EventFlag::empty();
            let event = KEvent::new(0, EventFilter::EVFILT_USER, flags, FFLAGS, 0, 0);
            let list = vec![event; 1000].into_boxed_slice();
            let len = 0;
            Events { list, len }
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.list[..self.len].iter().map(|ev| Event {
                readable: ev.filter() == EventFilter::EVFILT_READ,
                writable: ev.filter() == EventFilter::EVFILT_WRITE,
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
    use std::io;
    use std::os::windows::io::{AsRawSocket, RawSocket};
    use std::time::Duration;

    use wepoll_binding::{Epoll, EventFlag};

    pub struct Reactor(Epoll);
    impl Reactor {
        pub fn new() -> io::Result<Reactor> {
            Ok(Reactor(Epoll::new()?))
        }
        pub fn register(&self, sock: RawSocket, key: usize) -> io::Result<()> {
            self.0.register(&As(sock), EventFlag::empty(), key as u64)
        }
        pub fn reregister(
            &self,
            sock: RawSocket,
            key: usize,
            read: bool,
            write: bool,
        ) -> io::Result<()> {
            let mut flags = EventFlag::ONESHOT;
            if read {
                flags |= read_flags();
            }
            if write {
                flags |= write_flags();
            }
            self.0.reregister(&As(sock), flags, key as u64)
        }
        pub fn deregister(&self, sock: RawSocket) -> io::Result<()> {
            self.0.deregister(&As(sock))
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout = timeout.map(|t| {
                if t == Duration::from_millis(0) {
                    t
                } else {
                    t.max(Duration::from_millis(1))
                }
            });
            events.0.clear();
            self.0.poll(&mut events.0, timeout)
        }
    }
    struct As(RawSocket);
    impl AsRawSocket for As {
        fn as_raw_socket(&self) -> RawSocket {
            self.0
        }
    }
    fn read_flags() -> EventFlag {
        EventFlag::IN | EventFlag::RDHUP
    }
    fn write_flags() -> EventFlag {
        EventFlag::OUT
    }

    pub struct Events(wepoll_binding::Events);
    impl Events {
        pub fn new() -> Events {
            Events(wepoll_binding::Events::with_capacity(1000))
        }
        pub fn iter(&self) -> impl Iterator<Item = Event> + '_ {
            self.0.iter().map(|ev| Event {
                readable: ev.flags().intersects(read_flags()),
                writable: ev.flags().intersects(write_flags()),
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
