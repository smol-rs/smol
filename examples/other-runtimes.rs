//! Demonstrates how to use `async-std`, `tokio`, `surf`, and `request`.
//!
//! For compatibility with tokio-based libraries, enable the `tokio02` feature flag:
//!
//! ```toml
//! [dependencies]
//! smol = { version = "0.2", features = ["tokio02"] }
//! ```
//!
//! Run with:
//!
//! ```
//! cargo run --example other-runtimes
//! ```

use std::time::{Duration, Instant};

use anyhow::{Error, Result};

fn main() -> Result<()> {
    smol::run(async {
        // Sleep using async-std.
        let start = Instant::now();
        println!("Sleeping using async-std...");
        async_std::task::sleep(Duration::from_secs(1)).await;
        println!("Woke up after {:?}", start.elapsed());

        // Sleep using tokio (the `tokio02` feature must be enabled).
        let start = Instant::now();
        println!("Sleeping using tokio...");
        tokio::time::delay_for(Duration::from_secs(1)).await;
        println!("Woke up after {:?}", start.elapsed());

        // Make a GET request using surf.
        let body = surf::get("https://www.rust-lang.org")
            .recv_string()
            .await
            .map_err(Error::msg)?;
        println!("Body from surf: {:?}", body);

        // Make a GET request using reqwest (the `tokio02` feature must be enabled).
        let resp = reqwest::get("https://www.rust-lang.org").await?;
        let body = resp.text().await?;
        println!("Body from reqwest: {:?}", body);

        Ok(())
    })
}
