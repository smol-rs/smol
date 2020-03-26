use std::env;
use std::io;
use std::io::prelude::*;
use std::process::Command;

fn main() -> io::Result<()> {
    smol::run(async {
        let mut args = env::args().skip(1);
        let mut cmd = Command::new(args.next().expect("missing program name"));
        for arg in args {
            cmd.arg(arg);
        }
        let out = smol::blocking!(cmd.output())?;

        println!("{}", out.status);
        io::stdout().write_all(&out.stdout)?;
        io::stdout().write_all(&out.stderr)?;
        Ok(())
    })
}
