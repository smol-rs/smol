//! Abstraction over [epoll]/[kqueue]/[wepoll].
//!
//! [epoll]: https://en.wikipedia.org/wiki/Epoll
//! [kqueue]: https://en.wikipedia.org/wiki/Kqueue
//! [wepoll]: https://github.com/piscisaureus/wepoll

use std::future::Future;
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, IntoRawSocket, RawSocket};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
#[cfg(unix)]
use std::{
    os::unix::io::{AsRawFd, IntoRawFd, RawFd},
    os::unix::net::{SocketAddr as UnixSocketAddr, UnixDatagram, UnixListener, UnixStream},
    path::Path,
};

use futures_io::{AsyncRead, AsyncWrite};
use futures_util::future;
use futures_util::stream::{self, Stream};
use socket2::{Domain, Protocol, Socket, Type};

use crate::reactor::{Reactor, Source};
use crate::task::Task;

/// Async I/O.
///
/// This type converts a blocking I/O type into an async type, provided it is supported by
/// [epoll]/[kqueue]/[wepoll].
///
/// I/O operations can then be *asyncified* by methods [`Async::with()`] and [`Async::with_mut()`],
/// or you can use the predefined async methods on the standard networking types.
///
/// **NOTE**: Do not use this type with [`File`][`std::fs::File`], [`Stdin`][`std::io::Stdin`],
/// [`Stdout`][`std::io::Stdout`], or [`Stderr`][`std::io::Stderr`] because they're not
/// supported. Use [`reader()`][`crate::reader()`] and [`writer()`][`crate::writer()`] functions
/// instead to read/write on a thread.
///
/// # Examples
///
/// To make an async I/O handle cloneable, wrap it in [piper]'s `Arc`:
///
/// ```no_run
/// use piper::Arc;
/// use smol::Async;
/// use std::net::TcpStream;
///
/// # smol::run(async {
/// // Connect to a local server.
/// let stream = Async::<TcpStream>::connect("127.0.0.1:8000").await?;
///
/// // Create two handles to the stream.
/// let reader = Arc::new(stream);
/// let mut writer = reader.clone();
///
/// // Echo all messages from the read side of the stream into the write side.
/// futures::io::copy(reader, &mut writer).await?;
/// # std::io::Result::Ok(()) });
/// ```
///
/// If a type does but its reference doesn't implement [`AsyncRead`] and [`AsyncWrite`], wrap it in
/// [piper]'s `Mutex`:
///
/// ```no_run
/// use futures::prelude::*;
/// use piper::{Arc, Mutex};
/// use smol::Async;
/// use std::net::TcpStream;
///
/// # smol::run(async {
/// // Reads data from a stream and echoes it back.
/// async fn echo(stream: impl AsyncRead + AsyncWrite + Unpin) -> std::io::Result<u64> {
///     let stream = Mutex::new(stream);
///
///     // Create two handles to the stream.
///     let reader = Arc::new(stream);
///     let mut writer = reader.clone();
///
///     // Echo all messages from the read side of the stream into the write side.
///     futures::io::copy(reader, &mut writer).await
/// }
///
/// // Connect to a local server and echo its messages back.
/// let stream = Async::<TcpStream>::connect("127.0.0.1:8000").await?;
/// echo(stream).await?;
/// # std::io::Result::Ok(()) });
/// ```
///
/// [piper]: https://docs.rs/piper
/// [epoll]: https://en.wikipedia.org/wiki/Epoll
/// [kqueue]: https://en.wikipedia.org/wiki/Kqueue
/// [wepoll]: https://github.com/piscisaureus/wepoll
#[derive(Debug)]
pub struct Async<T> {
    /// A source registered in the reactor.
    source: Arc<Source>,

    /// The inner I/O handle.
    io: Option<Box<T>>,
}

#[cfg(unix)]
impl<T: AsRawFd> Async<T> {
    /// Creates an async I/O handle.
    ///
    /// This function will put the handle in non-blocking mode and register it in [epoll] on
    /// Linux/Android, [kqueue] on macOS/iOS/BSD, or [wepoll] on Windows.
    /// On Unix systems, the handle must implement `AsRawFd`, while on Windows it must implement
    /// `AsRawSocket`.
    ///
    /// If the handle implements [`Read`] and [`Write`], then `Async<T>` automatically
    /// implements [`AsyncRead`] and [`AsyncWrite`].
    /// Other I/O operations can be *asyncified* by methods [`Async::with()`] and
    /// [`Async::with_mut()`].
    ///
    /// **NOTE**: Do not use this type with [`File`][`std::fs::File`], [`Stdin`][`std::io::Stdin`],
    /// [`Stdout`][`std::io::Stdout`], or [`Stderr`][`std::io::Stderr`] because they're not
    /// supported by [epoll]/[kqueue]/[wepoll].
    /// Use [`reader()`][`crate::reader()`] and [`writer()`][`crate::writer()`] functions instead
    /// to read/write on a thread.
    ///
    /// [epoll]: https://en.wikipedia.org/wiki/Epoll
    /// [kqueue]: https://en.wikipedia.org/wiki/Kqueue
    /// [wepoll]: https://github.com/piscisaureus/wepoll
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = TcpListener::bind("127.0.0.1:80")?;
    /// let listener = Async::new(listener)?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn new(io: T) -> io::Result<Async<T>> {
        Ok(Async {
            source: Reactor::get().insert_io(io.as_raw_fd())?,
            io: Some(Box::new(io)),
        })
    }
}

#[cfg(unix)]
impl<T: AsRawFd> AsRawFd for Async<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.source.raw
    }
}

#[cfg(unix)]
impl<T: IntoRawFd> IntoRawFd for Async<T> {
    fn into_raw_fd(self) -> RawFd {
        self.into_inner().unwrap().into_raw_fd()
    }
}
#[cfg(windows)]
impl<T: AsRawSocket> Async<T> {
    /// Creates an async I/O handle.
    ///
    /// This function will put the handle in non-blocking mode and register it in [epoll] on
    /// Linux/Android, [kqueue] on macOS/iOS/BSD, or [wepoll] on Windows.
    /// On Unix systems, the handle must implement `AsRawFd`, while on Windows it must implement
    /// `AsRawSocket`.
    ///
    /// If the handle implements [`Read`] and [`Write`], then `Async<T>` automatically
    /// implements [`AsyncRead`] and [`AsyncWrite`].
    /// Other I/O operations can be *asyncified* by methods [`Async::with()`] and
    /// [`Async::with_mut()`].
    ///
    /// **NOTE**: Do not use this type with [`File`][`std::fs::File`], [`Stdin`][`std::io::Stdin`],
    /// [`Stdout`][`std::io::Stdout`], or [`Stderr`][`std::io::Stderr`] because they're not
    /// supported by epoll/kqueue/wepoll.
    /// Use [`reader()`][`crate::reader()`] and [`writer()`][`crate::writer()`] functions instead
    /// to read/write on a thread.
    ///
    /// [epoll]: https://en.wikipedia.org/wiki/Epoll
    /// [kqueue]: https://en.wikipedia.org/wiki/Kqueue
    /// [wepoll]: https://github.com/piscisaureus/wepoll
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = TcpListener::bind("127.0.0.1:80")?;
    /// let listener = Async::new(listener)?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn new(io: T) -> io::Result<Async<T>> {
        Ok(Async {
            source: Reactor::get().insert_io(io.as_raw_socket())?,
            io: Some(Box::new(io)),
        })
    }
}

#[cfg(windows)]
impl<T: AsRawSocket> AsRawSocket for Async<T> {
    fn as_raw_socket(&self) -> RawSocket {
        self.source.raw
    }
}

#[cfg(windows)]
impl<T: IntoRawSocket> IntoRawSocket for Async<T> {
    fn into_raw_socket(self) -> RawSocket {
        self.into_inner().unwrap().into_raw_socket()
    }
}

impl<T> Async<T> {
    /// Re-registers the I/O event to wake the poller.
    pub(crate) fn reregister_io_event(&self) -> io::Result<()> {
        self.source.reregister_io_event()
    }

    /// Gets a reference to the inner I/O handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    /// let inner = listener.get_ref();
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn get_ref(&self) -> &T {
        self.io.as_ref().unwrap()
    }

    /// Gets a mutable reference to the inner I/O handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let mut listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    /// let inner = listener.get_mut();
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn get_mut(&mut self) -> &mut T {
        self.io.as_mut().unwrap()
    }

    /// Unwraps the inner non-blocking I/O handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    /// let inner = listener.into_inner()?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn into_inner(mut self) -> io::Result<T> {
        let io = *self.io.take().unwrap();
        Reactor::get().remove_io(&self.source)?;
        Ok(io)
    }

    #[doc(hidden)]
    #[deprecated(note = "use `read_with()` or `write_with()` instead")]
    pub async fn with<R>(&self, op: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let io = self.io.as_ref().unwrap();
        loop {
            match op(io) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return res,
            }

            // Wait until the I/O handle is readable or writable.
            let readable = self.source.readable();
            let writable = self.source.writable();
            futures_util::pin_mut!(readable);
            futures_util::pin_mut!(writable);
            let _ = future::select(readable, writable).await;
        }
    }

    #[doc(hidden)]
    #[deprecated(note = "use `read_with_mut()` or `write_with_mut()` instead")]
    pub async fn with_mut<R>(&mut self, op: impl FnMut(&mut T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let io = self.io.as_mut().unwrap();
        loop {
            match op(io) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return res,
            }

            // Wait until the I/O handle is readable or writable.
            let readable = self.source.readable();
            let writable = self.source.writable();
            futures_util::pin_mut!(readable);
            futures_util::pin_mut!(writable);
            let _ = future::select(readable, writable).await;
        }
    }

    /// Performs a read operation asynchronously.
    ///
    /// The I/O handle is registered in the reactor and put in non-blocking mode. This function
    /// invokes the `op` closure in a loop until it succeeds or returns an error other than
    /// [`io::ErrorKind::WouldBlock`]. In between iterations of the loop, it waits until the OS
    /// sends a notification that the I/O handle is readable.
    ///
    /// The closure receives a shared reference to the I/O handle.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    ///
    /// // Accept a new client asynchronously.
    /// let (stream, addr) = listener.read_with(|l| l.accept()).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn read_with<R>(&self, op: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        loop {
            match op(self.get_ref()) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return res,
            }
            self.source.readable().await?;
        }
    }

    /// Performs a read operation asynchronously.
    ///
    /// The I/O handle is registered in the reactor and put in non-blocking mode. This function
    /// invokes the `op` closure in a loop until it succeeds or returns an error other than
    /// [`io::ErrorKind::WouldBlock`]. In between iterations of the loop, it waits until the OS
    /// sends a notification that the I/O handle is readable.
    ///
    /// The closure receives a mutable reference to the I/O handle.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let mut listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    ///
    /// // Accept a new client asynchronously.
    /// let (stream, addr) = listener.read_with_mut(|l| l.accept()).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn read_with_mut<R>(
        &mut self,
        op: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut op = op;
        loop {
            match op(self.get_mut()) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return res,
            }
            self.source.readable().await?;
        }
    }

    /// Performs a write operation asynchronously.
    ///
    /// The I/O handle is registered in the reactor and put in non-blocking mode. This function
    /// invokes the `op` closure in a loop until it succeeds or returns an error other than
    /// [`io::ErrorKind::WouldBlock`]. In between iterations of the loop, it waits until the OS
    /// sends a notification that the I/O handle is writable.
    ///
    /// The closure receives a shared reference to the I/O handle.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    /// socket.get_ref().connect("127.0.0.1:8000")?;
    ///
    /// let msg = b"hello";
    /// let len = socket.write_with(|s| s.send(msg)).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn write_with<R>(&self, op: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        loop {
            match op(self.get_ref()) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return res,
            }
            self.source.writable().await?;
        }
    }

    /// Performs a write operation asynchronously.
    ///
    /// The I/O handle is registered in the reactor and put in non-blocking mode. This function
    /// invokes the `op` closure in a loop until it succeeds or returns an error other than
    /// [`io::ErrorKind::WouldBlock`]. In between iterations of the loop, it waits until the OS
    /// sends a notification that the I/O handle is writable.
    ///
    /// The closure receives a mutable reference to the I/O handle.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let mut socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    /// socket.get_ref().connect("127.0.0.1:8000")?;
    ///
    /// let msg = b"hello";
    /// let len = socket.write_with_mut(|s| s.send(msg)).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn write_with_mut<R>(
        &mut self,
        op: impl FnMut(&mut T) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut op = op;
        loop {
            match op(self.get_mut()) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                res => return res,
            }
            self.source.writable().await?;
        }
    }
}

impl<T> Drop for Async<T> {
    fn drop(&mut self) {
        if self.io.is_some() {
            // Deregister and ignore errors because destructors should not panic.
            let _ = Reactor::get().remove_io(&self.source);

            // Drop the I/O handle to close it.
            self.io.take();
        }
    }
}

/// Pins a future and then polls it.
fn poll_future<T>(cx: &mut Context<'_>, fut: impl Future<Output = T>) -> Poll<T> {
    futures_util::pin_mut!(fut);
    fut.poll(cx)
}

impl<T: Read> AsyncRead for Async<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.read_with_mut(|io| io.read(buf)))
    }

    fn poll_read_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.read_with_mut(|io| io.read_vectored(bufs)))
    }
}

impl<T> AsyncRead for &Async<T>
where
    for<'a> &'a T: Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.read_with(|io| (&*io).read(buf)))
    }

    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.read_with(|io| (&*io).read_vectored(bufs)))
    }
}

impl<T: Write> AsyncWrite for Async<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.write_with_mut(|io| io.write(buf)))
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.write_with_mut(|io| io.write_vectored(bufs)))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.write_with_mut(|io| io.flush()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl<T> AsyncWrite for &Async<T>
where
    for<'a> &'a T: Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.write_with(|io| (&*io).write(buf)))
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.write_with(|io| (&*io).write_vectored(bufs)))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.write_with(|io| (&*io).flush()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

impl Async<TcpListener> {
    /// Creates a TCP listener bound to the specified address.
    ///
    /// Binding with port number 0 will request an available port from the OS.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    /// println!("Listening on {}", listener.get_ref().local_addr()?);
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn bind<A: ToString>(addr: A) -> io::Result<Async<TcpListener>> {
        let addr = addr
            .to_string()
            .parse::<SocketAddr>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Ok(Async::new(TcpListener::bind(addr)?)?)
    }

    /// Accepts a new incoming TCP connection.
    ///
    /// When a connection is established, it will be returned as a TCP stream together with its
    /// remote address.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    /// let (stream, addr) = listener.accept().await?;
    /// println!("Accepted client: {}", addr);
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn accept(&self) -> io::Result<(Async<TcpStream>, SocketAddr)> {
        let (stream, addr) = self.read_with(|io| io.accept()).await?;
        Ok((Async::new(stream)?, addr))
    }

    /// Returns a stream of incoming TCP connections.
    ///
    /// The stream is infinite, i.e. it never stops with a [`None`] item.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use futures::prelude::*;
    /// use smol::Async;
    /// use std::net::TcpListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<TcpListener>::bind("127.0.0.1:80")?;
    /// let mut incoming = listener.incoming();
    ///
    /// while let Some(stream) = incoming.next().await {
    ///     let stream = stream?;
    ///     println!("Accepted client: {}", stream.get_ref().peer_addr()?);
    /// }
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn incoming(&self) -> impl Stream<Item = io::Result<Async<TcpStream>>> + Send + Unpin + '_ {
        Box::pin(stream::unfold(self, |listener| async move {
            let res = listener.accept().await.map(|(stream, _)| stream);
            Some((res, listener))
        }))
    }
}

impl Async<TcpStream> {
    /// Creates a TCP connection to the specified address.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpStream;
    ///
    /// # smol::run(async {
    /// let stream = Async::<TcpStream>::connect("example.com:80").await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn connect<A: ToString>(addr: A) -> io::Result<Async<TcpStream>> {
        let addr = addr.to_string();
        let addr = Task::blocking(async move {
            addr.to_socket_addrs()?.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "could not resolve the address")
            })
        })
        .await?;

        // Create a socket.
        let domain = if addr.is_ipv6() {
            Domain::ipv6()
        } else {
            Domain::ipv4()
        };
        let socket = Socket::new(domain, Type::stream(), Some(Protocol::tcp()))?;

        // Begin async connect and ignore the inevitable "in progress" error.
        socket.set_nonblocking(true)?;
        socket.connect(&addr.into()).or_else(|err| {
            // Check for EINPROGRESS on Unix and WSAEWOULDBLOCK on Windows.
            #[cfg(unix)]
            let in_progress = err.raw_os_error() == Some(crate::sys::libc::EINPROGRESS);
            #[cfg(windows)]
            let in_progress = err.kind() == io::ErrorKind::WouldBlock;

            // If connect results with an "in progress" error, that's not an error.
            if in_progress {
                Ok(())
            } else {
                Err(err)
            }
        })?;
        let stream = Async::new(socket.into_tcp_stream())?;

        // Waits for connect to complete.
        let wait_connect = |mut stream: &TcpStream| match stream.write(&[]) {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => match stream.take_error()? {
                Some(err) => Err(err),
                None => Err(io::ErrorKind::WouldBlock.into()),
            },
            res => res.map(|_| ()),
        };

        // The stream becomes writable when connected.
        match stream.write_with(|io| wait_connect(io)).await {
            Ok(()) => Ok(stream),
            Err(err) => match stream.get_ref().take_error()? {
                Some(err) => Err(err),
                None => Err(err),
            },
        }
    }

    /// Reads data from the stream without removing it from the buffer.
    ///
    /// Returns the number of bytes read. Successive calls of this method read the same data.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::TcpStream;
    ///
    /// # smol::run(async {
    /// let stream = Async::<TcpStream>::connect("127.0.0.1:8080").await?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let len = stream.peek(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|io| io.peek(buf)).await
    }
}

impl Async<UdpSocket> {
    /// Creates a UDP socket bound to the specified address.
    ///
    /// Binding with port number 0 will request an available port from the OS.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    /// println!("Bound to {}", socket.get_ref().local_addr()?);
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn bind<A: ToString>(addr: A) -> io::Result<Async<UdpSocket>> {
        let addr = addr
            .to_string()
            .parse::<SocketAddr>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Ok(Async::new(UdpSocket::bind(addr)?)?)
    }

    /// Receives a single datagram message.
    ///
    /// Returns the number of bytes read and the address the message came from.
    ///
    /// This method must be called with a valid byte slice of sufficient size to hold the message.
    /// If the message is too long to fit, excess bytes may get discarded.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let (len, addr) = socket.recv_from(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.read_with(|io| io.recv_from(buf)).await
    }

    /// Receives a single datagram message without removing it from the queue.
    ///
    /// Returns the number of bytes read and the address the message came from.
    ///
    /// This method must be called with a valid byte slice of sufficient size to hold the message.
    /// If the message is too long to fit, excess bytes may get discarded.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let (len, addr) = socket.peek_from(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.read_with(|io| io.peek_from(buf)).await
    }

    /// Sends data to the specified address.
    ///
    /// Returns the number of bytes writen.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    ///
    /// let msg = b"hello";
    /// let addr = ([127, 0, 0, 1], 8000);
    /// let len = socket.send_to(msg, addr).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn send_to<A: Into<SocketAddr>>(&self, buf: &[u8], addr: A) -> io::Result<usize> {
        let addr = addr.into();
        self.write_with(|io| io.send_to(buf, addr)).await
    }

    /// Receives a single datagram message from the connected peer.
    ///
    /// Returns the number of bytes read.
    ///
    /// This method must be called with a valid byte slice of sufficient size to hold the message.
    /// If the message is too long to fit, excess bytes may get discarded.
    ///
    /// The [`connect`][`UdpSocket::connect()`] method connects this socket to a remote address.
    /// This method will fail if the socket is not connected.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    /// socket.get_ref().connect("127.0.0.1:8000")?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let len = socket.recv(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.write_with(|io| io.recv(buf)).await
    }

    /// Receives a single datagram message from the connected peer without removing it from the
    /// queue.
    ///
    /// Returns the number of bytes read and the address the message came from.
    ///
    /// This method must be called with a valid byte slice of sufficient size to hold the message.
    /// If the message is too long to fit, excess bytes may get discarded.
    ///
    /// The [`connect`][`UdpSocket::connect()`] method connects this socket to a remote address.
    /// This method will fail if the socket is not connected.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    /// socket.get_ref().connect("127.0.0.1:8000")?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let len = socket.peek(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|io| io.peek(buf)).await
    }

    /// Sends data to the connected peer.
    ///
    /// Returns the number of bytes written.
    ///
    /// The [`connect`][`UdpSocket::connect()`] method connects this socket to a remote address.
    /// This method will fail if the socket is not connected.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::net::UdpSocket;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
    /// socket.get_ref().connect("127.0.0.1:8000")?;
    ///
    /// let msg = b"hello";
    /// let len = socket.send(msg).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(|io| io.send(buf)).await
    }
}

#[cfg(unix)]
impl Async<UnixListener> {
    /// Creates a UDS listener bound to the specified path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<UnixListener>::bind("/tmp/socket")?;
    /// println!("Listening on {:?}", listener.get_ref().local_addr()?);
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixListener>> {
        let path = path.as_ref().to_owned();
        Ok(Async::new(UnixListener::bind(path)?)?)
    }

    /// Accepts a new incoming UDS stream connection.
    ///
    /// When a connection is established, it will be returned as a stream together with its remote
    /// address.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<UnixListener>::bind("/tmp/socket")?;
    /// let (stream, addr) = listener.accept().await?;
    /// println!("Accepted client: {:?}", addr);
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn accept(&self) -> io::Result<(Async<UnixStream>, UnixSocketAddr)> {
        let (stream, addr) = self.read_with(|io| io.accept()).await?;
        Ok((Async::new(stream)?, addr))
    }

    /// Returns a stream of incoming UDS connections.
    ///
    /// The stream is infinite, i.e. it never stops with a [`None`] item.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use futures::prelude::*;
    /// use smol::Async;
    /// use std::os::unix::net::UnixListener;
    ///
    /// # smol::run(async {
    /// let listener = Async::<UnixListener>::bind("127.0.0.1:80")?;
    /// let mut incoming = listener.incoming();
    ///
    /// while let Some(stream) = incoming.next().await {
    ///     let stream = stream?;
    ///     println!("Accepted client: {:?}", stream.get_ref().peer_addr()?);
    /// }
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn incoming(
        &self,
    ) -> impl Stream<Item = io::Result<Async<UnixStream>>> + Send + Unpin + '_ {
        Box::pin(stream::unfold(self, |listener| async move {
            let res = listener.accept().await.map(|(stream, _)| stream);
            Some((res, listener))
        }))
    }
}

#[cfg(unix)]
impl Async<UnixStream> {
    /// Creates a UDS stream connected to the specified path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixStream;
    ///
    /// # smol::run(async {
    /// let stream = Async::<UnixStream>::connect("/tmp/socket").await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixStream>> {
        // Create a socket.
        let socket = Socket::new(Domain::unix(), Type::stream(), None)?;

        // Begin async connect and ignore the inevitable "in progress" error.
        socket.set_nonblocking(true)?;
        socket
            .connect(&socket2::SockAddr::unix(path)?)
            .or_else(|err| {
                if err.raw_os_error() == Some(crate::sys::libc::EINPROGRESS) {
                    Ok(())
                } else {
                    Err(err)
                }
            })?;
        let stream = Async::new(socket.into_unix_stream())?;

        // Waits for connect to complete.
        let wait_connect = |mut stream: &UnixStream| match stream.write(&[]) {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => match stream.take_error()? {
                Some(err) => Err(err),
                None => Err(io::ErrorKind::WouldBlock.into()),
            },
            res => res.map(|_| ()),
        };

        // The stream becomes writable when connected.
        match stream.write_with(|io| wait_connect(io)).await {
            Ok(()) => Ok(stream),
            Err(err) => match stream.get_ref().take_error()? {
                Some(err) => Err(err),
                None => Err(err),
            },
        }
    }

    /// Creates an unnamed pair of connected UDS stream sockets.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixStream;
    ///
    /// # smol::run(async {
    /// let (stream1, stream2) = Async::<UnixStream>::pair()?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn pair() -> io::Result<(Async<UnixStream>, Async<UnixStream>)> {
        let (stream1, stream2) = UnixStream::pair()?;
        Ok((Async::new(stream1)?, Async::new(stream2)?))
    }
}

#[cfg(unix)]
impl Async<UnixDatagram> {
    /// Creates a UDS datagram socket bound to the specified path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UnixDatagram>::bind("/tmp/socket")?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Async<UnixDatagram>> {
        let path = path.as_ref().to_owned();
        Ok(Async::new(UnixDatagram::bind(path)?)?)
    }

    /// Creates a UDS datagram socket not bound to any address.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UnixDatagram>::unbound()?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn unbound() -> io::Result<Async<UnixDatagram>> {
        Ok(Async::new(UnixDatagram::unbound()?)?)
    }

    /// Creates an unnamed pair of connected Unix datagram sockets.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let (socket1, socket2) = Async::<UnixDatagram>::pair()?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub fn pair() -> io::Result<(Async<UnixDatagram>, Async<UnixDatagram>)> {
        let (socket1, socket2) = UnixDatagram::pair()?;
        Ok((Async::new(socket1)?, Async::new(socket2)?))
    }

    /// Receives data from the socket.
    ///
    /// Returns the number of bytes read and the address the message came from.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UnixDatagram>::bind("/tmp/socket")?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let (len, addr) = socket.recv_from(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, UnixSocketAddr)> {
        self.read_with(|io| io.recv_from(buf)).await
    }

    /// Sends data to the specified address.
    ///
    /// Returns the number of bytes written.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UnixDatagram>::unbound()?;
    ///
    /// let msg = b"hello";
    /// let addr = "/tmp/socket";
    /// let len = socket.send_to(msg, addr).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn send_to<P: AsRef<Path>>(&self, buf: &[u8], path: P) -> io::Result<usize> {
        self.write_with(|io| io.send_to(buf, &path)).await
    }

    /// Receives data from the connected peer.
    ///
    /// Returns the number of bytes read and the address the message came from.
    ///
    /// The [`connect`][`UnixDatagram::connect()`] method connects this socket to a remote address.
    /// This method will fail if the socket is not connected.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UnixDatagram>::bind("/tmp/socket1")?;
    /// socket.get_ref().connect("/tmp/socket2")?;
    ///
    /// let mut buf = [0u8; 1024];
    /// let len = socket.recv(&mut buf).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(|io| io.recv(buf)).await
    }

    /// Sends data to the connected peer.
    ///
    /// Returns the number of bytes written.
    ///
    /// The [`connect`][`UnixDatagram::connect()`] method connects this socket to a remote address.
    /// This method will fail if the socket is not connected.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use smol::Async;
    /// use std::os::unix::net::UnixDatagram;
    ///
    /// # smol::run(async {
    /// let socket = Async::<UnixDatagram>::bind("/tmp/socket1")?;
    /// socket.get_ref().connect("/tmp/socket2")?;
    ///
    /// let msg = b"hello";
    /// let len = socket.send(msg).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.write_with(|io| io.send(buf)).await
    }
}
