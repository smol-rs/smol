use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::io::{self, AsyncRead, AsyncWrite};

/// A reference-counting pointer that implements async I/O traits.
///
/// This is just a simple wrapper around `Arc<T>` that adds the following impls:
///
/// - `impl<T> AsyncRead for Shared<T> where &T: AsyncRead {}`
/// - `impl<T> AsyncWrite for Shared<T> where &T: AsyncWrite {}`
///
/// # Examples
///
/// ```no_run
/// use futures::io;
/// use piper::Shared;
/// use smol::Async;
/// use std::net::TcpStream;
///
/// # fn main() -> std::io::Result<()> { smol::run(async {
/// // A client that echoes messages back to the server.
/// let stream = Async::<TcpStream>::connect("127.0.0.1:8000").await?;
///
/// // Create two handles to the stream.
/// let reader = Shared::new(stream);
/// let mut writer = reader.clone();
///
/// // Echo data received from the reader back into the writer.
/// io::copy(reader, &mut writer).await?;
/// # Ok(()) }) }
/// ```
pub struct Shared<T>(pub Arc<T>);

impl<T> Unpin for Shared<T> {}

impl<T> Shared<T> {
    /// Constructs a new `Shared<T>`.
    ///
    /// # Examples
    ///
    /// ```
    /// use piper::Shared;
    /// use std::sync::Arc;
    ///
    /// // These two lines are equivalent:
    /// let a = Shared::new(7);
    /// let a = Shared(Arc::new(7));
    /// ```
    pub fn new(data: T) -> Shared<T> {
        Shared(Arc::new(data))
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Shared<T> {
        Shared(self.0.clone())
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: fmt::Debug> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: fmt::Display> fmt::Display for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T: Hash> Hash for Shared<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl<T> fmt::Pointer for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&(&**self as *const T), f)
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self(Arc::new(Default::default()))
    }
}

impl<T> From<T> for Shared<T> {
    fn from(t: T) -> Self {
        Self(Arc::new(t))
    }
}

// NOTE(stjepang): It would also make sense to have the following impls:
//
// - `impl<T> AsyncRead for &Shared<T> where &T: AsyncRead {}`
// - `impl<T> AsyncWrite for &Shared<T> where &T: AsyncWrite {}`
//
// However, those impls sometimes make Rust's type inference try too hard when types cannot be
// inferred. In the end, instead of complaining with a nice error message, the Rust compiler ends
// up overflowing and dumping a very long error message spanning multiple screens.
//
// Since those impls are not essential, I decided to err on the safe side and not include them.

impl<T> AsyncRead for Shared<T>
where
    for<'a> &'a T: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self.0).poll_read(cx, buf)
    }
}

impl<T> AsyncWrite for Shared<T>
where
    for<'a> &'a T: AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self.0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.0).poll_close(cx)
    }
}
