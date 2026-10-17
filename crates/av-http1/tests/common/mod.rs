//! Shared test machinery: a scripted fake HTTP server on loopback (plaintext), and the pieces the
//! TLS suite builds on. No Docker and no network beyond loopback.
#![allow(dead_code)]

pub mod pki;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use av_http1::{Client, Endpoint, Timeouts};

/// What the fake server does after it has read a request.
#[derive(Clone, Debug)]
pub enum Step {
    /// Write these bytes (and flush).
    Write(Vec<u8>),
    /// Pause.
    Sleep(Duration),
    /// Keep the connection open, silent, until the client closes it (5 s at most).
    Hold,
}

pub fn w(bytes: &[u8]) -> Step {
    Step::Write(bytes.to_vec())
}

/// A complete `200 OK` with a Content-Length body.
pub fn ok(body: &[u8]) -> Vec<Step> {
    let mut v = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    vec![Step::Write(v)]
}

/// Reads one request (head, then Content-Length bytes of body) and returns every byte of it.
pub fn read_request<S: Read>(s: &mut S) -> Vec<u8> {
    let mut got = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return got,
            Ok(n) => got.extend_from_slice(&tmp[..n]),
        }
    };
    let head = String::from_utf8_lossy(&got[..head_end]).to_ascii_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while got.len() < head_end + len {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&tmp[..n]),
        }
    }
    got
}

/// Runs a script against an accepted stream. Write errors are ignored (a client that has already
/// refused the response closes its end).
pub fn run_steps<S: Read + Write>(s: &mut S, steps: &[Step]) {
    for step in steps {
        match step {
            Step::Write(b) => {
                if s.write_all(b).is_err() || s.flush().is_err() {
                    return;
                }
            }
            Step::Sleep(d) => thread::sleep(*d),
            Step::Hold => {
                let end = Instant::now() + Duration::from_secs(5);
                let mut tmp = [0u8; 64];
                while Instant::now() < end {
                    match s.read(&mut tmp) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}

pub fn pause(ms: u64) -> Step {
    Step::Sleep(Duration::from_millis(ms))
}

/// A scripted plaintext server. Connection N gets script N (later connections get nothing and are
/// closed). `finish` stops it and returns the exact bytes of every request it read.
pub struct Fake {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Vec<Vec<u8>>>>,
}

pub fn fake(scripts: Vec<Vec<Step>>) -> Fake {
    fake_on("127.0.0.1:0", scripts).expect("bind 127.0.0.1")
}

pub fn fake_on(bind: &str, scripts: Vec<Vec<Step>>) -> std::io::Result<Fake> {
    let listener = TcpListener::bind(bind)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        let mut scripts = scripts.into_iter();
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    s.set_nonblocking(false).ok();
                    s.set_nodelay(true).ok();
                    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let req = read_request(&mut s);
                    seen.push(req);
                    if let Some(steps) = scripts.next() {
                        run_steps(&mut s, &steps);
                    }
                    let _ = s.shutdown(std::net::Shutdown::Write);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
        seen
    });
    Ok(Fake {
        addr,
        stop,
        handle: Some(handle),
    })
}

impl Fake {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint::parse(&self.url()).expect("loopback endpoint")
    }

    pub fn client(&self) -> Client {
        client_for(&self.url())
    }

    /// Stops the server and returns the raw bytes of each request it received, in order.
    pub fn finish(mut self) -> Vec<Vec<u8>> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().expect("not finished twice").join().expect("fake server thread")
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn client_for(url: &str) -> Client {
    Client::new(
        Endpoint::parse(url).expect("endpoint"),
        None,
        Timeouts::uniform(Duration::from_secs(3)),
    )
    .expect("client")
}

pub fn client_with(url: &str, timeouts: Timeouts) -> Client {
    Client::new(Endpoint::parse(url).expect("endpoint"), None, timeouts).expect("client")
}

pub fn connect_probe(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}
