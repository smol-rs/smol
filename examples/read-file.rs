//! Prints a file given as an argument to stdout.

use std::{env, fs, io};

fn main() -> io::Result<()> {
    let path = env::args().nth(1).expect("missing path argument");

    smol::run(async {
        let contents = smol::blocking!(fs::read_to_string(path))?;
        println!("{}", contents);
        Ok(())
    })
}
