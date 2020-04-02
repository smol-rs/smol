#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    use std::ffi::OsString;
    use std::io;

    use inotify::{EventMask, Inotify, WatchMask};
    use smol::Async;

    type Event = (OsString, EventMask);

    /// Reads some events without blocking.
    fn try_read(inotify: &mut Inotify) -> io::Result<Vec<Event>> {
        let mut buffer = [0; 1024];
        let events = inotify
            .read_events(&mut buffer)?
            .filter_map(|ev| ev.name.map(|name| (name.to_owned(), ev.mask)))
            .collect::<Vec<_>>();

        if events.is_empty() {
            Err(io::Error::new(io::ErrorKind::WouldBlock, ""))
        } else {
            Ok(events)
        }
    }

    smol::run(async {
        // Watch events in the current directory.
        let mut inotify = Async::new(Inotify::init()?)?;
        inotify.get_mut().add_watch(".", WatchMask::ALL_EVENTS)?;
        println!("Watching for filesystem events in the current directory...");

        loop {
            for event in inotify.read_with_mut(try_read).await? {
                println!("{:?}", event);
            }
        }
    })
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("This example works only on Linux!");
}
