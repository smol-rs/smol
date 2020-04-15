use tokio::runtime::Runtime;

// TODO: make a threadpool

fn main() {
    // Create a tokio runtime.
    let rt = Runtime::new().unwrap();

    // Set up the tokio context on the current thread.
    rt.enter(|| {
        // Run a smol executor on the current thread.
        smol::run(async {
            println!("Hello world!");

            // Tokio now knows onto which runtime to spawn this future!
            let task = tokio::spawn(async { 1 + 2 });
            assert_eq!(task.await.unwrap(), 3);
        })
    });
}
