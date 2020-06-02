//! Uses the `ctrlc` crate to catch the Ctrl-C signal.
//!
//! Run with:
//!
//! ```
//! cd examples  # make sure to be in this directory
//! cargo run --example ctrl-c
//! ```

use futures::prelude::*;

fn main() {
    // Set a handler that sends a message through a channel.
    let (s, ctrl_c) = piper::chan(100);
    let handle = move || {
        let _ = s.send(()).now_or_never();
    };
    ctrlc::set_handler(handle).unwrap();

    smol::run(async {
        println!("Waiting for Ctrl-C...");

        // Receive a message that indicates the Ctrl-C signal occured.
        ctrl_c.recv().await;

        println!("Done!");
    })
}
