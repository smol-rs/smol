use std::future::Future;
use std::io;
use std::net::{TcpListener, TcpStream, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use smol::{Async, Task, Timer};
#[cfg(unix)]
use tempfile::tempdir;

const LOREM_IPSUM: &[u8] = b"
Lorem ipsum dolor sit amet, consectetur adipiscing elit.
Donec pretium ante erat, vitae sodales mi varius quis.
Etiam vestibulum lorem vel urna tempor, eu fermentum odio aliquam.
Aliquam consequat urna vitae ipsum pulvinar, in blandit purus eleifend.
";

/// Runs future inside a local task.
///
/// The main future passed to `smol::run()` is sometimes polled even if it was not woken - e.g.
/// this can happen when the executor is waiting on the reactor and then wakes up for whatever
/// reason.
fn run<T: 'static>(future: impl Future<Output = T> + 'static) -> T {
    smol::run(async { Task::local(async { future.await }).await })
}

#[test]
fn tcp_connect() -> io::Result<()> {
    run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:0")?;
        let addr = listener.get_ref().local_addr()?;
        let task = Task::local(async move { listener.accept().await });

        let stream2 = Async::<TcpStream>::connect(&addr).await?;
        let stream1 = task.await?.0;

        assert_eq!(
            stream1.get_ref().peer_addr()?,
            stream2.get_ref().local_addr()?,
        );
        assert_eq!(
            stream2.get_ref().peer_addr()?,
            stream1.get_ref().local_addr()?,
        );

        // Now that the listener is closed, connect should fail.
        let err = Async::<TcpStream>::connect(&addr).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);

        Ok(())
    })
}

#[test]
fn tcp_peek_read() -> io::Result<()> {
    run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:0")?;
        let addr = listener.get_ref().local_addr()?;

        let mut stream = Async::<TcpStream>::connect(addr).await?;
        stream.write_all(LOREM_IPSUM).await?;

        let mut buf = [0; 1024];
        let mut incoming = listener.incoming();
        let mut stream = incoming.next().await.unwrap()?;

        let n = stream.peek(&mut buf).await?;
        assert_eq!(&buf[..n], LOREM_IPSUM);
        let n = stream.read(&mut buf).await?;
        assert_eq!(&buf[..n], LOREM_IPSUM);

        Ok(())
    })
}

#[test]
fn tcp_reader_hangup() -> io::Result<()> {
    run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:0")?;
        let addr = listener.get_ref().local_addr()?;
        let task = Task::local(async move { listener.accept().await });

        let mut stream2 = Async::<TcpStream>::connect(&addr).await?;
        let stream1 = task.await?.0;

        let task = Task::local(async move {
            Timer::after(Duration::from_secs(1)).await;
            drop(stream1);
        });

        while stream2.write_all(LOREM_IPSUM).await.is_ok() {}
        task.await;

        Ok(())
    })
}

#[test]
fn tcp_writer_hangup() -> io::Result<()> {
    run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:0")?;
        let addr = listener.get_ref().local_addr()?;
        let task = Task::local(async move { listener.accept().await });

        let mut stream2 = Async::<TcpStream>::connect(&addr).await?;
        let stream1 = task.await?.0;

        let task = Task::local(async move {
            Timer::after(Duration::from_secs(1)).await;
            drop(stream1);
        });

        let mut v = vec![];
        stream2.read_to_end(&mut v).await?;
        assert!(v.is_empty());

        task.await;
        Ok(())
    })
}

#[test]
fn udp_send_recv() -> io::Result<()> {
    run(async {
        let socket1 = Async::<UdpSocket>::bind("127.0.0.1:0")?;
        let socket2 = Async::<UdpSocket>::bind("127.0.0.1:0")?;
        socket1.get_ref().connect(socket2.get_ref().local_addr()?)?;

        let mut buf = [0u8; 1024];

        socket1.send(LOREM_IPSUM).await?;
        let n = socket2.peek(&mut buf).await?;
        assert_eq!(&buf[..n], LOREM_IPSUM);
        let n = socket2.recv(&mut buf).await?;
        assert_eq!(&buf[..n], LOREM_IPSUM);

        socket2
            .send_to(LOREM_IPSUM, socket1.get_ref().local_addr()?)
            .await?;
        let n = socket1.peek_from(&mut buf).await?.0;
        assert_eq!(&buf[..n], LOREM_IPSUM);
        let n = socket1.recv_from(&mut buf).await?.0;
        assert_eq!(&buf[..n], LOREM_IPSUM);

        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn udp_connect() -> io::Result<()> {
    run(async {
        let dir = tempdir()?;
        let path = dir.path().join("socket");

        let listener = Async::<UnixListener>::bind(&path)?;

        let mut stream = Async::<UnixStream>::connect(&path).await?;
        stream.write_all(LOREM_IPSUM).await?;

        let mut buf = [0; 1024];
        let mut incoming = listener.incoming();
        let mut stream = incoming.next().await.unwrap()?;

        let n = stream.read(&mut buf).await?;
        assert_eq!(&buf[..n], LOREM_IPSUM);

        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn uds_connect() -> io::Result<()> {
    run(async {
        let dir = tempdir()?;
        let path = dir.path().join("socket");
        let listener = Async::<UnixListener>::bind(&path)?;

        let addr = listener.get_ref().local_addr()?;
        let task = Task::local(async move { listener.accept().await });

        let stream2 = Async::<UnixStream>::connect(addr.as_pathname().unwrap()).await?;
        let stream1 = task.await?.0;

        assert_eq!(
            stream1.get_ref().peer_addr()?.as_pathname(),
            stream2.get_ref().local_addr()?.as_pathname(),
        );
        assert_eq!(
            stream2.get_ref().peer_addr()?.as_pathname(),
            stream1.get_ref().local_addr()?.as_pathname(),
        );

        // Now that the listener is closed, connect should fail.
        let err = Async::<UnixStream>::connect(addr.as_pathname().unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);

        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn uds_send_recv() -> io::Result<()> {
    run(async {
        let (socket1, socket2) = Async::<UnixDatagram>::pair()?;

        socket1.send(LOREM_IPSUM).await?;
        let mut buf = [0; 1024];
        let n = socket2.recv(&mut buf).await?;
        assert_eq!(&buf[..n], LOREM_IPSUM);

        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn uds_send_to_recv_from() -> io::Result<()> {
    run(async {
        let dir = tempdir()?;
        let path = dir.path().join("socket");
        let socket1 = Async::<UnixDatagram>::bind(&path)?;
        let socket2 = Async::<UnixDatagram>::unbound()?;

        socket2.send_to(LOREM_IPSUM, &path).await?;
        let mut buf = [0; 1024];
        let n = socket1.recv_from(&mut buf).await?.0;
        assert_eq!(&buf[..n], LOREM_IPSUM);

        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn uds_reader_hangup() -> io::Result<()> {
    run(async {
        let (socket1, mut socket2) = Async::<UnixStream>::pair()?;

        let task = Task::local(async move {
            Timer::after(Duration::from_secs(1)).await;
            drop(socket1);
        });

        while socket2.write_all(LOREM_IPSUM).await.is_ok() {}
        task.await;

        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn uds_writer_hangup() -> io::Result<()> {
    run(async {
        let (socket1, mut socket2) = Async::<UnixStream>::pair()?;

        let task = Task::local(async move {
            Timer::after(Duration::from_secs(1)).await;
            drop(socket1);
        });

        let mut v = vec![];
        socket2.read_to_end(&mut v).await?;
        assert!(v.is_empty());

        task.await;
        Ok(())
    })
}
