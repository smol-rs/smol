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

A small and fast async runtime.

## Examples

Connect to an HTTP website, make a GET request, and pipe the response to the standard output:

```rust
use async_net::TcpStream;
use smol::{io, prelude::*, Unblock};

fn main() -> io::Result<()> {
    smol::run(async {
        let mut stream = TcpStream::connect("example.com:80").await?;
        let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n";
        stream.write_all(req).await?;

        let mut stdout = Unblock::new(std::io::stdout());
        io::copy(&stream, &mut stdout).await?;
        Ok(())
    })
}
```

This example uses [`async-net`] for networking, but you can also use the primitive `Async`
type. See the [full code][get-request].

Look inside the [examples] directory for more.

[`async-net`]: https://docs.rs/async-net
[examples]: https://github.com/stjepang/smol/tree/master/examples
[get-request]: https://github.com/stjepang/smol/blob/master/examples/get-request.rs

## Compatibility

All async libraries work with smol out of the box.

However, [tokio] is generally hostile towards non-tokio libraries, insists on non-standard I/O
traits, and deliberately lacks documentation on integration with the larger Rust ecosystem.
Fortunately, there are ways around it.

Enable the `tokio02` feature flag and `smol::run()` will create a minimal
tokio runtime for its libraries:

```toml
[dependencies]
smol = { version = "0.2", features = ["tokio02"] }
```

[tokio]: https://docs.rs/tokio

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
