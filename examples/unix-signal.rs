#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;

    use futures::prelude::*;
    use smol::Async;

    smol::run(async {
        let (a, mut b) = Async::<UnixStream>::pair()?;
        signal_hook::pipe::register(signal_hook::SIGINT, a)?;

        println!("Waiting for Ctrl-C");
        b.read_exact(&mut [0]).await?;
        println!("Done!");

        Ok(())
    })
}

#[cfg(not(unix))]
fn main() {
    println!("This example works only on Unix systems!");
}
