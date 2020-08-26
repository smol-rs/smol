//! Uses the `signal-hook` crate to catch the Ctrl-C signal.
//!
//! Run with:
//!
//! ```
//! cargo run --example unix-signal
//! ```

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;

    use smol::{prelude::*, Async};

    smol::block_on(async {
        // Create a Unix stream that receives a byte on each signal occurrence.
        let (a, mut b) = Async::<UnixStream>::pair()?;
        signal_hook::pipe::register(signal_hook::SIGINT, a)?;
        println!("Waiting for Ctrl-C...");

        // Receive a byte that indicates the Ctrl-C signal occurred.
        b.read_exact(&mut [0]).await?;

        println!("Done!");
        Ok(())
    })
}

#[cfg(not(unix))]
fn main() {
    println!("This example works only on Unix systems!");
}
