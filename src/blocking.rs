//! The blocking executor.
//!
//! Tasks created by [`Task::blocking()`][`crate::Task::blocking()`] go into this executor. This
//! executor is independent of [`run()`][`crate::run()`] - it does not need to be driven.
//!
//! Blocking tasks are allowed to block without restrictions. However, the executor puts a limit on
//! the number of concurrently running tasks. Once that limit is hit, a task will need to complete
//! or yield in order for others to run.
//!
//! In idle state, this executor has no threads and consumes no resources. Once tasks are spawned,
//! new threads will get started, as many as is needed to keep up with the present amount of work.
//! When threads are idle, they wait for some time for new work to come in and shut down after a
//! certain timeout.
//!
//! This module also implements convenient adapters:
//!
//! - [`blocking!`] as syntax sugar around [`Task::blocking()`][`crate::Task::blocking()`]
//! - [`iter()`] converts an [`Iterator`] into a [`Stream`]
//! - [`reader()`] converts a [`Read`] into an [`AsyncRead`]
//! - [`writer()`] converts a [`Write`] into an [`AsyncWrite`]

use std::io::{Read, Write};

use futures_io::{AsyncRead, AsyncWrite};
use futures_util::stream::Stream;

/// Spawns blocking code onto a thread.
///
/// Note that `blocking!(expr)` is just syntax sugar for
/// `Task::blocking(async move { expr }).await`.
///
/// # Examples
///
/// Read a file into a string:
///
/// ```no_run
/// use smol::blocking;
/// use std::fs;
///
/// # smol::run(async {
/// let contents = blocking!(fs::read_to_string("file.txt"))?;
/// # std::io::Result::Ok(()) });
/// ```
///
/// Spawn a process:
///
/// ```no_run
/// use smol::blocking;
/// use std::process::Command;
///
/// # smol::run(async {
/// let out = blocking!(Command::new("dir").output())?;
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
/// This adapter converts any kind of synchronous iterator into an asynchronous stream by running
/// it on the blocking executor and sending items back over a channel.
///
/// # Examples
///
/// List files in the current directory:
///
/// ```no_run
/// use futures::stream::StreamExt;
/// use smol::{blocking, iter};
/// use std::fs;
///
/// # smol::run(async {
/// // Load a directory.
/// let mut dir = blocking!(fs::read_dir("."))?;
/// let mut dir = iter(dir);
///
/// // Iterate over the contents of the directory.
/// while let Some(res) = dir.next().await {
///     println!("{}", res?.file_name().to_string_lossy());
/// }
/// # std::io::Result::Ok(()) });
/// ```
pub fn iter<T: Send + 'static>(
    iter: impl Iterator<Item = T> + Send + 'static,
) -> impl Stream<Item = T> + Send + Unpin + 'static {
    ::blocking::Blocking::new(iter)
}

/// Creates an async reader that runs on a thread.
///
/// This adapter converts any kind of synchronous reader into an asynchronous reader by running it
/// on the blocking executor and sending bytes back over a pipe.
///
/// # Examples
///
/// Read from a file:
///
/// ```no_run
/// use futures::prelude::*;
/// use smol::{blocking, reader};
/// use std::fs::File;
///
/// # smol::run(async {
/// // Open a file for reading.
/// let file = blocking!(File::open("foo.txt"))?;
/// let mut file = reader(file);
///
/// // Read the whole file.
/// let mut contents = Vec::new();
/// file.read_to_end(&mut contents).await?;
/// # std::io::Result::Ok(()) });
/// ```
///
/// Read output from a process:
///
/// ```no_run
/// use futures::prelude::*;
/// use smol::reader;
/// use std::process::{Command, Stdio};
///
/// # smol::run(async {
/// // Spawn a child process and make an async reader for its stdout.
/// let child = Command::new("dir").stdout(Stdio::piped()).spawn()?;
/// let mut child_stdout = reader(child.stdout.unwrap());
///
/// // Read the entire output.
/// let mut output = String::new();
/// child_stdout.read_to_string(&mut output).await?;
/// # std::io::Result::Ok(()) });
/// ```
pub fn reader(reader: impl Read + Send + 'static) -> impl AsyncRead + Send + Unpin + 'static {
    ::blocking::Blocking::new(reader)
}

/// Creates an async writer that runs on a thread.
///
/// This adapter converts any kind of synchronous writer into an asynchronous writer by running it
/// on the blocking executor and receiving bytes over a pipe.
///
/// **Note:** Don't forget to flush the writer at the end, or some written bytes might get lost!
///
/// # Examples
///
/// Write into a file:
///
/// ```no_run
/// use futures::prelude::*;
/// use smol::{blocking, writer};
/// use std::fs::File;
///
/// # smol::run(async {
/// // Open a file for writing.
/// let file = blocking!(File::open("foo.txt"))?;
/// let mut file = writer(file);
///
/// // Write some bytes into the file and flush.
/// file.write_all(b"hello").await?;
/// file.flush().await?;
/// # std::io::Result::Ok(()) });
/// ```
///
/// Write into standard output:
///
/// ```no_run
/// use futures::prelude::*;
/// use smol::writer;
///
/// # smol::run(async {
/// // Create an async writer to stdout.
/// let mut stdout = writer(std::io::stdout());
///
/// // Write a message and flush.
/// stdout.write_all(b"hello").await?;
/// stdout.flush().await?;
/// # std::io::Result::Ok(()) });
/// ```
pub fn writer(writer: impl Write + Send + 'static) -> impl AsyncWrite + Send + Unpin + 'static {
    ::blocking::Blocking::new(writer)
}
