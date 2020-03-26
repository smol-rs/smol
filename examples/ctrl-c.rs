use once_cell::sync::Lazy;
use piper::Receiver;
use signal_hook::iterator::Signals;

async fn ctrl_c() {
    static RECEIVER: Lazy<Receiver<i32>> = Lazy::new(|| {
        let signals = Signals::new(&[signal_hook::SIGINT]).unwrap();
        smol::iter(100, Box::leak(Box::new(signals)).forever())
    });
    RECEIVER.recv().await;
}

fn main() {
    smol::run(async {
        println!("Waiting for Ctrl-C");
        ctrl_c().await;
        println!("Done!");
    })
}
