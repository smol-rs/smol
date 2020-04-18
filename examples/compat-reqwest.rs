use anyhow::Result;

fn main() -> Result<()> {
    smol::run(async {
        let resp = reqwest::get("https://www.rust-lang.org").await?;
        let body = resp.text().await?;

        println!("{:?}", body);
        Ok(())
    })
}
