#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{self, AtomicBool, Ordering};

use socket2::{Domain, Socket, Type};

#[derive(Debug)]
pub struct IoFlag {
    flag: AtomicBool,
    socket_notify: Socket,
    socket_wakeup: Socket,
}

impl IoFlag {
    pub fn create() -> io::Result<IoFlag> {
        // https://stackoverflow.com/questions/24933411/how-to-emulate-socket-socketpair-on-windows
        // https://github.com/mhils/backports.socketpair/blob/master/backports/socketpair/__init__.py
        // https://github.com/python-trio/trio/blob/master/trio/_core/_wakeup_socketpair.py
        // https://gist.github.com/geertj/4325783

        // Create a temporary listener.
        let listener = Socket::new(Domain::ipv4(), Type::stream(), None)?;
        listener.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
        listener.listen(1)?;
        let addr = listener.local_addr()?;

        // First socket: connect to the listener.
        let sock1 = Socket::new(Domain::ipv4(), Type::stream(), None)?;
        sock1.set_nonblocking(true)?;
        let _ = sock1.connect(&addr);
        let _ = sock1.set_nodelay(true);
        sock1.set_send_buffer_size(1)?;

        // Second socket: accept a client from the listener.
        let (sock2, _) = listener.accept()?;
        sock2.set_nonblocking(true)?;
        sock2.set_recv_buffer_size(1)?;

        Ok(IoFlag {
            flag: AtomicBool::new(false),
            socket_notify: sock1,
            socket_wakeup: sock2,
        })
    }

    pub fn set(&self) {
        atomic::fence(Ordering::SeqCst);

        if !self.flag.load(Ordering::SeqCst) {
            if !self.flag.swap(true, Ordering::SeqCst) {
                loop {
                    match (&self.socket_notify).write(&[1]) {
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        _ => break,
                    }
                }
                loop {
                    match (&self.socket_notify).flush() {
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        _ => break,
                    }
                }
            }
        }
    }

    pub fn get(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub fn clear(&self) -> bool {
        let value = self.flag.swap(false, Ordering::SeqCst);
        if value {
            loop {
                match (&self.socket_wakeup).read(&mut [0; 64]) {
                    Ok(n) if n > 0 => {}
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
        atomic::fence(Ordering::SeqCst);
        value
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for IoFlag {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.socket_wakeup.as_raw_fd()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawSocket for IoFlag {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        self.socket_wakeup.as_raw_socket()
    }
}
