//! Uses the `ctrlc` crate to catch the Ctrl-C signal.
//!
//! Run with:
//!
//! ```
//! cargo run --example ctrl-c
//! ```

use blocking::block_on;
use futures_lite::*;

fn main() {
    // Set a handler that sends a message through a channel.
    let (s, ctrl_c) = async_channel::bounded(100);
    let handle = move || {
        let _ = future::poll_once(s.send(()));
    };
    ctrlc::set_handler(handle).unwrap();

    block_on(async {
        println!("Waiting for Ctrl-C...");

        // Receive a message that indicates the Ctrl-C signal occurred.
        let _ = ctrl_c.recv().await;

        println!("Done!");
    })
}
