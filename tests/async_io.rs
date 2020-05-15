use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use smol::{Async, Task};
#[cfg(unix)]
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::{
    io,
    net::{TcpListener, TcpStream, UdpSocket},
};
#[cfg(unix)]
use tempfile::tempdir;

const LOREM_IPSUM: &[u8] = b"
Lorem ipsum dolor sit amet, consectetur adipiscing elit.
Donec pretium ante erat, vitae sodales mi varius quis.
Etiam vestibulum lorem vel urna tempor, eu fermentum odio aliquam.
Aliquam consequat urna vitae ipsum pulvinar, in blandit purus eleifend.
";

#[test]
fn tcp_connection() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:8080")?;
        let addr = listener.get_ref().local_addr()?;
        let task = Task::spawn(async move { listener.accept().await });

        let stream2 = Async::<TcpStream>::connect(&addr).await?;
        let stream1 = task.await?.0;

        assert_eq!(
            stream1.get_ref().peer_addr()?,
            stream2.get_ref().local_addr()?
        );
        assert_eq!(
            stream2.get_ref().peer_addr()?,
            stream1.get_ref().local_addr()?
        );

        // Now that the listener is closed, connect should fail.
        Async::<TcpStream>::connect(&addr).await.unwrap_err();

        Ok(())
    })
}

#[test]
fn tcp_peek_read() -> io::Result<()> {
    smol::run(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:8081")?;

        let mut stream = Async::<TcpStream>::connect("127.0.0.1:8081").await?;
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
        let socket1 = Async::<UdpSocket>::bind("127.0.0.1:8000")?;
        let socket2 = Async::<UdpSocket>::bind("127.0.0.1:9000")?;
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
fn uds_connection() -> io::Result<()> {
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
