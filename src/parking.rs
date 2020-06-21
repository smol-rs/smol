use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use slab::Slab;

use crate::reactor::Reactor;

static REGISTRY: Lazy<Mutex<Slab<Unparker>>> = Lazy::new(|| Mutex::new(Slab::new()));

/// Parks a thread.
pub(crate) struct Parker {
    key: Cell<Option<usize>>,
    unparker: Unparker,
}

unsafe impl Send for Parker {}

impl Parker {
    /// Creates a new [`Parker`].
    pub fn new() -> Parker {
        Parker {
            key: Cell::new(None),
            unparker: Unparker {
                inner: Arc::new(Inner {
                    state: AtomicUsize::new(EMPTY),
                    lock: Mutex::new(()),
                    cvar: Condvar::new(),
                }),
            },
        }
    }

    /// Blocks the current thread until the token is made available.
    pub fn park(&self) {
        self.register();
        self.unparker.inner.park(None);
    }

    /// Blocks the current thread until the token is made available or the timeout is reached.
    pub fn park_timeout(&self, timeout: Duration) -> bool {
        self.register();
        self.unparker.inner.park(Some(timeout))
    }

    // /// Blocks the current thread until the token is made available or the deadline is reached.
    // pub fn park_deadline(&self, deadline: Instant) -> bool {
    //     self.register();
    //     self.unparker
    //         .inner
    //         .park(Some(deadline.saturating_duration_since(Instant::now())))
    // }
    //
    // /// Atomically makes the token available if it is not already.
    // pub fn unpark(&self) {
    //     self.unparker.unpark()
    // }

    /// Returns a handle for unparking.
    pub fn unparker(&self) -> Unparker {
        self.unparker.clone()
    }

    fn register(&self) {
        if self.key.get().is_none() {
            let mut reg = REGISTRY.lock().unwrap();
            let key = reg.insert(self.unparker.clone());
            self.key.set(Some(key));
        }
    }

    fn unregister(&self) {
        if let Some(key) = self.key.take() {
            let mut reg = REGISTRY.lock().unwrap();
            reg.remove(key);

            // Notify another parker to make sure the reactor keeps getting polled.
            if let Some((_, u)) = reg.iter().next() {
                u.unpark();
            }
        }
    }
}

impl Drop for Parker {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl fmt::Debug for Parker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Parker { .. }")
    }
}

/// Unparks a thread.
pub(crate) struct Unparker {
    inner: Arc<Inner>,
}

unsafe impl Send for Unparker {}
unsafe impl Sync for Unparker {}

impl Unparker {
    /// Atomically makes the token available if it is not already.
    pub fn unpark(&self) {
        self.inner.unpark()
    }
}

impl fmt::Debug for Unparker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Unparker { .. }")
    }
}

impl Clone for Unparker {
    fn clone(&self) -> Unparker {
        Unparker {
            inner: self.inner.clone(),
        }
    }
}

const EMPTY: usize = 0;
const PARKED: usize = 1;
const POLLING: usize = 2;
const NOTIFIED: usize = 3;

struct Inner {
    state: AtomicUsize,
    lock: Mutex<()>,
    cvar: Condvar,
}

impl Inner {
    fn park(&self, timeout: Option<Duration>) -> bool {
        // If we were previously notified then we consume this notification and return quickly.
        if self
            .state
            .compare_exchange(NOTIFIED, EMPTY, SeqCst, SeqCst)
            .is_ok()
        {
            // Process available I/O events.
            if let Some(mut reactor_lock) = Reactor::get().try_lock() {
                reactor_lock
                    .react(Some(Duration::from_secs(0)))
                    .expect("failure while polling I/O");
            }
            return true;
        }

        // If the timeout is zero, then there is no need to actually block.
        if let Some(dur) = timeout {
            if dur == Duration::from_millis(0) {
                // Process available I/O events.
                if let Some(mut reactor_lock) = Reactor::get().try_lock() {
                    reactor_lock
                        .react(Some(Duration::from_secs(0)))
                        .expect("failure while polling I/O");
                }
                return false;
            }
        }

        // Otherwise we need to coordinate going to sleep.
        let mut reactor_lock = Reactor::get().try_lock();
        let state = match reactor_lock {
            None => PARKED,
            Some(_) => POLLING,
        };
        let mut m = self.lock.lock().unwrap();

        match self.state.compare_exchange(EMPTY, state, SeqCst, SeqCst) {
            Ok(_) => {}
            // Consume this notification to avoid spurious wakeups in the next park.
            Err(NOTIFIED) => {
                // We must read `state` here, even though we know it will be `NOTIFIED`. This is
                // because `unpark` may have been called again since we read `NOTIFIED` in the
                // `compare_exchange` above. We must perform an acquire operation that synchronizes
                // with that `unpark` to observe any writes it made before the call to `unpark`. To
                // do that we must read from the write it made to `state`.
                let old = self.state.swap(EMPTY, SeqCst);
                assert_eq!(old, NOTIFIED, "park state changed unexpectedly");
                return true;
            }
            Err(n) => panic!("inconsistent park_timeout state: {}", n),
        }

        match timeout {
            None => {
                loop {
                    // Block the current thread on the conditional variable.
                    match &mut reactor_lock {
                        None => m = self.cvar.wait(m).unwrap(),
                        Some(reactor_lock) => {
                            drop(m);

                            reactor_lock.react(None).expect("failure while polling I/O");

                            m = self.lock.lock().unwrap();
                        }
                    }

                    match self.state.compare_exchange(NOTIFIED, EMPTY, SeqCst, SeqCst) {
                        Ok(_) => return true, // got a notification
                        Err(_) => {}          // spurious wakeup, go back to sleep
                    }
                }
            }
            Some(timeout) => {
                // Wait with a timeout, and if we spuriously wake up or otherwise wake up from a
                // notification we just want to unconditionally set `state` back to `EMPTY`, either
                // consuming a notification or un-flagging ourselves as parked.
                let _m = match reactor_lock.as_mut() {
                    None => self.cvar.wait_timeout(m, timeout).unwrap().0,
                    Some(reactor_lock) => {
                        drop(m);
                        let deadline = Instant::now() + timeout;
                        loop {
                            reactor_lock
                                .react(Some(deadline.saturating_duration_since(Instant::now())))
                                .expect("failure while polling I/O");

                            if Instant::now() >= deadline {
                                break;
                            }
                        }
                        self.lock.lock().unwrap()
                    }
                };

                match self.state.swap(EMPTY, SeqCst) {
                    NOTIFIED => true,          // got a notification
                    PARKED | POLLING => false, // no notification
                    n => panic!("inconsistent park_timeout state: {}", n),
                }
            }
        }
    }

    pub fn unpark(&self) {
        // To ensure the unparked thread will observe any writes we made before this call, we must
        // perform a release operation that `park` can synchronize with. To do that we must write
        // `NOTIFIED` even if `state` is already `NOTIFIED`. That is why this must be a swap rather
        // than a compare-and-swap that returns if it reads `NOTIFIED` on failure.
        let state = match self.state.swap(NOTIFIED, SeqCst) {
            EMPTY => return,    // no one was waiting
            NOTIFIED => return, // already unparked
            state => state,     // gotta go wake someone up
        };

        // There is a period between when the parked thread sets `state` to `PARKED` (or last
        // checked `state` in the case of a spurious wakeup) and when it actually waits on `cvar`.
        // If we were to notify during this period it would be ignored and then when the parked
        // thread went to sleep it would never wake up. Fortunately, it has `lock` locked at this
        // stage so we can acquire `lock` to wait until it is ready to receive the notification.
        //
        // Releasing `lock` before the call to `notify_one` means that when the parked thread wakes
        // it doesn't get woken only to have to wait for us to release `lock`.
        drop(self.lock.lock().unwrap());

        if state == PARKED {
            self.cvar.notify_one();
        } else {
            Reactor::get().notify();
        }
    }
}
