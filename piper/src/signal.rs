//! A primitive for asynchronous or blocking synchronization.

use std::cell::Cell;
use std::future::Future;
use std::mem::{self, ManuallyDrop};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{self, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// Set when there is at least one watcher that has already been notified.
const NOTIFIED: usize = 1 << 0;

/// Set when there is at least one notifiable watcher.
const NOTIFIABLE: usize = 1 << 1;

struct Inner {
    /// Holds three bits: `LOCKED`, `NOTIFIED`, and `NOTIFIABLE`.
    flags: AtomicUsize,

    /// A linked list holding blocked tasks.
    list: Mutex<List>,
}

impl Inner {
    /// Locks the list.
    fn lock(&self) -> ListGuard<'_> {
        ListGuard {
            inner: self,
            guard: self.list.lock().unwrap(),
        }
    }
}

/// A linked list holding entries.
pub struct Signal {
    inner: AtomicPtr<Inner>,
}

unsafe impl Send for Signal {}
unsafe impl Sync for Signal {}

impl Signal {
    /// Creates a new `Signal`.
    #[inline]
    pub fn new() -> Signal {
        Signal {
            inner: AtomicPtr::default(),
        }
    }

    /// Returns a guard watching new Signal.
    #[cold]
    pub fn listen(&self) -> SignalListener {
        let inner = self.inner();
        let entry = inner.lock().insert();
        full_fence();

        SignalListener {
            inner: unsafe { Arc::clone(&ManuallyDrop::new(Arc::from_raw(inner))) },
            entry: Some(entry),
        }
    }

    /// Notifies a single active watcher.
    ///
    /// If there is an active [`SignalListener`] that has already been notified, another one will
    /// **not** be notified.
    ///
    /// [`SignalListener`] struct.SignalListener.html
    #[inline]
    pub fn notify_one(&self) {
        let inner = self.inner();
        full_fence();

        let flags = inner.flags.load(Ordering::Relaxed);
        if flags & NOTIFIED == 0 && flags & NOTIFIABLE != 0 {
            inner.lock().notify(false);
        }
    }

    /// Notifies all active watchers.
    #[inline]
    pub fn notify_all(&self) {
        let inner = self.inner();
        full_fence();

        if inner.flags.load(Ordering::Relaxed) & NOTIFIABLE != 0 {
            inner.lock().notify(true);
        }
    }

    /// Returns a reference to the inner list of entries.
    fn inner(&self) -> &Inner {
        let mut inner = self.inner.load(Ordering::Acquire);
        if inner.is_null() {
            let new = Arc::new(Inner {
                flags: AtomicUsize::new(0),
                list: Mutex::new(List {
                    head: None,
                    tail: None,
                    len: 0,
                    notifiable: 0,
                }),
            });
            let new = Arc::into_raw(new) as *mut Inner;

            inner = self.inner.compare_and_swap(inner, new, Ordering::AcqRel);

            if inner.is_null() {
                inner = new;
            } else {
                unsafe {
                    drop(Arc::from_raw(new));
                }
            }
        }

        unsafe { &*inner }
    }
}

impl Drop for Signal {
    #[inline]
    fn drop(&mut self) {
        let inner: *mut Inner = *self.inner.get_mut();

        if !inner.is_null() {
            unsafe {
                drop(Arc::from_raw(inner));
            }
        }
    }
}

impl Default for Signal {
    fn default() -> Signal {
        Signal::new()
    }
}

/// A guard watching an `Signal`.
pub struct SignalListener {
    inner: Arc<Inner>,
    entry: Option<NonNull<Entry>>,
}

unsafe impl Send for SignalListener {}
unsafe impl Sync for SignalListener {}

impl Future for SignalListener {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut list = self.inner.lock();

        let entry = match self.entry {
            None => unreachable!("cannot poll a completed `SignalListener` future"),
            Some(entry) => entry,
        };
        let state = unsafe { &entry.as_ref().state };

        match state.replace(State::Notified) {
            State::Notified => {
                list.remove(entry);
                drop(list);

                self.entry = None;
                return Poll::Ready(());
            }
            State::Created => state.set(State::Polling(cx.waker().clone())),
            State::Polling(w) => state.set(State::Polling(w)),
        }

        Poll::Pending
    }
}

impl Drop for SignalListener {
    fn drop(&mut self) {
        if let Some(entry) = self.entry.take() {
            let mut list = self.inner.lock();

            if list.remove(entry).is_notified() {
                list.notify(false);
            }
        }
    }
}

/// A guard holding the list locked.
struct ListGuard<'a> {
    inner: &'a Inner,
    guard: MutexGuard<'a, List>,
}

impl Drop for ListGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        let list = &mut **self;
        let mut flags = 0;

        // Set the `NOTIFIED` flag if there is at least one notified watcher.
        if list.len - list.notifiable > 0 {
            flags |= NOTIFIED;
        }

        // Set the `NOTIFIABLE` flag if there is at least one notifiable watcher.
        if list.notifiable > 0 {
            flags |= NOTIFIABLE;
        }

        self.inner.flags.store(flags, Ordering::Release);
    }
}

impl Deref for ListGuard<'_> {
    type Target = List;

    #[inline]
    fn deref(&self) -> &List {
        &*self.guard
    }
}

impl DerefMut for ListGuard<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut List {
        &mut *self.guard
    }
}

/// The state of a watcher.
enum State {
    /// It has just been created.
    Created,

    /// It has received a notification.
    Notified,

    /// A task is polling it.
    Polling(Waker),
}

impl State {
    fn is_notified(&self) -> bool {
        match self {
            State::Notified => true,
            State::Created | State::Polling(_) => false,
        }
    }
}

/// An entry representing the state of a watcher.
struct Entry {
    /// State of this watcher.
    state: Cell<State>,

    /// Previous entry in the linked list.
    prev: Cell<Option<NonNull<Entry>>>,

    /// Next entry in the linked list.
    next: Cell<Option<NonNull<Entry>>>,
}

/// A linked list of entries.
struct List {
    /// First entry in the list.
    head: Option<NonNull<Entry>>,

    /// Last entry in the list.
    tail: Option<NonNull<Entry>>,

    /// Number of entries in the list.
    len: usize,

    /// Number of notifiable entries in the list.
    notifiable: usize,
}

impl List {
    /// Inserts a new entry into the list.
    fn insert(&mut self) -> NonNull<Entry> {
        unsafe {
            let entry = NonNull::new_unchecked(Box::into_raw(Box::new(Entry {
                state: Cell::new(State::Created),
                prev: Cell::new(self.tail),
                next: Cell::new(None),
            })));

            match mem::replace(&mut self.tail, Some(entry)) {
                None => self.head = Some(entry),
                Some(t) => t.as_ref().next.set(Some(entry)),
            }

            self.len += 1;
            self.notifiable += 1;

            entry
        }
    }

    /// Removes an entry from the list and returns its state.
    fn remove(&mut self, entry: NonNull<Entry>) -> State {
        unsafe {
            let prev = entry.as_ref().prev.get();
            let next = entry.as_ref().next.get();

            match prev {
                None => self.head = next,
                Some(p) => p.as_ref().next.set(next),
            }

            match next {
                None => self.tail = prev,
                Some(n) => n.as_ref().prev.set(prev),
            }

            let entry = Box::from_raw(entry.as_ptr());
            let state = entry.state.into_inner();

            if !state.is_notified() {
                self.notifiable -= 1;
            }
            self.len -= 1;

            state
        }
    }

    /// Notifies an entry.
    #[cold]
    fn notify(&mut self, notify_all: bool) {
        let mut head = self.head;

        while let Some(h) = head {
            let h = unsafe { h.as_ref() };

            let state = h.state.replace(State::Notified);
            let is_notified = state.is_notified();

            match state {
                State::Notified => {}
                State::Created => {}
                State::Polling(w) => w.wake(),
            }

            if !is_notified {
                self.notifiable -= 1;
            }

            if !notify_all {
                break;
            }

            head = h.next.get();
        }
    }
}

/// Equivalent to `atomic::fence(Ordering::SeqCst)`, but sometimes faster.
#[inline]
fn full_fence() {
    if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
        // HACK(stjepang): On x86 architectures there are two different ways of executing
        // a `SeqCst` fence.
        //
        // 1. `atomic::fence(SeqCst)`, which compiles into a `mfence` instruction.
        // 2. `_.compare_and_swap(_, _, SeqCst)`, which compiles into a `lock cmpxchg`
        //    instruction.
        //
        // Both instructions have the effect of a full barrier, but benchmarks have shown
        // that the second one makes pinning faster in this particular case.  It is not
        // clear that this is permitted by the C++ memory model (SC fences work very
        // differently from SC accesses), but experimental evidence suggests that this
        // works fine.  Using inline assembly would be a viable (and correct) alternative,
        // but alas, that is not possible on stable Rust.
        let a = AtomicUsize::new(0);
        a.compare_and_swap(0, 0, Ordering::SeqCst);
    } else {
        atomic::fence(Ordering::SeqCst);
    }
}
