#[cfg(unix)]
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::{
    io,
    net::{Shutdown, TcpListener, TcpStream, UdpSocket},
    sync::Arc,
    time::Duration,
};

use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use smol::{Async, Task};
#[cfg(unix)]
use tempfile::tempdir;

const LOREM_IPSUM: &[u8] = b"
Lorem ipsum dolor sit amet, consectetur adipiscing elit.
Donec pretium ante erat, vitae sodales mi varius quis.
Etiam vestibulum lorem vel urna tempor, eu fermentum odio aliquam.
Aliquam consequat urna vitae ipsum pulvinar, in blandit purus eleifend.
";

#[test]
fn tcp_connect() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:12300")?;
        let addr = listener.get_ref().local_addr()?;
        let task = Task::spawn(async move { listener.accept().await });

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
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:12301")?;

        let mut stream = Async::<TcpStream>::connect("127.0.0.1:12301").await?;
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
fn udp_send_recv() -> io::Result<()> {
    smol::run(async {
        let socket1 = Async::<UdpSocket>::bind("127.0.0.1:12302")?;
        let socket2 = Async::<UdpSocket>::bind("127.0.0.1:12303")?;
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
    smol::run(async {
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
    smol::run(async {
        let dir = tempdir()?;
        let path = dir.path().join("socket");
        let listener = Async::<UnixListener>::bind(&path)?;

        let addr = listener.get_ref().local_addr()?;
        let task = Task::spawn(async move { listener.accept().await });

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
    smol::run(async {
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
    smol::run(async {
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

// Test that we correctly re-register interests when we are previously
// interested in both readable and writable events and then we get only one of
// them. (we need to re-register interest on the other.)
#[test]
fn tcp_duplex() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:0")?;
        let stream0 =
            Arc::new(Async::<TcpStream>::connect(listener.get_ref().local_addr()?).await?);
        let stream1 = Arc::new(listener.accept().await?.0);

        async fn do_read(s: Arc<Async<TcpStream>>) -> io::Result<()> {
            let mut buf = vec![0u8; 4096];
            loop {
                let len = (&*s).read(&mut buf).await?;
                if len == 0 {
                    return Ok(());
                }
            }
        }

        async fn do_write(s: Arc<Async<TcpStream>>) -> io::Result<()> {
            let buf = vec![0u8; 4096];
            for _ in 0..4096 {
                (&*s).write_all(&buf).await?;
            }
            s.get_ref().shutdown(Shutdown::Write)?;
            Ok(())
        }

        // Read from and write to stream0.
        let r0 = Task::spawn(do_read(stream0.clone()));
        let w0 = Task::spawn(do_write(stream0));

        // Sleep a bit, so that reading and writing are both blocked.
        smol::Timer::after(Duration::from_millis(5)).await;

        // Start reading stream1, make stream0 writable.
        let r1 = Task::spawn(do_read(stream1.clone()));

        // Finish writing to stream0.
        w0.await?;
        r1.await?;

        // Start writing to stream1, make stream0 readable.
        let w1 = Task::spawn(do_write(stream1));

        // Will r0 be correctly woken?
        r0.await?;
        w1.await?;

        Ok(())
    })
}
