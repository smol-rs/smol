use once_cell::sync::Lazy;
use piper::Receiver;
use signal_hook::iterator::Signals;

static CTRL_C: Lazy<Receiver<i32>> = Lazy::new(|| {
    let signals = Signals::new(&[signal_hook::SIGINT]).unwrap();
    smol::iter(100, Box::leak(Box::new(signals)).forever())
});

fn main() {
    smol::run(async {
        println!("Waiting for Ctrl-C");
        CTRL_C.recv().await;
        println!("Done!");
    })
}
