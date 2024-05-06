use std::cell::Cell;
use std::future::Future;
use std::panic::catch_unwind;
use std::thread;

use async_executor::{Executor, Task};
use async_lock::OnceCell;
use futures_lite::future;

thread_local! {
    static SMOL: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct EnterScope {
    first: bool,
}

// Conforms to msrv 1.63 which does not have stable get/set on LocalKey<Cell<T>>.
impl EnterScope {
    fn get() -> bool {
        SMOL.with(|cell| cell.get())
    }

    fn set(scope: bool) {
        SMOL.with(|cell| cell.set(scope))
    }
}

impl Drop for EnterScope {
    fn drop(&mut self) {
        if self.first {
            EnterScope::set(false)
        }
    }
}

pub(crate) fn enter() -> EnterScope {
    let smol = EnterScope::get();
    EnterScope::set(true);
    EnterScope { first: !smol }
}

/// Gets executor if runs in scope of smol, that is [block_on], [unblock] and tasks spawnned from
/// [crate::spawn()].
pub fn try_executor() -> Option<&'static Executor<'static>> {
    if EnterScope::get() {
        Some(global())
    } else {
        None
    }
}

/// Same as [async_io::block_on] expect it setup thread context executor for [try_executor].
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    let _scope = enter();
    async_io::block_on(future)
}

/// Same as [blocking::unblock] expect it setup thread context executor for [try_executor].
pub fn unblock<T, F>(f: F) -> Task<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    blocking::unblock(move || {
        let _scope = enter();
        f()
    })
}

pub(crate) fn global() -> &'static Executor<'static> {
    static GLOBAL: OnceCell<Executor<'_>> = OnceCell::new();
    GLOBAL.get_or_init_blocking(|| {
        let num_threads = {
            // Parse SMOL_THREADS or default to 1.
            std::env::var("SMOL_THREADS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1)
        };

        for n in 1..=num_threads {
            thread::Builder::new()
                .name(format!("smol-{}", n))
                .spawn(|| loop {
                    catch_unwind(|| block_on(global().run(future::pending::<()>()))).ok();
                })
                .expect("cannot spawn executor thread");
        }

        // Prevent spawning another thread by running the process driver on this thread.
        let ex = Executor::new();
        #[cfg(not(target_os = "espidf"))]
        ex.spawn(async_process::driver()).detach();
        ex
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future;

    #[test]
    fn try_executor_no() {
        assert!(try_executor().is_none());
    }

    #[test]
    fn try_executor_block_on() {
        let executor = block_on(async { try_executor().unwrap() });
        assert!(std::ptr::eq(executor, global()));
    }

    #[test]
    fn try_executor_block_on_recursively() {
        let executor = block_on(async { block_on(async { try_executor().unwrap() }) });
        assert!(std::ptr::eq(executor, global()));
    }

    #[test]
    fn try_executor_unblock() {
        let executor = future::block_on(unblock(|| try_executor().unwrap()));
        assert!(std::ptr::eq(executor, global()));
    }

    #[test]
    fn try_executor_spawn() {
        for _ in 0..100 {
            let executor = future::block_on(crate::spawn(async { try_executor().unwrap() }));
            assert!(std::ptr::eq(executor, global()));
        }
    }
}
