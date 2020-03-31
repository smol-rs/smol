//! Prints a file given as an argument to stdout.

use std::fs::File;
use std::io;
use std::env;

fn main() -> io::Result<()> {
    let path = env::args().nth(1).expect("missing path argument");

    smol::run(async {
        let file = smol::reader(File::open(path)?);
        let mut stdout = smol::writer(io::stdout());

        futures::io::copy(file, &mut stdout).await?;
        Ok(())
    })
}
