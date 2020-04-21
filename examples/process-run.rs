// TODO: document
use std::env;
use std::process::Command;

use futures::io;

fn main() -> io::Result<()> {
    smol::run(async {
        let mut args = env::args().skip(1);
        let mut cmd = Command::new(args.next().expect("missing program name"));
        for arg in args {
            cmd.arg(arg);
        }

        println!("{}", smol::blocking!(cmd.output())?.status);
        Ok(())
    })
}
