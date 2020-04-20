# smol

A small and fast async runtime.

https://discord.gg/5RxMVnr

## Goals

* Small - Fits into a single source file.
* Fast - On par with async-std and Tokio.
* Safe - Written in 100% safe Rust.
* Complete - Fully featured and ready for production.
* Documented - Simple code, easy to understand and modify.
* Lightweight - Small dependencies, relies on epoll/kqueue/wepoll.
* Portable - Linux, Android, macOS, iOS, Windows, FreeBSD, OpenBSD, NetBSD, DragonFly BSD.

## Features

* Executor - Configurable threads, work stealing, supports non-send futures.
* Blocking - Thread pool for isolating blocking code.
* Networking - TCP, UDP, Unix domain sockets, and custom files/sockets.
* Process - Spawns child processes and interacts with their I/O.
* Files - Filesystem manipulation operations.
* Stdio - Asynchronous stdin, stdout, and stderr.
* Timer - Efficient userspace timers.

# Documentation

```
cargo doc --document-private-items --no-deps --open
```

# TODO Crate recommendations

# TODO certificate and private key
```
// To access the HTTPS version, import the certificate into Chrome/Firefox:
// 1. Open settings and go to the certificate 'Authorities' list
// 2. Click 'Import' and select certificate.pem
// 3. Enable 'Trust this CA to identify websites' and click OK
// 4. Restart the browser and go to https://127.0.0.1:8001
//
// The certificate was generated using minica and openssl:
// 1. minica --domains localhost -ip-addresses 127.0.0.1 -ca-cert certificate.pem
// 2. openssl pkcs12 -export -out identity.pfx -inkey localhost/key.pem -in localhost/cert.pem
```

## Examples

* [Async-h1 client](./examples/async-h1-client.rs)
* [Async-h1 server](./examples/async-h1-server.rs)
* [Chat Client](./examples/chat-client.rs)
* [Chat Server](./examples/chat-server.rs)
* [Compat reqwest](./examples/compat-reqwest.rs)
* [Compat tokio](./examples/compat-tokio.rs)
* [Ctrl-c](./examples/ctrl-c.rs)
* [Hyper Client](./examples/hyper-client.rs)
* [Hyper Server](./examples/hyper-server.rs)
* [Linux Inotify](./examples/linux-inotify.rs)
* [Linux Timefd](./examples/linux-timerfd.rs)
* [Process Output](./examples/process-output.rs)
* [Process Run](./examples/process-run.rs)
* [Read Dir](./examples/read-dir.rs)
* [Read File](./examples/read-file.rs)
* [Simple Client](./examples/simple-client.rs)
* [Simple Server](./examples/simple-server.rs)
* [Stdin to Stdout](./examples/stdin-to-stdout.rs)
* [Tcp Client](./examples/tcp-client.rs)
* [Tcp Server](./examples/tcp-server.rs)
* [Thread Pool](./examples/thread-pool.rs)
* [Timer Sleep](./examples/timer-sleep.rs)
* [Timer Timeout](./examples/timer-timeout.rs)
* [TLS Client](./examples/tls-client.rs)
* [TLS Server](./examples/tls-server.rs)
* [Unix Signal](./examples/unix-signal.rs)
* [Web Crawler](./examples/web-crawler.rs)
* [Websocket Client](./examples/websocket-client.rs)
* [WebSocket Server](./examples/websocket-server.rs)
* [Windows Uds](./examples/windows-uds.rs)

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
