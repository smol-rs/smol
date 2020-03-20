use std::net::{TcpListener, TcpStream};

use futures::io;
use http_types::{Response, StatusCode};
use io_arc::IoArc;
use smol::{Async, Task};

async fn serve(addr: String, stream: Async<TcpStream>) -> http_types::Result<()> {
    let stream = IoArc::new(stream);
    async_h1::accept(&addr, stream.clone(), |_req| async move {
        let mut res = Response::new(StatusCode::Ok);
        res.insert_header("Content-Type", "text/plain")?;
        res.set_body("Hello from async-h1!");
        Ok(res)
    })
    .await
}

fn main() -> io::Result<()> {
    // Create a thread pool.
    for _ in 0..num_cpus::get_physical().max(1) {
        std::thread::spawn(|| smol::run(futures::future::pending::<()>()));
    }

    smol::block_on(async {
        let listener = Async::<TcpListener>::bind("127.0.0.1:8080")?;
        let addr = format!("http://{}", listener.get_ref().local_addr()?);
        println!("listening on {}", addr);

        loop {
            let (stream, _) = listener.accept().await?;
            Task::spawn(serve(addr.clone(), stream)).unwrap().forget();
        }
    })
}
