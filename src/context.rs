//! Task context common to all executors.
//!
//! Before executor, we "enter" it by setting up some necessary thread-locals.

/// Enters the tokio context if the `tokio` feature is enabled.
pub(crate) fn enter<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(not(feature = "tokio02"))]
    return f();

    #[cfg(feature = "tokio02")]
    {
        use once_cell::sync::Lazy;
        use std::cell::Cell;
        use tokio::runtime::Runtime;

        thread_local! {
            /// The level of nested `enter` calls we are in, to ensure that the outer most always has a
            /// runtime spawned.
            static NESTING: Cell<usize> = Cell::new(0);
        }

        static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("cannot initialize tokio"));

        NESTING.with(|nesting| {
            let res = if nesting.get() == 0 {
                nesting.replace(1);
                RT.enter(f)
            } else {
                nesting.replace(nesting.get() + 1);
                f()
            };
            nesting.replace(nesting.get() - 1);
            res
        })
    }
}
