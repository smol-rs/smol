// TODO: document
use std::env;
use std::process::{Command, Stdio};

use futures::io;
use futures::prelude::*;
use smol::blocking;

fn main() -> io::Result<()> {
    smol::run(async {
        let mut args = env::args().skip(1);
        let mut cmd = Command::new(args.next().expect("missing program name"));
        for arg in args {
            cmd.arg(arg);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let child_out = smol::reader(child.stdout.take().unwrap());
        let child_err = smol::reader(child.stderr.take().unwrap());

        let mut stdout = smol::writer(std::io::stdout());
        let mut stderr = smol::writer(std::io::stderr());

        future::try_join(
            io::copy(child_out, &mut stdout),
            io::copy(child_err, &mut stderr),
        )
        .await?;

        println!("{}", blocking!(child.wait())?);
        Ok(())
    })
}
