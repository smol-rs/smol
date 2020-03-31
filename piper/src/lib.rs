//! Asynchronous pipes, channels, and mutexes.

#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod chan;
mod mutex;
mod pipe;
mod signal;

pub use chan::{chan, Receiver, Sender};
pub use mutex::{Mutex, MutexGuard};
pub use pipe::{pipe, Reader, Writer};
