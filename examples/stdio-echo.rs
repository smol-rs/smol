use std::process::{Command, Stdio};

use futures::io;
use futures::prelude::*;
use smol::Task;

fn stdin() -> impl AsyncRead + Unpin + 'static {
    let mut child = Command::new("cat")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to execute command");
    let mut out = child.stdout.take().unwrap();

    let (reader, mut writer) = piper::pipe(2); // TODO
    Task::blocking(async move {
        io::copy(io::AllowStdIo::new(out), &mut writer).await;
    })
    .forget();

    // TODO todo!("return a wrapped Reader that calls child.kill() on drop");
    reader
}

// TODO: just pipe stdin to stdout

fn main() -> io::Result<()> {
    smol::run(async {
        let mut stdin = io::BufReader::new(stdin());

        let mut line = String::new();
        stdin.read_line(&mut line).await?;
        dbg!(line);

        Ok(())
    })
}
