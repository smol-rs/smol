//! The thread-local executor.
//!
//! Tasks created by [`Task::local()`] go into this executor on the
//! wasm32 backend.

use core::future::Future;
use core::marker::PhantomData;
use crate::task::Task;

/// An executor for the web environment Thread-local tasks are spawned
/// by calling [`Task::local()`] and their futures do not have to
/// implement [`Send`].
pub(crate) struct WebExecutor {}

impl WebExecutor {

    /// Spawns a future onto this executor.
    ///
    /// Returns a [`Task`] handle for the spawned task.
    pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
        Task(PhantomData)
    }

}
