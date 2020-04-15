use anyhow::Result;
use tokio::runtime::Runtime;

fn main() -> Result<()> {
    // Create a tokio runtime.
    let rt = Runtime::new().unwrap();

    // Set up the tokio context on the current thread.
    rt.enter(|| {
        // Run a smol executor on the current thread.
        smol::run(async {
            let body = reqwest::get("https://www.rust-lang.org")
                .await?
                .text()
                .await?;

            println!("{:?}", body);
            Ok(())
        })
    })
}
