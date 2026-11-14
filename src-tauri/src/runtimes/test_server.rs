//! A tiny HTTP/1.1 server on 127.0.0.1 for tests. Each connection serves one request.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Close the connection after this many body bytes while announcing the full length.
    pub cut_after: Option<usize>,
}

impl Response {
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self { status, headers: Vec::new(), body, cut_after: None }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

pub struct TestServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

impl TestServer {
    pub fn start(handler: impl Fn(&Request) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Some(request) = read_request(&mut stream) else { continue };
                let response = handler(&request);
                log.lock().unwrap().push(request);
                let _ = write_response(&mut stream, &response);
            }
        });
        Self { base, requests }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut impl Read) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, headers, body })
}

fn write_response(stream: &mut impl Write, response: &Response) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {} Test\r\n", response.status)?;
    for (name, value) in &response.headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "Content-Length: {}\r\nConnection: close\r\n\r\n", response.body.len())?;
    let sent = response.cut_after.unwrap_or(response.body.len()).min(response.body.len());
    stream.write_all(&response.body[..sent])?;
    stream.flush()
}
