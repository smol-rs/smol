#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::fs;
    use std::path::{Path, PathBuf};

    use futures::io;
    use futures::prelude::*;
    use smol::{blocking, Async, Task};
    use uds_windows::{UnixListener, UnixStream};

    async fn client(addr: PathBuf) -> io::Result<()> {
        // Connect accesses the filesystem, so it's a blocking operation.
        let stream = Async::new(blocking!(UnixStream::connect(addr))?)?;
        println!("Connected to {:?}", stream.get_ref().peer_addr()?);

        let mut stdout = smol::writer(std::io::stdout());
        io::copy(&stream, &mut stdout).await?;
        Ok(())
    }

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("socket");
    let _ = fs::remove_file(&path);

    smol::run(async {
        // Create a listener.
        let listener = Async::new(UnixListener::bind(&path)?)?;
        println!("Listening on {:?}", listener.get_ref().local_addr()?);

        // Spawn a client task.
        let task = Task::spawn(client(path.clone()));

        // Accept the client.
        let (stream, _) = listener.with(|l| l.accept()).await?;
        println!("Accepted a client");

        // Send a message, drop the stream, and wait for the client.
        Async::new(stream)?.write_all(b"Hello!\n").await?;
        task.await?;

        Ok(())
    })
}

#[cfg(not(windows))]
fn main() {
    println!("This example works only on Windows!");
}
