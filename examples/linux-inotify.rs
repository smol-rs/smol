//! Uses the `inotify` crate to watch for changes in the current directory.
//!
//! Run with:
//!
//! ```
//! cd examples  # make sure to be in this directory
//! cargo run --example linux-inotify
//! ```

#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    use std::ffi::OsString;
    use std::io;

    use inotify::{EventMask, Inotify, WatchMask};
    use smol::Async;

    type Event = (OsString, EventMask);

    /// Reads some events without blocking.
    ///
    /// If there are no events, an [`io::ErrorKind::WouldBlock`] error is returned.
    fn read_op(inotify: &mut Inotify) -> io::Result<Vec<Event>> {
        let mut buffer = [0; 1024];
        let events = inotify
            .read_events(&mut buffer)?
            .filter_map(|ev| ev.name.map(|name| (name.to_owned(), ev.mask)))
            .collect::<Vec<_>>();

        if events.is_empty() {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(events)
        }
    }

    smol::run(async {
        // Watch events in the current directory.
        let mut inotify = Async::new(Inotify::init()?)?;
        inotify.get_mut().add_watch(".", WatchMask::ALL_EVENTS)?;
        println!("Watching for filesystem events in the current directory...");
        println!("Try opening a file to trigger some events.");
        println!();

        // Wait for events in a loop and print them on the screen.
        loop {
            for event in inotify.with_mut(read_op).await? {
                println!("{:?}", event);
            }
        }
    })
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("This example works only on Linux!");
}
