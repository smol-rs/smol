use futures::prelude::*;
use smol::Task;

fn unordered<T: Send + 'static>(
    iter: impl IntoIterator<Item = impl Future<Output = T> + Send + 'static>,
) -> piper::Receiver<T> {
    let v = iter.into_iter().collect::<Vec<_>>();
    let (s, r) = piper::chan(v.len());
    for f in v {
        let s = s.clone();
        Task::spawn(async move { s.send(f.await).await }).detach();
    }
    r
}

fn main() {
    smol::run(async {
        // TODO
    })
}
