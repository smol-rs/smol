// TODO: document
#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    use std::io;
    use std::os::unix::io::AsRawFd;
    use std::time::{Duration, Instant};

    use smol::Async;
    use timerfd::{SetTimeFlags, TimerFd, TimerState};

    /// Converts a `nix::Error` into `std::io::Error`.
    fn io_err(err: nix::Error) -> io::Error {
        match err {
            nix::Error::Sys(code) => code.into(),
            err => io::Error::new(io::ErrorKind::Other, Box::new(err)),
        }
    }

    /// Sleeps using a `TimerFd`.
    async fn sleep(dur: Duration) -> io::Result<()> {
        // Create a timer.
        let mut timer = TimerFd::new()?;
        timer.set_state(TimerState::Oneshot(dur), SetTimeFlags::Default);

        // When the timer fires, a 64-bit integer can be read from it.
        Async::new(timer)?
            .with(|t| nix::unistd::read(t.as_raw_fd(), &mut [0u8; 8]).map_err(io_err))
            .await?;
        Ok(())
    }

    smol::run(async {
        let start = Instant::now();
        println!("Sleeping...");
        sleep(Duration::from_secs(1)).await?;
        println!("Woke up after {:?}", start.elapsed());
        Ok(())
    })
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("This example works only on Linux!");
}
