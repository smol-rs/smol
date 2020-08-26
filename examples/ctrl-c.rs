//! Uses the `ctrlc` crate to catch the Ctrl-C signal.
//!
//! Run with:
//!
//! ```
//! cargo run --example ctrl-c
//! ```

fn main() {
    // Set a handler that sends a message through a channel.
    let (s, ctrl_c) = async_channel::bounded(100);
    let handle = move || {
        let _ = s.try_send(());
    };
    ctrlc::set_handler(handle).unwrap();

    smol::block_on(async {
        println!("Waiting for Ctrl-C...");

        // Receive a message that indicates the Ctrl-C signal occurred.
        let _ = ctrl_c.recv().await;

        println!("Done!");
    })
}
