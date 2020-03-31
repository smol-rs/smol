use once_cell::sync::Lazy;
use piper::Receiver;
use signal_hook::iterator::Signals;
use smol::Task;

static CTRL_C: Lazy<Receiver<()>> = Lazy::new(|| {
    let (s, r) = piper::chan(100);
    Task::blocking(async move {
        for _ in Signals::new(&[signal_hook::SIGINT]).unwrap().forever() {
            s.send(()).await;
        }
    })
    .detach();
    r
});

fn main() {
    smol::run(async {
        println!("Waiting for Ctrl-C");
        CTRL_C.recv().await;
        println!("Done!");
    })
}
