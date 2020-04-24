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
        use tokio::runtime::Runtime;

        static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("cannot initialize tokio"));

        RT.enter(f)
    }
}
