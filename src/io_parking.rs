use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

use crate::io_event::IoEvent;
use crate::reactor::{Reactor, ReactorLock};

pub(crate) struct IoParker {
    unparker: IoUnparker,
    _marker: PhantomData<*const ()>,
}

unsafe impl Send for IoParker {}

impl IoParker {
    pub fn new() -> IoParker {
        IoParker {
            unparker: IoUnparker {
                inner: Arc::new(Inner {
                    state: AtomicUsize::new(EMPTY),
                    lock: Mutex::new(()),
                    cvar: Condvar::new(),
                }),
            },
            _marker: PhantomData,
        }
    }

    pub fn park(&self) {
        self.unparker.inner.park(None);
    }

    pub fn park_timeout(&self, timeout: Duration) -> bool {
        self.unparker.inner.park(Some(timeout))
    }

    pub fn park_deadline(&self, deadline: Instant) -> bool {
        self.unparker
            .inner
            .park(Some(deadline.saturating_duration_since(Instant::now())))
    }

    pub fn unpark(&self) {
        self.unparker.unpark()
    }

    pub fn unparker(&self) -> IoUnparker {
        self.unparker.clone()
    }
}

impl Drop for IoParker {
    fn drop(&mut self) {
        // TODO: wake up another active IoParker
    }
}

impl fmt::Debug for IoParker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("IoParker { .. }")
    }
}

pub(crate) struct IoUnparker {
    inner: Arc<Inner>,
}

unsafe impl Send for IoUnparker {}
unsafe impl Sync for IoUnparker {}

impl IoUnparker {
    pub fn unpark(&self) {
        self.inner.unpark()
    }
}

impl fmt::Debug for IoUnparker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("IoUnparker { .. }")
    }
}

impl Clone for IoUnparker {
    fn clone(&self) -> IoUnparker {
        IoUnparker {
            inner: self.inner.clone(),
        }
    }
}

const EMPTY: usize = 0;
const PARKED: usize = 1;
const POLLING: usize = 2;
const NOTIFIED: usize = 3;

static EVENT: Lazy<IoEvent> = Lazy::new(|| IoEvent::new().unwrap());

struct Inner {
    state: AtomicUsize,
    lock: Mutex<()>,
    cvar: Condvar,
}

impl Inner {
    fn park(&self, timeout: Option<Duration>) -> bool {
        let mut reactor_lock = Reactor::get().try_lock();

        // If we were previously notified then we consume this notification and return quickly.
        if self
            .state
            .compare_exchange(NOTIFIED, EMPTY, SeqCst, SeqCst)
            .is_ok()
        {
            // Process available I/O events.
            if let Some(mut reactor_lock) = Reactor::get().try_lock() {
                reactor_lock.poll().expect("failure while polling I/O");
            }
            return true;
        }

        // If the timeout is zero, then there is no need to actually block.
        if let Some(dur) = timeout {
            if dur == Duration::from_millis(0) {
                // Process available I/O events.
                if let Some(mut reactor_lock) = Reactor::get().try_lock() {
                    reactor_lock.poll().expect("failure while polling I/O");
                }
                return false;
            }
        }

        // Otherwise we need to coordinate going to sleep.
        let mut m = self.lock.lock().unwrap();

        let state = match reactor_lock {
            None => PARKED,
            Some(_) => POLLING,
        };

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
                            if EVENT.clear() {
                                reactor_lock.poll().expect("TODO");
                            } else {
                                reactor_lock.wait().expect("TODO");
                            }
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
                let _m = match &mut reactor_lock {
                    None => self.cvar.wait_timeout(m, timeout).unwrap().0,
                    Some(reactor_lock) => {
                        drop(m);
                        if EVENT.clear() {
                            reactor_lock.poll().expect("TODO");
                        } else {
                            reactor_lock.wait().expect("TODO"); // TODO: use actual timeout
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
            EVENT.notify();
        }
    }
}
