//! Uses the `signal-hook` crate to catch the Ctrl-C signal.
//!
//! Run with:
//!
//! ```
//! cargo run --example unix-signal
//! ```

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use smol::prelude::*;

    smol::block_on(async {
        // Create a signal stream for SIGINT.
        let mut signal = signal_hook_async_std::Signals::new(&[signal_hook::consts::SIGINT])?;
        println!("Waiting for Ctrl-C...");

        // Wait for the signal stream to return a signal.
        signal.next().await;

        println!("Done!");
        Ok(())
    })
}

#[cfg(not(unix))]
fn main() {
    println!("This example works only on Unix systems!");
}
