//! Async I/O.

use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[cfg(unix)]
use std::{
    os::unix::io::AsRawFd,
    os::unix::net::{SocketAddr as UnixSocketAddr, UnixDatagram, UnixListener, UnixStream},
    path::Path,
};

#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, FromRawSocket, RawSocket};

use futures::future;
use futures::io::{AsyncRead, AsyncWrite};
use futures::stream::{self, Stream};
use socket2::{Domain, Protocol, Socket, Type};

use crate::reactor::{Reactor, Source};
use crate::task::Task;

/// Async I/O.
///
/// TODO
/// TODO: suggest using Shared to split
/// TODO: suggest using Lock if Async<T> is not splittable
#[derive(Debug)]
pub struct Async<T> {
    /// A source registered in the reactor.
    source: Arc<Source>,

    /// The inner I/O handle.
    io: Option<Box<T>>,
}

#[cfg(unix)]
impl<T: AsRawFd> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    ///
    /// TODO: explain AsRawFd and AsRawSocket
    /// TODO: **warning** for unix users: the I/O handle must be compatible with epoll/kqueue!
    /// Most notably, `File`, `Stdin`, `Stdout`, `Stderr` will **not** work.
    pub fn new(io: T) -> io::Result<Async<T>> {
        Ok(Async {
            source: Reactor::get().insert_io(io.as_raw_fd())?,
            io: Some(Box::new(io)),
        })
    }
}

#[cfg(windows)]
impl<T: AsRawSocket> Async<T> {
    /// Converts a non-blocking I/O handle into an async I/O handle.
    ///
    /// TODO
    pub fn new(io: T) -> io::Result<Async<T>> {
        Ok(Async {
            source: Reactor::get().insert_io(io.as_raw_socket())?,
            io: Some(Box::new(io)),
        })
    }
}

impl<T> Async<T> {
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

    /// TODO
    pub async fn with<R>(&self, op: impl FnMut(&T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let mut io = self.io.as_ref().unwrap();
        let source = &self.source;
        future::poll_fn(|cx| source.poll_io(cx, || op(&mut io))).await
    }

    /// TODO
    pub async fn with_mut<R>(&mut self, op: impl FnMut(&mut T) -> io::Result<R>) -> io::Result<R> {
        let mut op = op;
        let mut io = self.io.as_mut().unwrap();
        let source = &self.source;
        future::poll_fn(|cx| source.poll_io(cx, || op(&mut io))).await
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
    futures::pin_mut!(fut);
    fut.poll(cx)
}

impl<T: Read> AsyncRead for Async<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with_mut(|io| io.read(buf)))
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
        poll_future(cx, self.with(|io| (&*io).read(buf)))
    }
}

impl<T: Write> AsyncWrite for Async<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_future(cx, self.with_mut(|io| io.write(buf)))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.with_mut(|io| io.flush()))
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
        poll_future(cx, self.with(|io| (&*io).write(buf)))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        poll_future(cx, self.with(|io| (&*io).flush()))
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
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Async<TcpListener>> {
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
        let (stream, addr) = self.with(|io| io.accept()).await?;
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

        // Begin async connect and ignore the inevitable "not yet connected" error.
        socket.set_nonblocking(true)?;
        let _ = socket.connect(&addr.into());
        let stream = Async::new(socket.into_tcp_stream())?;

        // Waits until the stream becomes writable.
        let wait_writable = |mut stream: &TcpStream| match stream.write(&[]) {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
            }
            res => res.map(|_| ()),
        };
        // The stream becomes writable when connected.
        stream.with(|io| wait_writable(io)).await?;

        Ok(stream)
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
        self.with(|io| io.peek(buf)).await
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
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
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
        self.with(|io| io.recv_from(buf)).await
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
        self.with(|io| io.peek_from(buf)).await
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
        self.with(|io| io.send_to(buf, addr)).await
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
        self.with(|io| io.recv(buf)).await
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
        self.with(|io| io.peek(buf)).await
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
        self.with(|io| io.send(buf)).await
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
        let (stream, addr) = self.with(|io| io.accept()).await?;
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

        // Begin async connect and ignore the inevitable "not yet connected" error.
        socket.set_nonblocking(true)?;
        let _ = socket.connect(&socket2::SockAddr::unix(path)?);
        let stream = Async::new(socket.into_unix_stream())?;

        // Waits until the stream becomes writable.
        let wait_writable = |mut stream: &UnixStream| match stream.write(&[]) {
            Err(err) if err.kind() == io::ErrorKind::NotConnected => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
            }
            res => res.map(|_| ()),
        };
        // The stream becomes writable when connected.
        stream.with(|io| wait_writable(io)).await?;

        Ok(stream)
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
        self.with(|io| io.recv_from(buf)).await
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
        self.with(|io| io.send_to(buf, &path)).await
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
        self.with(|io| io.recv(buf)).await
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
        self.with(|io| io.send(buf)).await
    }
}
