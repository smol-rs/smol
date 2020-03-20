#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod chan;
mod mutex;
mod pipe;
mod signal;

pub use chan::{chan, Receiver, Sender};
pub use mutex::{Mutex, MutexGuard};
pub use pipe::{pipe, Reader, Writer};
pub use signal::{Signal, SignalListener};

macro_rules! select {
    ($p:pat = $e:expr => $body:expr, $($rest:tt)*) => {
        println!("PATTERN");
        select!($($rest)*)
    };

    (default => $body:expr, $($rest:tt)*) => {
        println!("DEFAULT");
        select!($($rest)*)
    };

    // Optional comma after the last expression.
    ($p:pat = $e:expr => $body:expr) => {
        select!($p = $e => { $body },);
    };
    (default => $body:expr) => {
        select!(default => { $body },);
    };
    // Optional comma after block expressions.
    ($p:pat = $e:expr => $body:block $($rest:tt)*) => {
        select!($p = $e => $body, $($rest)*)
    };
    (default => $body:block $($rest:tt)*) => {
        select!(default => $body, $($rest)*)
    };
    // Optional pattern.
    ($e:expr => $($rest:tt)*) => {
        select!(_ = $e => $($rest)*)
    };
    // End of the macro.
    () => {};
}
