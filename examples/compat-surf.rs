fn main() -> http_types::Result<()> {
    smol::run(async {
        let body = surf::get("https://www.rust-lang.org").recv_string().await?;
        println!("{:?}", body);
        Ok(())
    })
}
