#[test]
fn spawn() {
    assert_eq!(42, smol::run(smol::Task::spawn(async { 42 })));
}

#[test]
fn spawn_detach() {
    let (s, r) = piper::chan(1);
    smol::Task::spawn(async move { s.send(()).await }).detach();
    assert_eq!(Some(()), smol::run(r.recv()));
}

#[test]
fn blocking() {
    assert_eq!(42, smol::run(smol::Task::blocking(async { 42 })));
}

#[test]
fn blocking_detach() {
    let (s, r) = piper::chan(1);
    smol::Task::blocking(async move { s.send(()).await }).detach();
    assert_eq!(Some(()), smol::run(r.recv()));
}
