//! The blocking executor.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, Read, Write};
use std::panic;
use std::pin::Pin;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use futures::io::{AllowStdIo, AsyncRead, AsyncWrite, AsyncWriteExt};
use futures::stream::Stream;
use once_cell::sync::Lazy;

use crate::context;
use crate::task::{Runnable, Task};
use crate::throttle;

// TODO: docs

/// A thread pool for blocking tasks.
pub(crate) struct BlockingExecutor {
    state: Mutex<State>,
    cvar: Condvar,
}

struct State {
    /// Number of sleeping threads in the pool.
    idle_count: usize,
    /// Total number of thread in the pool.
    thread_count: usize,
    /// Runnable blocking tasks.
    queue: VecDeque<Runnable>,
}

impl BlockingExecutor {
    /// Returns a reference to the blocking executor.
    pub fn get() -> &'static BlockingExecutor {
        static EXECUTOR: Lazy<BlockingExecutor> = Lazy::new(|| BlockingExecutor {
            state: Mutex::new(State {
                idle_count: 0,
                thread_count: 0,
                queue: VecDeque::new(),
            }),
            cvar: Condvar::new(),
        });
        &EXECUTOR
    }

    /// Spawns a future onto this executor.
    ///
    /// Returns a `Task` handle for the spawned task.
    pub fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        // Create a task, schedule it, and return its `Task` handle.
        let (runnable, handle) = async_task::spawn(future, move |r| self.schedule(r), ());
        runnable.schedule();
        Task(Some(handle))
    }

    /// Runs the main loop on the current thread.
    fn main_loop(&'static self) {
        let mut state = self.state.lock().unwrap();
        loop {
            state.idle_count -= 1;

            // Run tasks in the queue.
            while let Some(runnable) = state.queue.pop_front() {
                self.grow_pool(state);
                let _ = panic::catch_unwind(|| runnable.run());
                state = self.state.lock().unwrap();
            }

            // Put the thread to sleep until another task is scheduled.
            state.idle_count += 1;
            let timeout = Duration::from_millis(500);
            let (s, res) = self.cvar.wait_timeout(state, timeout).unwrap();
            state = s;

            if res.timed_out() && state.queue.is_empty() {
                // If there are no tasks after a while, stop this thread.
                state.idle_count -= 1;
                state.thread_count -= 1;
                break;
            }
        }
    }

    /// Schedules a runnable task for execution.
    fn schedule(&'static self, runnable: Runnable) {
        let mut state = self.state.lock().unwrap();
        state.queue.push_back(runnable);
        // Notify a sleeping thread and spawn more threads if needed.
        self.cvar.notify_one();
        self.grow_pool(state);
    }

    /// Spawns more blocking threads if the pool is overloaded with work.
    fn grow_pool(&'static self, mut state: MutexGuard<'static, State>) {
        // If runnable tasks greatly outnumber idle threads and there aren't too many threads
        // already, then be aggressive: wake all idle threads and spawn one more thread.
        while state.queue.len() > state.idle_count * 5 && state.thread_count < 500 {
            state.idle_count += 1;
            state.thread_count += 1;
            self.cvar.notify_all();

            thread::spawn(move || {
                // If enabled, set up tokio before the main loop begins.
                context::enter(|| self.main_loop()) // TODO
            });
        }
    }
}

/// Spawns blocking code onto a thread.
///
/// TODO
///
/// # Examples
///
/// ```no_run
/// use smol::blocking;
/// use std::fs;
///
/// # smol::run(async {
/// let contents = blocking!(fs::read_to_string("file.txt"))?;
/// # std::io::Result::Ok(()) });
/// ```
#[macro_export]
macro_rules! blocking {
    ($($expr:tt)*) => {
        $crate::Task::blocking(async move { $($expr)* }).await
    };
}

/// Creates a stream that iterates on a thread.
///
/// TODO
///
/// # Examples
///
/// List files in the current directory:
///
/// ```no_run
/// use smol::blocking;
/// use std::fs;
///
/// # smol::run(async {
/// let mut dir = smol::iter(blocking!(fs::read_dir("."))?);
///
/// while let Some(res) = dir.next().await {
///     println!("{}", res?.file_name().to_string_lossy());
/// }
/// # std::io::Result::Ok(()) });
/// ```
pub fn iter<T: Send + 'static>(
    iter: impl Iterator<Item = T> + Send + 'static,
) -> impl Stream<Item = T> + Send + Unpin + 'static {
    /// Current state of the iterator.
    enum State<T, I> {
        /// The iterator is idle.
        Idle(Option<I>),
        /// The iterator is running in a blocking task and sending items into a channel.
        Busy(piper::Receiver<T>, Task<I>),
    }

    impl<T, I> Unpin for State<T, I> {}

    impl<T: Send + 'static, I: Iterator<Item = T> + Send + 'static> Stream for State<T, I> {
        type Item = T;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(throttle::poll(cx));

            match &mut *self {
                State::Idle(iter) => {
                    // If idle, take the iterator out to run it on a blocking task.
                    let mut iter = iter.take().unwrap();

                    // This channel capacity seems to work well in practice. If it's too low, there
                    // will be too much synchronization between tasks. If too high, memory
                    // consumption increases.
                    let (sender, receiver) = piper::chan(8 * 1024); // 8192 items

                    // Spawn a blocking task that runs the iterator and returns it when done.
                    let task = Task::blocking(async move {
                        for item in &mut iter {
                            sender.send(item).await;
                        }
                        iter
                    });

                    // Move into the busy state and poll again.
                    *self = State::Busy(receiver, task);
                    self.poll_next(cx)
                }
                State::Busy(receiver, task) => {
                    // Poll the channel.
                    let opt = futures::ready!(Pin::new(receiver).poll_next(cx));

                    // If the channel is closed, retrieve the iterator back from the blocking task.
                    // This is not really a required step, but it's cleaner to drop the iterator on
                    // the same thread that created it.
                    if opt.is_none() {
                        // Poll the task to retrieve the iterator.
                        let iter = futures::ready!(Pin::new(task).poll(cx));
                        *self = State::Idle(Some(iter));
                    }

                    Poll::Ready(opt)
                }
            }
        }
    }

    State::Idle(Some(iter))
}

/// Creates an async reader that runs on a thread.
pub fn reader(reader: impl Read + Send + 'static) -> impl AsyncRead + Send + Unpin + 'static {
    /// Current state of the reader.
    enum State<T> {
        /// The reader is idle.
        Idle(Option<T>),
        /// The reader is running in a blocking task and sending bytes into a pipe.
        Busy(piper::Reader, Task<(io::Result<()>, T)>),
    }

    impl<T: AsyncRead + Send + Unpin + 'static> AsyncRead for State<T> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(throttle::poll(cx));

            match &mut *self {
                State::Idle(io) => {
                    // If idle, take the I/O handle out to read it on a blocking task.
                    let mut io = io.take().unwrap();

                    // This pipe capacity seems to work well in practice. If it's too low, there
                    // will be too much synchronization between tasks. If too high, memory
                    // consumption increases.
                    let (reader, mut writer) = piper::pipe(8 * 1024 * 1024); // 8 MB

                    // Spawn a blocking task that reads and returns the I/O handle when done.
                    let task = Task::blocking(async move {
                        // Copy bytes from the I/O handle into the pipe until the pipe is closed or
                        // an error occurs.
                        let res = futures::io::copy(&mut io, &mut writer).await;
                        (res.map(drop), io)
                    });

                    // Move into the busy state and poll again.
                    *self = State::Busy(reader, task);
                    self.poll_read(cx, buf)
                }
                State::Busy(reader, task) => {
                    // Poll the pipe.
                    let n = futures::ready!(Pin::new(reader).poll_read(cx, buf))?;

                    // If the pipe is closed, retrieve the I/O handle back from the blocking task.
                    // This is not really a required step, but it's cleaner to drop the handle on
                    // the same thread that created it.
                    if n == 0 {
                        // Poll the task to retrieve the I/O handle.
                        let (res, io) = futures::ready!(Pin::new(task).poll(cx));
                        // Make sure to move into the idle state before reporting errors.
                        *self = State::Idle(Some(io));
                        res?;
                    }

                    Poll::Ready(Ok(n))
                }
            }
        }
    }

    // It's okay to treat the `Read` type as `AsyncRead` because it's only read from inside a
    // blocking task.
    let io = Box::pin(AllowStdIo::new(reader));
    State::Idle(Some(io))
}

/// Creates an async writer that runs on a thread.
///
/// Make sure to flush at the end.
///
/// TODO
pub fn writer(writer: impl Write + Send + 'static) -> impl AsyncWrite + Send + Unpin + 'static {
    /// Current state of the writer.
    enum State<T> {
        /// The writer is idle.
        Idle(Option<T>),
        /// The writer is running in a blocking task and receiving bytes from a pipe.
        Busy(Option<piper::Writer>, Task<(io::Result<()>, T)>),
    }

    impl<T: AsyncWrite + Send + Unpin + 'static> State<T> {
        /// Starts a blocking task.
        fn start(&mut self) {
            if let State::Idle(io) = self {
                // If idle, take the I/O handle out to write on a blocking task.
                let mut io = io.take().unwrap();

                // This pipe capacity seems to work well in practice. If it's too low, there will
                // be too much synchronization between tasks. If too high, memory consumption
                // increases.
                let (reader, writer) = piper::pipe(8 * 1024 * 1024); // 8 MB

                // Spawn a blocking task that writes and returns the I/O handle when done.
                let task = Task::blocking(async move {
                    // Copy bytes from the pipe into the I/O handle until the pipe is closed or an
                    // error occurs. Flush the I/O handle at the end.
                    match futures::io::copy(reader, &mut io).await {
                        Ok(_) => (io.flush().await, io),
                        Err(err) => (Err(err), io),
                    }
                });
                // Move into the busy state.
                *self = State::Busy(Some(writer), task);
            }
        }
    }

    impl<T: AsyncWrite + Send + Unpin + 'static> AsyncWrite for State<T> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(throttle::poll(cx));

            loop {
                match &mut *self {
                    // The writer is idle and closed.
                    State::Idle(None) => return Poll::Ready(Ok(0)),

                    // The writer is idle and open - start a blocking task.
                    State::Idle(Some(_)) => self.start(),

                    // The task is flushing and in process of stopping.
                    State::Busy(None, task) => {
                        // Poll the task to retrieve the I/O handle.
                        let (res, io) = futures::ready!(Pin::new(task).poll(cx));
                        // Make sure to move into the idle state before reporting errors.
                        *self = State::Idle(Some(io));
                        res?;
                    }

                    // The writer is busy - write more bytes into the pipe.
                    State::Busy(Some(writer), _) => return Pin::new(writer).poll_write(cx, buf),
                }
            }
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            // Throttle if the current task has done too many I/O operations without yielding.
            futures::ready!(throttle::poll(cx));

            loop {
                match &mut *self {
                    // The writer is idle and closed.
                    State::Idle(None) => return Poll::Ready(Ok(())),

                    // The writer is idle and open - start a blocking task.
                    State::Idle(Some(_)) => self.start(),

                    // The task is busy.
                    State::Busy(writer, task) => {
                        // Drop the writer to close the pipe. This stops the `futures::io::copy`
                        // operation in the task, after which the task flushes the I/O handle and
                        // returns it back.
                        writer.take();

                        // Poll the task to retrieve the I/O handle.
                        let (res, io) = futures::ready!(Pin::new(task).poll(cx));
                        // Make sure to move into the idle state before reporting errors.
                        *self = State::Idle(Some(io));
                        return Poll::Ready(res);
                    }
                }
            }
        }

        fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            // First, make sure the I/O handle is flushed.
            futures::ready!(Pin::new(&mut *self).poll_flush(cx))?;

            // Then move into the idle state with no I/O handle, thus dropping it.
            *self = State::Idle(None);
            Poll::Ready(Ok(()))
        }
    }

    // It's okay to treat the `Write` type as `AsyncWrite` because it's only written to inside a
    // blocking task.
    let io = AllowStdIo::new(writer);
    State::Idle(Some(io))
}
