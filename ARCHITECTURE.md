# smol-rs Architecture

The architecture of [`smol-rs`].

This document describes the architecture of [`smol-rs`] and its crates on a high
level. It is intended for new contributors who want to quickly familiarize
themselves with [`smol`]'s composition before contributing. However it may also
be useful for evaluating [`smol`] in comparison with other runtimes.

## Thousand-Mile View

[`smol`] is a small, safe and fast concurrent runtime built in pure Rust. Its
primary goal is to enable M:N concurrency in Rust programs; multiple coroutines
can be multiplexed onto a single thread, allowing a server to handle hundreds
of thousands (if not millions) of clients at a time. However, [`smol`] can just
as easily multiplex tasks onto multiple threads to enable blazingy fast
concurrency. [`smol`] is intended to work on any scale; [`smol`] should work for
programs with two coroutines as well as two million.

On an architectural level, [`smol`] prioritizes maintainable code and clarity.
[`smol`] aims to provide the performance of a modern `async` runtime while still
remaining hackable. This philosophy informs much of the decisions in [`smol`]'s
codebase and differentiate it from other contemporary runtimes.

On a technical level, [`smol`] is a [work-stealing] executor built around a
[one-shot] asynchronous event loop. It also contains a thread pool for
filesystem operations and a reactor for waiting for child processes to finish.
[`smol`] itself is a meta-crate that combines the features of numerous subcrates
into a single `async` runtime-based package.

smol-rs consists of the following crates:

- [`async-io`] provides a one-shot reactor for polling asynchronous I/O. It is
  used for registering sockets into `epoll` or another system, then polling them
  simultaneously.
- [`blocking`] provides a managed thread-pool for polling blocking operations as
  asynchronous tasks. It is used by many parts of [`smol`] for turning
  operations that would normally be non-concurrent into concurrent operations,
  and vice versa.
- [`async-executor`] provides a work-stealing executor that is used as the
  scheduler for an `async` program. While the executor isn't as optimized as
  other contemporary executors, it provides a performant executor implemented in
  mostly safe code in under 1.5 KLOC.
- [`futures-lite`] provides a handful of low-level primitives for combining and
  dealing with `async` coroutines.
- [`async-channel`] and [`async-lock`] provide synchronization primitives that
  work to connect asynchronous tasks.
- [`async-net`] provides a set of higher-level APIs over networking primitives.
  It combines [`async-io`] and [`blocking`] to create a full-featured, fully
  asynchronous networking API.
- [`async-fs`] provides an asynchronous API for manipulating the filesystem. The
  API itself is built on top of [`blocking`].

These subcrates in and of themselves depend on subcrates for further
functionality. These are explained in a more bottom-up fashion below.

## Lower Level Crates

These crates provide safer, lower-level functionality used in the higher-level
crates. These could be used in higher-level crates but are intended primarily
for use in [`smol`]'s underlying plumbing.

### [`parking`]

[`parking`] is used to block threads until an arbitrary signal is received, or
"parks" them. The [`std::thread::park`] API suffers from involving global state;
any arbitrary user code can unpark the thread and wake up the parker. The goal
of this crate is to provide an API that can be used to block the current thread
and only wake up once a signal is delivered.

[`parking`] is implemented relatively simply. It uses a combination of a
[`Mutex`] and a [`Condvar`] to store whether the thread is parked or not, then
wait until the thread has received a wakeup signal.

### [`waker-fn`]

[`waker-fn`] is provided to easily create [`Waker`]s for use in `async`
computation. The [`Waker`] is similar to a callback in the `async` models of
other languages; it is called when the coroutine has stopped waiting and is
ready to be polled again. [`waker-fn`] makes this comparison more literal by
literally allowing a [`Waker`] to be constructed from a callback.

[`waker-fn`] is used to construct higher-level asynchronous runtimes. It is
implemented simply by creating an object that implements the [`Wake`] trait and
using that as a [`Waker`].

### [`atomic-waker`]

In order to construct runtimes, you need to be able to store [`Waker`]s in a
concurrent way. That way, different parties can simultaneously store [`Waker`]s
to show interest in an event, while activators of the event can take that
[`Waker`] can wake it. [`atomic-waker`] provides [`AtomicWaker`], which is a
low-level solution to this problem.

[`AtomicWaker`] uses an interiorly mutable slot protected by an atomic variable
to synchronize the [`Waker`]. Storing the [`Waker`] acquires an atomic lock,
writes to the slot and then releases that atomic lock. Reading the [`Waker`] out
requires acquiring that lock and moving the [`Waker`] out.

[`AtomicWaker`] aims to be lock-free while also avoiding spinloops. It uses a
novel strategy to accomplish this task. Simultaneous attempts to store
[`Waker`]s in the slot will choose one [`Waker`] while waking up the other
[`Waker`], making the task inserting that [`Waker`] try again. Simultaneous
attempts to move out the [`Waker`] will have one of the operations return the
[`Waker`] and the others return `None`. Simultaneous attempts to write to and
read from the slot will mark the slot as "needs to wake", making the writer wake
up the [`Waker`] immediately once it has acquired the lock.

The main weakness of [`AtomicWaker`] is the fact that it can only store one
[`Waker`], meaning only one task can wait using this primitive at once. This
limitation is addressed in other code in [`smol-rs`].

### [`fastrand`]

Higher-level operations require a fast random number generator in order to
provide fairness. [`fastrand`] is a dead-simple random number generator that
aims to provide psuedorandomness.

The underlying RNG algorithm for [`fastrand`] is [wyrand], which provides
decently distributed but fast random numbers. Most of the crate is built on a
function that generates 64-bit random numbers. The remaining API transforms this
function into more useful results.

Global RNG is provided by a thread-local slot that is queried every time global
randomness is needed. All generated RNG instances derive their seed from the
thread-local RNG. The seed for the global RNG is derived from the hash of the
thread ID and local time or, on Web targets, the browser RNG.

The API of [`fastrand`] is deliberately kept small and constrained. Higher-level
functionality is moved to [`fastrand-contrib`].

### [`concurrent-queue`]

[`concurrent-queue`] is a fork of the [`crossbeam-queue`] crate. It provides a
lock-free queue containing arbitrary items. There are three types of queues:
optimized single-item queues, queues with a bounded capacity, and infinite
queues with unbounded capacity.

Each queue works in the following way:

- The single-capacity queue is essentially a spinlock around an interiorly
  mutable slot. Reads to or writes from this slot lock the spinlock.
- The bounded queue contains several slots that each track their state, as well
  as pointers to the current head and tail of the list. Pushing to the queue
  moves the tail forward and writes the data to the slot that was just moved
  past. Popping the queue pushes the head forward and moves out the slot that
  was previously pushed into. Two "laps" of the queue are used in order to
  create a "ring buffer" that can be continuously pushed into or popped from.
- The unbounded queue works like a lot of bounded queues linked together. It
  starts with a bounded queue. When the bounded queue runs out of space, it
  creates another queue, links it to the previous queue and pushes items to
  there. All of the created bounded queues are linked together to form a
  seamless unbounded queue.

### [`piper`]

TODO: Explain this!

### [`polling`]

[`polling`] is a portable interface to the underlying operating system API for
asynchronous I/O. It aims to allow for efficiently polling hundreds of thousands
of sockets at once using underlying OS primitives.

[`polling`] is a relatively simple wrapper around [`epoll`] on Linux and
[`kqueue`] on BSD/macOS. Most of the code in [`polling`] is dedicated to
providing wrappers around [IOCP] on Windows and [`poll`] on Unixes without any
better option.

[IOCP] is built around waiting for specific operations to complete, rather than
creating an "event loop". However, Windows exposes a subsystem called [`AFD`]
that can be used to register a wait for polling a set of sockets. Once we've
used internal Windows APIs to access [`AFD`], we group sockets into "poll
groups" and then set up an outgoing "poll" operation for each poll group. From
here we can collect these "poll" events from [IOCP] in order to simulate a
classic Unix event loop.

For the [`poll`] system call, the usual "add/modify/remove" system exposed by
other event loop systems doesn't work, as [`poll`] only takes a flat list of
file descriptors. Therefore we set up our own hash map of file descriptors,
which is kept in sync with a list of `pollfd`'s that are then passed to
[`poll`]. This list can be modified on the fly by waking up the [`poll`] syscall
using a designated "wakeup" pipe, modifying the list, then resuming the [`poll`]
operation.

[`polling`] does not consider I/O safety and therefore its API is unsafe. It is
the burder of higher-level APIs to implement safer APIs on top of [`polling`].

## Medium-Level Crates

These crates provide a high-level, sometimes `async` API intended for use in
production programs. These are featureful, safe and can even be used on their
own. However, there are higher-level APIs that may be of interest to more
casual users.

### [`async-task`]

In order to build an executor, there are essentially two parts to be
implemented:

- A task system that allocates the coroutine on the heap, attaches some
  concurrent state to it, then provides handles to that task.
- A task queue that takes these tasks and decides how they are scheduled.

[`async-task`] implements the former system in a way that it can be generically
applied to different styles of executors. Essentially, [`async-task`] takes care
of the boring, soundness-prone part of the executor so that higher-level crates
can focus on optimizing their scheduling strategies.

An asynchronous task is a function of two primitives. The first is the future
provided by the user that is intended to be polled on the executor. The second
is a scheduler function provided by the executor that is used to indicate that
a future is ready and should be scheduled as soon as possible.

At a low level, [`async-task`] can be seen as putting a future on the heap and
providing a handle that can be used by executors. The [`Runnable`] is the part
of the task that represents the future to be run. When the [`Runnable`] is
spawned to the existence by the coroutine indicating that it is ready, it can
be either run to poll the future once and potentially return a value to the
user, or dropped to cancel the future and drop the task. The [`Task`] is the
user-facing handle. It returns `Pending` when polled until the [`Runnable`] is
ran and the underlying future returns a value.

The task handle can be modeled like this:

```rust
enum State<T> {
    Running(Pin<Box<dyn Future<Item = T>>>),
    Finished(T)
}

struct Inner<T> {
    task: Mutex<State<T>>,
    waker: AtomicWaker
}

pub struct Task<T> {
    inner: Weak<Inner<T>>
}

pub struct Runnable<T> {
    inner: Arc<Inner<T>>,
}
```

Running the [`Runnable`] polls the future once and, if it succeeds, stores the
result of the future inside of the task. Polling the [`Task`] sees if the inner
future has finished yet. If it fails, it stores its [`Waker`] inside of the
state and waits until the [`Runnable`] finishes.

In practice the actual implementation is much more optimized, is lock-free, only
involves a single heap allocation and supports many more features.

### [`event-listener`]

[`event-listener`] is [`atomic-waker`] on steroids. It supports an unbounded
number of tasks waiting on an event, as well as an unbounded number of users
activating that event.

The core of [`event-listener`] is based around a linked list containing the
wakers of the tasks that are waiting on it. When the event is activated, a
number of wakers are popped off of this linked list and woken.

TODO: More in-depth explanation

### [`async-signal`]

TODO: Explain this!

### [`async-io`]

TODO: Explain this!

### [`blocking`]

TODO: Explain this!

## Higher-Level Crates

These are high-level crates, built with the intention of being used in both
libraries and user programs.

### [`futures-lite`]

TODO: Explain this!

### [`async-channel`]

TODO: Explain this!

### [`async-lock`]

TODO: Explain this!

### [`async-executor`]

TODO: Explain this!

### [`async-net`]

TODO: Explain this!

### [`async-fs`]

TODO: Explain this!

### [`async-process`]

TODO: Explain this!

[`smol-rs`]: https://github.com/smol-rs
[`smol`]: https://github.com/smol-rs/smol
[`async-channel`]: https://github.com/smol-rs/async-channel
[`async-executor`]: https://github.com/smol-rs/async-executor
[`async-fs`]: https://github.com/smol-rs/async-fs
[`async-io`]: https://github.com/smol-rs/async-io
[`async-lock`]: https://github.com/smol-rs/async-lock
[`async-net`]: https://github.com/smol-rs/async-net
[`async-process`]: https://github.com/smol-rs/async-process
[`async-signal`]: https://github.com/smol-rs/async-signal
[`async-task`]: https://github.com/smol-rs/async-task
[`atomic-waker`]: https://github.com/smol-rs/atomic-waker
[`blocking`]: https://github.com/smol-rs/blocking
[`concurrent-queue`]: https://github.com/smol-rs/concurrent-queue
[`event-listener`]: https://github.com/smol-rs/event-listener
[`fastrand`]: https://github.com/smol-rs/fastrand
[`futures-lite`]: https://github.com/smol-rs/futures-lite
[`parking`]: https://github.com/smol-rs/parking
[`piper`]: https://github.com/smol-rs/piper
[`polling`]: https://github.com/smol-rs/polling
[`waker-fn`]: https://github.com/smol-rs/waker-fn

[`std::thread::park`]: https://doc.rust-lang.org/std/thread/fn.park.html
[`Mutex`]: https://doc.rust-lang.org/std/sync/struct.Mutex.html
[`Condvar`]: https://doc.rust-lang.org/std/sync/struct.Condvar.html

[`Wake`]: https://doc.rust-lang.org/std/task/trait.Wake.html
[`Waker`]: https://doc.rust-lang.org/std/task/struct.Waker.html

[`AtomicWaker`]: https://docs.rs/atomic-waker/latest/atomic_waker/

[`fastrand-contrib`]: https://github.com/smol-rs/fastrand-contrib
[wyrand]: https://github.com/wangyi-fudan/wyhash

[`crossbeam-queue`]: https://github.com/crossbeam-rs/crossbeam/tree/master/crossbeam-queue

[`epoll`]: https://en.wikipedia.org/wiki/Epoll
[`kqueue`]: https://en.wikipedia.org/wiki/Kqueue
[IOCP]: https://learn.microsoft.com/en-us/windows/win32/fileio/i-o-completion-ports
[`poll`]: https://en.wikipedia.org/wiki/Poll_(Unix)
[`AFD`]: https://2023.notgull.net/device-afd/

[`Runnable`]: https://docs.rs/async-task/latest/async_task/struct.Runnable.html
[`Task`]: https://docs.rs/async-task/latest/async_task/struct.Task.html

[work-stealing]: https://en.wikipedia.org/wiki/Work_stealing
[one-shot]: https://github.com/smol-rs/polling
