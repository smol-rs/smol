## Goals

* Small - Fits into a single source file.
* Fast - On par with async-std and Tokio.
* Safe - Written in 100% safe Rust.
* Complete - Fully featured and ready for production.
* Documented - Simple code, easy to understand and modify.
* Portable - Works on Linux, Android, macOS, iOS, Windows, FreeBSD, OpenBSD, NetBSD, DragonFly BSD, Solaris.
* Lightweight - Small dependencies, relies on epoll/kqueue/WSAPoll.

## Features

* Executor - Configurable threads, work stealing, supports non-send futures. (TODO: non-send futures)
* Blocking - Thread pool for isolating blocking code.
* Networking - TCP, UDP, Unix domain sockets, and custom files/sockets.
* Process - Spawns child processes and interacts with their I/O.
* Files - Filesystem manipulation operations. (TODO: some are not implemented yet)
* Stdio - Asynchronous stdin, stdout, and stderr. (TODO: not working yet) 
* Timer - Efficient userspace timers.

## Examples (TODO: turn these into links)

* Hello world
* HTTP request
* Hyper server
* Timer
* More...

## License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br/>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
