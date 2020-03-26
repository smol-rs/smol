use std::net::TcpListener;
use std::thread;

use futures::{future, io};
use http_types::{Request, Response, StatusCode};
use io_arc::IoArc;
use smol::{Async, Task};

/// Serves a request and returns a response.
async fn serve(_req: Request) -> http_types::Result<Response> {
    let mut res = Response::new(StatusCode::Ok);
    res.insert_header("Content-Type", "text/plain")?;
    res.set_body("Hello from async-h1!");
    Ok(res)
}

fn main() -> io::Result<()> {
    // Create a thread pool.
    let num_threads = num_cpus::get_physical().max(1);
    for _ in 0..num_threads {
        thread::spawn(|| smol::run(future::pending::<()>()));
    }

    smol::block_on(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:3000")?;
        let addr = format!("http://{}", listener.get_ref().local_addr()?);
        println!("Listening on {}", addr);

        loop {
            let (stream, _) = listener.accept().await?;
            let addr = addr.clone();
            Task::spawn(async move { async_h1::accept(&addr, IoArc::new(stream), serve).await })
                .unwrap()
                .forget();
        }
    })
}
