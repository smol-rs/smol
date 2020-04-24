// TODO: document
use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use piper::Sender;
use scraper::{Html, Selector};
use smol::Task;

const ROOT: &str = "https://www.rust-lang.org";

/// Fetches the HTML contents of a web page.
async fn fetch(url: String, sender: Sender<String>) {
    let body = surf::get(&url).recv_string().await;
    let body = body.unwrap_or_default();
    sender.send(body).await;
}

/// Extracts links from a HTML body.
fn links(body: String) -> Vec<String> {
    let mut v = Vec::new();
    for elem in Html::parse_fragment(&body).select(&Selector::parse("a").unwrap()) {
        if let Some(link) = elem.value().attr("href") {
            v.push(link.to_string());
        }
    }
    v
}

fn main() -> Result<()> {
    smol::run(async {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        seen.insert(ROOT.to_string());
        queue.push_back(ROOT.to_string());

        let (s, r) = piper::chan(100);
        let mut tasks = 0;

        while queue.len() + tasks > 0 {
            while let Some(url) = queue.pop_front() {
                println!("{}", url);
                tasks += 1;
                Task::spawn(fetch(url, s.clone())).detach();
            }

            let body = match r.try_recv() {
                Some(body) => body,
                None => r.recv().await.unwrap(),
            };
            tasks -= 1;

            for mut url in links(body) {
                if url.starts_with('/') {
                    url = format!("{}{}", ROOT, url);
                }
                if url.starts_with(ROOT) && seen.insert(url.clone()) {
                    url = url.trim_end_matches('/').to_string();
                    queue.push_back(url);
                }
            }
        }
        Ok(())
    })
}
