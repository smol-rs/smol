use futures_util::future;

#[test]
fn smoke() {
    std::thread::spawn(|| {
        smol::run(future::pending::<()>());
    });
    let res = smol::block_on(async { 1 + 2 });
    assert_eq!(res, 3);
}

#[test]
#[should_panic = "boom"]
fn panic() {
    std::thread::spawn(|| {
        smol::run(future::pending::<()>());
    });
    smol::block_on(async {
        // This panic should get propagated into the parent thread.
        panic!("boom");
    });
}

#[test]
fn nested_block_on() {
    std::thread::spawn(|| {
        smol::run(future::pending::<()>());
    });

    let x = smol::block_on(async {
        let a = smol::block_on(async { smol::block_on(async { future::ready(3).await }) });
        let b = smol::block_on(async { smol::block_on(async { future::ready(2).await }) });
        a + b
    });

    assert_eq!(x, 3 + 2);
}
