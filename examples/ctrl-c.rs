use futures::prelude::*;

fn main() {
    let (s, ctrl_c) = piper::chan(100);
    let handle = move || drop(s.send(()).now_or_never());
    ctrlc::set_handler(handle).unwrap();

    smol::run(async {
        println!("Waiting for Ctrl-C");
        ctrl_c.recv().await;
        println!("Done!");
    })
}
