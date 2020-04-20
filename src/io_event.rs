use std::io::{self, Read, Write};
use std::sync::atomic::{self, AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, FromRawSocket, RawSocket};

use socket2::{Domain, Socket, Type};

use crate::async_io::Async;

/// A boolean flag that is set whenever a thread-local task is woken by another thread.
///
/// Every time this flag's value is changed, an I/O event is triggered.
///
/// TODO
#[derive(Clone)]
pub(crate) struct IoEvent {
    pipe: Arc<SelfPipe>,
}

/// TODO
impl IoEvent {
    pub fn new() -> io::Result<IoEvent> {
        Ok(IoEvent {
            pipe: Arc::new(SelfPipe::new()?),
        })
    }

    pub fn set(&self) {
        self.pipe.set();
    }

    pub fn clear(&self) -> bool {
        self.pipe.clear()
    }

    pub async fn ready(&self) {
        self.pipe.ready().await;
    }
}

/// A boolean flag that triggers I/O events whenever changed.
///
/// https://cr.yp.to/docs/selfpipe.html
///
/// TODO
struct SelfPipe {
    flag: AtomicBool,
    writer: Socket,
    reader: Async<Socket>,
}

/// TODO
impl SelfPipe {
    /// Creates a self-pipe.
    fn new() -> io::Result<SelfPipe> {
        let (writer, reader) = pipe()?;
        writer.set_send_buffer_size(1)?;
        reader.set_recv_buffer_size(1)?;
        Ok(SelfPipe {
            flag: AtomicBool::new(false),
            writer,
            reader: Async::new(reader)?,
        })
    }

    /// Sets the flag to `true`.
    // TODO: rename to raise() as in "raise a signal"? or even better: emit() or notify()
    fn set(&self) {
        // Publish all in-memory changes before setting the flag.
        atomic::fence(Ordering::SeqCst);

        // If the flag is not set...
        if !self.flag.load(Ordering::SeqCst) {
            // If this thread sets it...
            if !self.flag.swap(true, Ordering::SeqCst) {
                // Trigger an I/O event by writing a byte into the sending socket.
                let _ = (&self.writer).write(&[1]);
                let _ = (&self.writer).flush();
            }
        }
    }

    /// Sets the flag to `false`.
    fn clear(&self) -> bool {
        // Read all available bytes from the receiving socket.
        while self.reader.get_ref().read(&mut [0; 64]).is_ok() {}
        let value = self.flag.swap(false, Ordering::SeqCst);

        // Publish all in-memory changes after clearing the flag.
        atomic::fence(Ordering::SeqCst);
        value
    }

    /// Waits until the flag is changed.
    ///
    /// Note that this method may spuriously report changes when they didn't really happen.
    async fn ready(&self) {
        self.reader
            .with(|_| match self.flag.load(Ordering::SeqCst) {
                true => Ok(()),
                false => Err(io::Error::new(io::ErrorKind::WouldBlock, "")),
            })
            .await
            .expect("failure while waiting on a self-pipe");
    }
}

/// TODO
#[cfg(unix)]
fn pipe() -> io::Result<(Socket, Socket)> {
    let (sock1, sock2) = Socket::pair(Domain::unix(), Type::stream(), None)?;
    sock1.set_nonblocking(true)?;
    sock2.set_nonblocking(true)?;
    Ok((sock1, sock2))
}

/// TODO
#[cfg(windows)]
fn pipe() -> io::Result<(Socket, Socket)> {
    // TODO The only portable way of manually triggering I/O events is to create a socket and
    // send/receive dummy data on it. This pattern is also known as "the self-pipe trick".
    // See the links below for more information.
    //
    // https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
    // https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
    // https://gist.github.com/geertj/4325783

    // Create a temporary listener.
    let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    listener.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
    listener.listen(1)?;

    // First socket: start connecting to the listener.
    let sock1 = Socket::new(Domain::ipv4(), Type::stream(), None)?;
    sock1.set_nonblocking(true)?;
    let _ = sock1.set_nodelay(true)?;
    let _ = sock1.connect(&listener.local_addr()?);

    // Second socket: accept a connection from the listener.
    let (sock2, _) = listener.accept()?;
    sock2.set_nonblocking(true)?;
    let _ = sock2.set_nodelay(true)?;

    Ok((sock1, sock2))
}
