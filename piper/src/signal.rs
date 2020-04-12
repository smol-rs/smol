//! A synchronization primitive for notifying async tasks and threads.
//!
//! This is a variant of a conditional variable that is heavily inspired by eventcounts invented
//! by Dmitry Vyukov: http://www.1024cores.net/home/lock-free-algorithms/eventcounts

use std::cell::Cell;
use std::future::Future;
use std::mem::{self, ManuallyDrop};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{self, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::thread::{self, Thread};

/// A bit set inside `Signal` when there is at least one listener that has already been notified.
const NOTIFIED: usize = 1 << 0;

/// A bit set inside `Signal` when there is at least one notifiable listener.
const NOTIFIABLE: usize = 1 << 1;

/// Inner state of `Signal`.
struct Inner {
    /// Holds bits `NOTIFIED` and `NOTIFIABLE`.
    flags: AtomicUsize,

    /// A linked list holding registered listeners.
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

/// A synchronization primitive for notifying async tasks and threads.
///
/// Listeners can be registered using `listen()`. There are two ways of notifying listeners:
///
/// 1. `notify_one()` notifies one listener.
/// 2. `notify_all()` notifies all listeners.
///
/// If there are no active listeners at the time a notification is sent, it simply gets lost.
///
/// Note that `notify_one()` does not notify one *additional* listener - it only makes sure
/// *at least* one listener among the active ones is notified.
///
/// There are two ways for a listener to wait for a notification:
///
/// 1. In an asynchronous manner using `.await`.
/// 2. In a blocking manner by calling `wait()` on it.
///
/// If a notified listener is dropped without ever waiting for a notification, dropping will notify
/// another another active listener.
///
/// Listeners are registered and notified in the first-in first-out fashion, ensuring fairness.
pub struct Signal {
    /// A pointer to heap-allocated inner state.
    ///
    /// This pointer is initially null and gets lazily initialized on first use. Semantically, it
    /// is an `Arc<Inner>` so it's important to keep in mind that it contributes to the `Arc`s
    /// reference count.
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

    /// Returns a guard listening for a notification.
    #[cold]
    pub fn listen(&self) -> SignalListener {
        let inner = self.inner();
        let listener = SignalListener {
            inner: unsafe { Arc::clone(&ManuallyDrop::new(Arc::from_raw(inner))) },
            entry: Some(inner.lock().insert()),
        };

        // Make sure the listener is registered before whatever happens next.
        full_fence();
        listener
    }

    /// Notifies a single active listener.
    ///
    /// Note that this does not notify one *additional* listener - it only makes sure *at least*
    /// one listener among the active ones is notified.
    #[inline]
    pub fn notify_one(&self) {
        let inner = self.inner();

        // Make sure the notification comes after whatever triggered it.
        full_fence();

        // Notify if no active listeners have been notified and there is at least one listener.
        let flags = inner.flags.load(Ordering::Relaxed);
        if flags & NOTIFIED == 0 && flags & NOTIFIABLE != 0 {
            inner.lock().notify(false);
        }
    }

    /// Notifies all active listeners.
    #[inline]
    pub fn notify_all(&self) {
        let inner = self.inner();

        // Make sure the notification comes after whatever triggered it.
        full_fence();

        // Notify if there is at least one listener.
        if inner.flags.load(Ordering::Relaxed) & NOTIFIABLE != 0 {
            inner.lock().notify(true);
        }
    }

    /// Returns a reference to the inner state.
    fn inner(&self) -> &Inner {
        let mut inner = self.inner.load(Ordering::Acquire);

        // Initialize the state if this is its first use.
        if inner.is_null() {
            // Allocate on the heap.
            let new = Arc::new(Inner {
                flags: AtomicUsize::new(0),
                list: Mutex::new(List {
                    head: None,
                    tail: None,
                    len: 0,
                    notifiable: 0,
                }),
            });
            // Convert the heap-allocated state into a raw pointer.
            let new = Arc::into_raw(new) as *mut Inner;

            // Attempt to replace the null-pointer with the new state pointer.
            inner = self.inner.compare_and_swap(inner, new, Ordering::AcqRel);

            // Check if the old pointer value was indeed null.
            if inner.is_null() {
                // If yes, then use the new state pointer.
                inner = new;
            } else {
                // If not, that means a concurrent operation has initialized the state.
                // In that case, use the old pointer and deallocate the new one.
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

        // If the state pointer has been initialized, deallocate it.
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

/// A guard waiting for a notification from a `Signal`.
///
/// There are two ways for a listener to wait for a notification:
///
/// 1. In an asynchronous manner using `.await`.
/// 2. In a blocking manner by calling `wait()` on it.
///
/// If a notified listener is dropped without ever waiting for a notification, dropping will notify
/// another another active listener.
pub struct SignalListener {
    /// A reference to `Signal`s inner state.
    inner: Arc<Inner>,

    /// A pointer to this listener's entry in the linked list.
    entry: Option<NonNull<Entry>>,
}

unsafe impl Send for SignalListener {}
unsafe impl Sync for SignalListener {}

impl SignalListener {
    /// Blocks until a notification is received.
    pub fn wait(mut self) {
        // Take out the entry pointer and set it to `None`.
        let entry = match self.entry.take() {
            None => unreachable!("cannot wait twice on a `SignalListener`"),
            Some(entry) => entry,
        };

        // Set this listener's state to `Waiting`.
        {
            let mut list = self.inner.lock();
            let e = unsafe { entry.as_ref() };

            // Do a dummy replace operation in order to take out the state.
            match e.state.replace(State::Notified) {
                State::Notified => {
                    // If this listener has been notified, remove it from the list and return.
                    list.remove(entry);
                    return;
                }
                // Otherwise, set the state to `Waiting`.
                _ => e.state.set(State::Waiting(thread::current())),
            }
        }

        // Wait until a notification is received.
        loop {
            thread::park();

            let mut list = self.inner.lock();
            let e = unsafe { entry.as_ref() };

            // Do a dummy replace operation in order to take out the state.
            match e.state.replace(State::Notified) {
                State::Notified => {
                    // If this listener has been notified, remove it from the list and return.
                    list.remove(entry);
                    return;
                }
                // Otherwise, set the state back to `Waiting`.
                state => e.state.set(state),
            }
        }
    }
}

impl Future for SignalListener {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut list = self.inner.lock();

        let entry = match self.entry {
            None => unreachable!("cannot poll a completed `SignalListener` future"),
            Some(entry) => entry,
        };
        let state = unsafe { &entry.as_ref().state };

        // Do a dummy replace operation in order to take out the state.
        match state.replace(State::Notified) {
            State::Notified => {
                // If this listener has been notified, remove it from the list and return.
                list.remove(entry);
                drop(list);
                self.entry = None;
                return Poll::Ready(());
            }
            State::Created => {
                // If the listener was just created, put it in the `Polling` state.
                state.set(State::Polling(cx.waker().clone()));
            }
            State::Polling(w) => {
                // If the listener was in the `Pooling` state, keep it.
                state.set(State::Polling(w));
            }
            State::Waiting(_) => {
                unreachable!("cannot poll and wait on `SignalListener` at the same time")
            }
        }

        Poll::Pending
    }
}

impl Drop for SignalListener {
    fn drop(&mut self) {
        // If this listener has never picked up a notification...
        if let Some(entry) = self.entry.take() {
            let mut list = self.inner.lock();

            // But if a notification was delivered to it...
            if list.remove(entry).is_notified() {
                // Then pass it on to another active listener.
                list.notify(false);
            }
        }
    }
}

/// A guard holding the linked list locked.
struct ListGuard<'a> {
    /// A reference to `Signal`s inner state.
    inner: &'a Inner,

    /// The actual guard that acquired the linked list.
    guard: MutexGuard<'a, List>,
}

impl Drop for ListGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        let list = &mut **self;
        let mut flags = 0;

        // Set the `NOTIFIED` flag if there is at least one notified listener.
        if list.len - list.notifiable > 0 {
            flags |= NOTIFIED;
        }

        // Set the `NOTIFIABLE` flag if there is at least one notifiable listener.
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

/// The state of a listener.
enum State {
    /// It has just been created.
    Created,

    /// It has received a notification.
    Notified,

    /// An async task is polling it.
    Polling(Waker),

    /// A thread is blocked on it.
    Waiting(Thread),
}

impl State {
    /// Returns `true` if this is the `Notified` state.
    #[inline]
    fn is_notified(&self) -> bool {
        match self {
            State::Notified => true,
            State::Created | State::Polling(_) | State::Waiting(_) => false,
        }
    }
}

/// An entry representing a registered listener.
struct Entry {
    /// THe state of this listener.
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

    /// Total number of entries in the list.
    len: usize,

    /// Number of notifiable entries in the list.
    ///
    /// Notifiable entries are those that haven't been notified yet.
    notifiable: usize,
}

impl List {
    /// Inserts a new entry into the list.
    fn insert(&mut self) -> NonNull<Entry> {
        unsafe {
            // Allocate an entry that is going to become the new tail.
            let entry = NonNull::new_unchecked(Box::into_raw(Box::new(Entry {
                state: Cell::new(State::Created),
                prev: Cell::new(self.tail),
                next: Cell::new(None),
            })));

            // Replace the tail with the new entry.
            match mem::replace(&mut self.tail, Some(entry)) {
                None => self.head = Some(entry),
                Some(t) => t.as_ref().next.set(Some(entry)),
            }

            // Bump the total count and the count of notifiable entries.
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

            // Unlink from the previous entry.
            match prev {
                None => self.head = next,
                Some(p) => p.as_ref().next.set(next),
            }

            // Unlink from the next entry.
            match next {
                None => self.tail = prev,
                Some(n) => n.as_ref().prev.set(prev),
            }

            // Deallocate and extract the state.
            let entry = Box::from_raw(entry.as_ptr());
            let state = entry.state.into_inner();

            // Update the counters.
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
        let mut entry = self.tail;

        // Iterate over the entries in the list.
        while let Some(e) = entry {
            let e = unsafe { e.as_ref() };

            // Set the state of this entry to `Notified`.
            let state = e.state.replace(State::Notified);
            let is_notified = state.is_notified();

            // Wake the task or unpark the thread.
            match state {
                State::Notified => {}
                State::Created => {}
                State::Polling(w) => w.wake(),
                State::Waiting(t) => t.unpark(),
            }

            // Update the count of notifiable entries.
            if !is_notified {
                self.notifiable -= 1;
            }

            // If all entries need to be notified, go to the next one.
            if notify_all {
                entry = e.prev.get();
            } else {
                break;
            }
        }
    }
}

/// Equivalent to `atomic::fence(Ordering::SeqCst)`, but in some cases faster.
#[inline]
fn full_fence() {
    if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
        // HACK(stjepang): On x86 architectures there are two different ways of executing
        // a `SeqCst` fence.
        //
        // 1. `atomic::fence(SeqCst)`, which compiles into a `mfence` instruction.
        // 2. `_.compare_and_swap(_, _, SeqCst)`, which compiles into a `lock cmpxchg` instruction.
        //
        // Both instructions have the effect of a full barrier, but empirical benchmarks have shown
        // that the second one makes notifiying listeners a bit faster.
        //
        // The ideal solution here would be to use inline assembly, but we're instead creating a
        // temporary atomic variable and compare-and-exchanging its value. No sane compiler to
        // x86 platforms is going to optimize this away.
        let a = AtomicUsize::new(0);
        a.compare_and_swap(0, 1, Ordering::SeqCst);
    } else {
        atomic::fence(Ordering::SeqCst);
    }
}
