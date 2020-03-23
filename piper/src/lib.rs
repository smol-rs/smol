#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod chan;
mod mutex;
mod pipe;
mod signal;

pub use chan::{chan, Receiver, Sender};
pub use mutex::{Mutex, MutexGuard};
pub use pipe::{pipe, Reader, Writer};
pub use signal::{Signal, SignalListener};

#[doc(hidden)]
pub use futures;

#[macro_export]
macro_rules! select {
    ([$($tokens:tt)*] default => $($tail:tt)*) => {
        $crate::select!([$($tokens)* default =>] $($tail)*)
    };
    ([$($tokens:tt)*] $p:pat = $f:expr => $($tail:tt)*) => {
        $crate::select!(
            [$($tokens)* $p = $crate::futures::future::FutureExt::fuse($f) =>] $($tail)*
        )
    };
    ([$($tokens:tt)*] $f:expr => $($tail:tt)*) => {
        $crate::select!([$($tokens)*] _ = $f => $($tail)*)
    };
    ([$($tokens:tt)*] $t:tt $($tail:tt)*) => { $crate::select!([$($tokens)* $t] $($tail)*) };
    ([$($tokens:tt)*]) => { $crate::futures::select! { $($tokens)* } };
    ($($tokens:tt)*) => { $crate::select!([] $($tokens)*) };
}
