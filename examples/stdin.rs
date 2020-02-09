// use std::io::prelude::*;
// use std::process::{Command, Stdio};

use futures::io;
// use futures::prelude::*;

// fn stdin() -> impl AsyncBufRead + Unpin + 'static {
//     let mut child = Command::new("cat")
//         .stdin(Stdio::inherit())
//         .stdout(Stdio::piped())
//         .spawn()
//         .expect("Failed to execute command");
//     let mut out = child.stdout.take().unwrap();
//
//     let (reader, mut writer) = todo!("create a Reader/Writer");
//     Task::blocking(async move {
//         todo!("copy from out to the Writer");
//         // std::io::copy(&mut out, &mut writer);
//     })
//     .detach();
//
//     todo!("return a wrapped Reader that calls child.kill() on drop");
//     futures::empty()
// }

fn main() -> io::Result<()> {
    smol::run(async {
        // let stdin = stdin();
        //
        // let mut line = String::new();
        // stdin.read_line(&mut line).await?;

        Ok(())
    })
}
