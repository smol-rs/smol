# wepoll-sys

wepoll-sys provides Rust bindings to [wepoll][wepoll], generated using
[bindgen][bindgen]. The wepoll library is included in this crate and compiled
automatically, removing the need for manually installing it.

## Requirements

* Rust 2018
* Windows
* clang
* A compiler such as gcc, the MSVC compiler (`cl.exe`), etc

## Usage

Add wepoll-sys as a Windows dependency (since it won't build on other
platforms):

```toml
[dependencies.'cfg(windows)'.dependencies]
wepoll-sys = "2.0"
```

Since this crate just provides a generated wrapper around the wepoll library,
usage is the same as with the C code. For example:

```rust
use wepoll_sys;

fn main() {
    let wepoll = wepoll_sys::epoll_create(1);

    if wepoll.is_null() {
        panic!("epoll_create(1) failed!");
    }

    // ...
}
```

[wepoll]: https://github.com/piscisaureus/wepoll
[bindgen]: https://rust-lang.github.io/rust-bindgen/
