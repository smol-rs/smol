//! The reactor, async I/O, and timers.

#[cfg(not(any(
    target_os = "linux",     // epoll
    target_os = "android",   // epoll
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::io::RawFd;

#[cfg(windows)]
use std::os::windows::io::RawSocket;

use once_cell::sync::Lazy;
use slab::Slab;

use crate::io_event::IoEvent;
use crate::throttle;

/// The reactor driving I/O events and timers.
///
/// Every async I/O handle ("source") and every timer is registered here. Invocations of `run()`
/// poll the reactor to check for new events every now and then.
///
/// There is only one global instance of this type, accessible by `Reactor::get()`.
pub(crate) struct Reactor {
    /// Raw bindings to epoll/kqueue/wepoll.
    sys: sys::Reactor,

    /// Registered sources.
    sources: piper::Lock<Slab<Arc<Source>>>,

    /// Temporary storage for I/O events when polling the reactor.
    events: piper::Mutex<sys::Events>,

    /// An ordered map of registered timers.
    ///
    /// Timers are in the order in which they fire. The `u64` in this type is a unique timer ID
    /// used to distinguish timers that fire at the same time. The `Waker` represents the task
    /// awaiting the timer.
    timers: piper::Lock<BTreeMap<(Instant, u64), Waker>>,

    /// An I/O event that is triggered when a new earliest timer is registered.
    ///
    /// This is used to wake up the thread waiting on the reactor, which would otherwise wait until
    /// the previously earliest timer.
    ///
    /// The reason why this field is lazily created is because `IoEvent`s can be created only after
    /// the reactor is fully initialized.
    event: Lazy<IoEvent>,
}

impl Reactor {
    /// Returns a reference to the reactor.
    pub fn get() -> &'static Reactor {
        static REACTOR: Lazy<Reactor> = Lazy::new(|| Reactor {
            sys: sys::Reactor::new().expect("cannot initialize I/O event notification"),
            sources: piper::Lock::new(Slab::new()),
            events: piper::Mutex::new(sys::Events::new()),
            timers: piper::Lock::new(BTreeMap::new()),
            event: Lazy::new(|| IoEvent::new().expect("cannot create an `IoEvent`")),
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

        #[cfg(unix)]
        {
            use nix::fcntl::{fcntl, FcntlArg, OFlag};

            // Put the I/O handle in non-blocking mode.
            let flags = fcntl(raw, FcntlArg::F_GETFL).map_err(io_err)?;
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(raw, FcntlArg::F_SETFL(flags)).map_err(io_err)?;
        }

        #[cfg(windows)]
        {
            // Put the I/O handle in non-blocking mode.
            let socket = unsafe { Socket::from_raw_socket(raw) };
            mem::ManuallyDrop::new(socket).set_nonblocking(true)?;
        }

        // Create a source and register it.
        let source = Arc::new(Source {
            raw,
            key: vacant.key(),
            wakers: piper::Lock::new(Vec::new()),
            tick: AtomicU64::new(0),
        });
        self.sys.register(raw, source.key)?;

        Ok(vacant.insert(source).clone())
    }

    /// Deregisters an I/O source from the reactor.
    pub fn remove_io(&self, source: &Source) -> io::Result<()> {
        let mut sources = self.sources.lock();
        sources.remove(source.key);
        self.sys.deregister(source.raw)
    }

    /// TODO
    pub fn insert_timer(&self, when: Instant, waker: Waker) -> u64 {
        let mut timers = self.timers.lock();

        // If this timer is going to be the earliest one, interrupt the reactor.
        if let Some((first, _)) = timers.keys().next() {
            if when < *first {
                self.event.set();
            }
        }

        // Generate a new ID.
        static ID_GENERATOR: AtomicU64 = AtomicU64::new(1);
        let id = ID_GENERATOR.fetch_add(1, Ordering::Relaxed);
        assert!(id < u64::max_value() / 2, "exhausted timer IDs");

        // Insert this timer into the timers map.
        timers.insert((when, id), waker);
        id
    }

    /// TODO
    pub fn remove_timer(&self, when: Instant, id: u64) {
        self.timers.lock().remove(&(when, id));
    }

    /// Processes ready events without blocking.
    ///
    /// This doesn't have strong guarantees. If there are ready events, they may or may not be
    /// processed depending on whether the reactor is locked.
    pub fn poll(&self) -> io::Result<()> {
        if let Some(events) = self.events.try_lock() {
            let reactor = self;
            let mut lock = ReactorLock { reactor, events };
            // React to events without blocking.
            lock.react(false)?;
        }
        Ok(())
    }

    /// Locks the reactor.
    pub async fn lock(&self) -> ReactorLock<'_> {
        let reactor = self;
        let events = self.events.lock().await;
        ReactorLock { reactor, events }
    }
}

/// Polls the reactor for I/O events and wakes up tasks.
pub(crate) struct ReactorLock<'a> {
    reactor: &'a Reactor,
    events: piper::MutexGuard<'a, sys::Events>,
}

impl ReactorLock<'_> {
    /// Blocks until at least one event is processed.
    pub fn wait(&mut self) -> io::Result<()> {
        self.react(true)
    }

    fn react(&mut self, block: bool) -> io::Result<()> {
        // TODO: document this function.......................
        self.reactor.event.clear();

        let next_timer = {
            // Split timers into ready and pending timers.
            let mut timers = self.reactor.timers.lock();
            let pending = timers.split_off(&(Instant::now(), 0));
            let ready = mem::replace(&mut *timers, pending);

            // Wake up tasks waiting on timers.
            for (_, waker) in ready {
                waker.wake();
            }
            // Find when the next timer fires.
            timers.keys().next().map(|(when, _)| *when)
        };

        let timeout = if block {
            // Calculate the timeout till the first timer fires.
            next_timer.map(|when| when.saturating_duration_since(Instant::now()))
        } else {
            // If this poll doesn't block, the timeout is zero.
            Some(Duration::from_secs(0))
        };

        // Block on I/O events.
        loop {
            match self.reactor.sys.wait(&mut self.events, timeout) {
                Ok(0) => return Ok(()),
                Ok(_) => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }

        // Iterate over sources in the event list.
        let sources = self.reactor.sources.lock();
        for source in self.events.iter().filter_map(|i| sources.get(i)) {
            // I/O events may deregister sources, so we need to re-register.
            self.reactor.sys.reregister(source.raw, source.key)?;

            let mut wakers = source.wakers.lock();
            let tick = source.tick.load(Ordering::Acquire);
            source.tick.store(tick.wrapping_add(1), Ordering::Release);

            // Wake up tasks waiting on I/O.
            for w in wakers.drain(..) {
                w.wake();
            }
        }
        Ok(())
    }
}

/// A registered source of I/O events.
#[derive(Debug)]
pub(crate) struct Source {
    /// Raw file descriptor on Unix platforms.
    #[cfg(unix)]
    raw: RawFd,

    /// Raw socket handle on Windows.
    #[cfg(windows)]
    raw: RawSocket,

    /// The ID of this source obtain during registration.
    key: usize,

    /// A list of wakers representing tasks interested in events on this source.
    wakers: piper::Lock<Vec<Waker>>,

    /// Incremented on every I/O notification - this is only used for synchronization.
    tick: AtomicU64,
}

impl Source {
    /// Attempts a non-blocking I/O operation and registers a waker if it errors with `WouldBlock`.
    pub fn poll_io<R>(
        &self,
        cx: &mut Context<'_>,
        mut op: impl FnMut() -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        // Throttle if the current task did too many I/O operations without yielding.
        futures::ready!(throttle::poll(cx));

        loop {
            // This number is bumped just before I/O notifications while wakers are locked.
            let tick = self.tick.load(Ordering::Acquire);

            // Attempt the non-blocking operation.
            match op() {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return Poll::Ready(res),
            }

            // Lock the waker list and retry the non-blocking operation.
            let mut wakers = self.wakers.lock();

            // If the current task is already registered, return.
            if wakers.iter().any(|w| w.will_wake(cx.waker())) {
                return Poll::Pending;
            }

            // If there were no new notifications, register and return.
            if self.tick.load(Ordering::Acquire) == tick {
                wakers.push(cx.waker().clone());
                return Poll::Pending;
            }
        }
    }
}

/// Converts a `nix::Error` into `std::io::Error`.
#[cfg(unix)]
fn io_err(err: nix::Error) -> io::Error {
    match err {
        nix::Error::Sys(code) => code.into(),
        err => io::Error::new(io::ErrorKind::Other, Box::new(err)),
    }
}

/// Bindings to epoll (Linux, Android).
#[cfg(any(target_os = "linux", target_os = "android"))]
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
            let ev = &mut EpollEvent::new(flags(), key as u64);
            epoll_ctl(self.0, EpollOp::EpollCtlAdd, fd, Some(ev)).map_err(io_err)
        }
        pub fn reregister(&self, _raw: RawFd, _key: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            epoll_ctl(self.0, EpollOp::EpollCtlDel, fd, None).map_err(io_err)
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms = timeout
                .and_then(|t| t.as_millis().try_into().ok())
                .unwrap_or(-1);
            events.len = epoll_wait(self.0, &mut events.list, timeout_ms).map_err(io_err)?;
            Ok(events.len)
        }
    }
    fn flags() -> EpollFlags {
        EpollFlags::EPOLLET | EpollFlags::EPOLLIN | EpollFlags::EPOLLOUT | EpollFlags::EPOLLRDHUP
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
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.list[..self.len].iter().map(|ev| ev.data() as usize)
        }
    }
}

/// Bindings to kqueue (macOS, iOS, FreeBSD, NetBSD, OpenBSD, DragonFly BSD).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
mod sys {
    use std::convert::TryInto;
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
        pub fn register(&self, fd: RawFd, key: usize) -> io::Result<()> {
            let flags = EventFlag::EV_CLEAR | EventFlag::EV_RECEIPT | EventFlag::EV_ADD;
            let udata = key as _;
            let changelist = [
                KEvent::new(fd as _, EventFilter::EVFILT_WRITE, flags, FFLAGS, 0, udata),
                KEvent::new(fd as _, EventFilter::EVFILT_READ, flags, FFLAGS, 0, udata),
            ];
            let mut eventlist = changelist.clone();
            kevent_ts(self.0, &changelist, &mut eventlist, None).map_err(io_err)?;
            for ev in &eventlist {
                // See https://github.com/tokio-rs/mio/issues/582
                let (flags, data) = (ev.flags(), ev.data());
                if flags.contains(EventFlag::EV_ERROR) && data != 0 && data != Errno::EPIPE as _ {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn reregister(&self, _fd: RawFd, _key: usize) -> io::Result<()> {
            Ok(())
        }
        pub fn deregister(&self, fd: RawFd) -> io::Result<()> {
            let flags = EventFlag::EV_RECEIPT | EventFlag::EV_DELETE;
            let changelist = [
                KEvent::new(fd as _, EventFilter::EVFILT_WRITE, flags, FFLAGS, 0, 0),
                KEvent::new(fd as _, EventFilter::EVFILT_READ, flags, FFLAGS, 0, 0),
            ];
            let mut eventlist = changelist.clone();
            kevent_ts(self.0, &changelist, &mut eventlist, None).map_err(io_err)?;
            for ev in &eventlist {
                let (flags, data) = (ev.flags(), ev.data());
                if flags.contains(EventFlag::EV_ERROR) && data != 0 {
                    return Err(io::Error::from_raw_os_error(data as _));
                }
            }
            Ok(())
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
            let timeout_ms: Option<usize> = timeout.and_then(|t| t.as_millis().try_into().ok());
            let timeout = timeout_ms.map(|ms| libc::timespec {
                tv_sec: (ms / 1000) as libc::time_t,
                tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
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
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.list[..self.len].iter().map(|ev| ev.udata() as usize)
        }
    }
}

/// Bindings to wepoll (Windows).
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
            self.0.register(&As(sock), flags(), key as u64)
        }
        pub fn reregister(&self, sock: RawSocket, key: usize) -> io::Result<()> {
            // Ignore errors because a concurrent poll can reregister the handle at any point.
            let _ = self.0.reregister(&As(sock), flags(), key as u64);
            Ok(())
        }
        pub fn deregister(&self, sock: RawSocket) -> io::Result<()> {
            // Ignore errors because an event can deregister the handle at any point.
            let _ = self.0.deregister(&As(sock));
            Ok(())
        }
        pub fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> io::Result<usize> {
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
    fn flags() -> EventFlag {
        EventFlag::ONESHOT | EventFlag::IN | EventFlag::OUT | EventFlag::RDHUP
    }

    pub struct Events(wepoll_binding::Events);
    impl Events {
        pub fn new() -> Events {
            Events(wepoll_binding::Events::with_capacity(1000))
        }
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.0.iter().map(|ev| ev.data() as usize)
        }
    }
}
