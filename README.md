# smol

[![Build](https://github.com/stjepang/smol/workflows/Build%20and%20test/badge.svg)](
https://github.com/stjepang/smol/actions)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](
https://github.com/stjepang/smol)
[![Cargo](https://img.shields.io/crates/v/smol.svg)](
https://crates.io/crates/smol)
[![Documentation](https://docs.rs/smol/badge.svg)](
https://docs.rs/smol)
[![Chat](https://img.shields.io/discord/701824908866617385.svg?logo=discord)](
https://discord.gg/x6m5Vvt)

A small and fast async runtime for Rust.

This runtime extends [the standard library][std] with async combinators
and is only 1500 lines of code long.

[std]: https://docs.rs/std

Reading the [docs] or looking at the [examples] is a great way to start learning
async Rust.

[docs]: https://docs.rs/smol
[examples]: ./examples

Async I/O is implemented using [epoll] on Linux/Android, [kqueue] on
macOS/iOS/BSD, and [wepoll] on Windows.

What makes smol different from [async-std] and [tokio]? 
Read this [blog post](https://stjepang.github.io/2020/04/03/why-im-building-a-new-async-runtime.html).

[epoll]: https://en.wikipedia.org/wiki/Epoll
[kqueue]: https://en.wikipedia.org/wiki/Kqueue
[wepoll]: https://github.com/piscisaureus/wepoll

## Features

* Async TCP, UDP, Unix domain sockets, and custom file descriptors.
* Thread-local executor for `!Send` futures.
* Work-stealing executor that adapts to uneven workloads.
* Blocking executor for files, processes, and standard I/O.
* Tasks that support cancellation.
* Userspace timers.

## Examples

You need to be in the [examples] directory to run them:

```terminal
$ cd examples
$ ls
$ cargo run --example ctrl-c
```

## Compatibility

See [this example](./examples/other-runtimes.rs) for how to use smol with
[async-std], [tokio], [surf], and [reqwest].

There is an optional feature for seamless integration with crates depending
on tokio. It creates a global tokio runtime and sets up its context inside smol.
Enable the feature as follows:

```toml
[dependencies]
smol = { version = "0.1", features = ["tokio02"] }
```

[async-std]: https://docs.rs/async-std
[tokio]: https://docs.rs/tokio
[surf]: https://docs.rs/surf
[reqwest]: https://docs.rs/reqwest

## Documentation

You can read the docs [here][docs], or generate them on your own.

If you'd like to explore the implementation in more depth, the following
command generates docs for the whole crate, including private modules:

```
cargo doc --document-private-items --no-deps --open
```

[docs]: https://docs.rs/smol

## Other crates

My personal crate recommendation list:

* Channels, pipes, and mutexes: [piper]
* HTTP clients: [surf], [isahc], [reqwest]
* HTTP servers: [async-h1], [hyper]
* WebSockets: [async-tungstenite]
* TLS authentication: [async-native-tls]
* Signals: [ctrlc], [signal-hook]

[piper]: https://docs.rs/piper
[surf]: https://docs.rs/surf
[isahc]: https://docs.rs/isahc
[reqwest]: https://docs.rs/reqwest
[async-h1]: https://docs.rs/async-h1
[hyper]: https://docs.rs/hyper
[async-tungstenite]: https://docs.rs/async-tungstenite
[async-native-tls]: https://docs.rs/async-native-tls
[native-tls]: https://docs.rs/native-tls
[ctrlc]: https://docs.rs/ctrlc
[signal-hook]: https://docs.rs/signal-hook

## TLS certificate

Some code examples are using TLS for authentication. The repository
contains a self-signed certificate usable for testing, but it should **not**
be used for real-world scenarios. Browsers and tools like curl will
show this certificate as insecure.

In browsers, accept the security prompt or use `curl -k` on the
command line to bypass security warnings.

The certificate file was generated using
[minica](https://github.com/jsha/minica) and
[openssl](https://www.openssl.org/):

```
minica --domains localhost -ip-addresses 127.0.0.1 -ca-cert certificate.pem
openssl pkcs12 -export -out identity.pfx -inkey localhost/key.pem -in localhost/cert.pem
```

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

#### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
