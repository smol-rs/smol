//! Uses the `ctrlc` crate to catch the Ctrl-C signal.
//!
//! Run with:
//!
//! ```
//! cargo run --example ctrl-c
//! ```

use smol::future;

fn main() {
    // Set a handler that sends a message through a channel.
    let (s, ctrl_c) = async_channel::bounded(100);
    let handle = move || {
        let _ = future::poll_once(s.send(()));
    };
    ctrlc::set_handler(handle).unwrap();

    smol::run(async {
        println!("Waiting for Ctrl-C...");

        // Receive a message that indicates the Ctrl-C signal occurred.
        let _ = ctrl_c.recv().await;

        println!("Done!");
    })
}
