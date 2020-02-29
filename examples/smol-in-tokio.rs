use std::thread;

use tokio::runtime::Handle;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() {
    // Grab a handle to the current tokio runtime.
    let handle = Handle::current();
    // Create a channel that gets closed when the smol executor stops.
    let (s, r) = oneshot::channel::<()>();

    // Spawn a new thread for a smol executor.
    thread::spawn(move || {
        // Set up the tokio context in the new thread.
        handle.enter(|| {
            // Run a smol executor.
            smol::run(async {
                println!("Hello world!");

                // Tokio now knows onto which runtime to spawn this future!
                let task = tokio::spawn(async { 1 + 2 });
                assert_eq!(task.await.unwrap(), 3);
            });

            // Drop the sender handle to close the channel.
            drop(s);
        })
    });

    // Wait until the sender is dropped, i.e. the smol executor stops.
    let _ = r.await;
}
