//! A tiny HTTP/1.1 server for tests: one thread, one request per connection.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub type Handler = Box<dyn Fn(&Request) -> (u16, String) + Send>;

pub struct Fake {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

pub fn serve(handler: Handler) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let mut headers = BTreeMap::new();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let len = headers
                .get("content-length")
                .and_then(|l| l.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let request = Request {
                method,
                path,
                headers,
                body: String::from_utf8(body).unwrap(),
            };
            let (status, body) = handler(&request);
            log.lock().unwrap().push(request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Fake { url, requests }
}
