use futures::io;

fn main() -> io::Result<()> {
    smol::run(async {
        let stdin = smol::reader(std::io::stdin());
        let mut stdout = smol::writer(std::io::stdout());

        io::copy(stdin, &mut stdout).await?;
        Ok(())
    })
}
