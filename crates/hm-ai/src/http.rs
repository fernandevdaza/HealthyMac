//! A minimal HTTP/1.1 GET for talking to runtimes on localhost.
//!
//! Only ever connects to 127.0.0.1. The UI polls through this, so it is strictly bounded in time and
//! memory no matter how the other end behaves.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use hm_core::t;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEAD_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_string())
}

pub(crate) fn get(port: u16, path: &str) -> io::Result<Response> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    stream.write_all(
        format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\n\
             User-Agent: HealthyMac\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )?;

    let mut received = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    ErrorKind::TimedOut,
                    t!("sin respuesta a tiempo", "no response in time"),
                )
            })?;
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                return parse(&received, true)?
                    .ok_or_else(|| invalid(t!("respuesta incompleta", "incomplete response")));
            }
            Ok(read) => {
                received.extend_from_slice(&chunk[..read]);
                if received.len() > MAX_RESPONSE_BYTES {
                    return Err(invalid(t!(
                        "respuesta demasiado grande",
                        "response too large"
                    )));
                }
                if let Some(response) = parse(&received, false)? {
                    return Ok(response);
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// Returns the response once it is complete. `Ok(None)` means more bytes are needed. With `at_eof`
/// the connection has closed, which ends a response that declared no length.
fn parse(received: &[u8], at_eof: bool) -> io::Result<Option<Response>> {
    let Some(head_end) = received.windows(4).position(|window| window == b"\r\n\r\n") else {
        if received.len() > MAX_HEAD_BYTES {
            return Err(invalid(t!(
                "cabeceras demasiado grandes",
                "headers too large"
            )));
        }
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&received[..head_end]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|line| line.starts_with("HTTP/1."))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| invalid(t!("línea de estado no válida", "invalid status line")))?;

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
    }

    let body = &received[head_end + 4..];
    if chunked {
        return Ok(decode_chunked(body)?.map(|body| Response { status, body }));
    }
    match content_length {
        Some(length) if body.len() >= length => Ok(Some(Response {
            status,
            body: body[..length].to_vec(),
        })),
        Some(_) => Ok(None),
        None if at_eof => Ok(Some(Response {
            status,
            body: body.to_vec(),
        })),
        None => Ok(None),
    }
}

/// `Ok(None)` while the terminating chunk hasn't arrived.
fn decode_chunked(mut input: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            return Ok(None);
        };
        let line = String::from_utf8_lossy(&input[..line_end]);
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| invalid(t!("tamaño de trozo no válido", "invalid chunk size")))?;
        if size > MAX_RESPONSE_BYTES {
            return Err(invalid(t!("trozo demasiado grande", "chunk too large")));
        }
        input = &input[line_end + 2..];
        if size == 0 {
            return Ok(Some(out));
        }
        let Some(chunk) = input.get(..size) else {
            return Ok(None);
        };
        out.extend_from_slice(chunk);
        let Some(rest) = input.get(size + 2..) else {
            return Ok(None);
        };
        input = rest;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    /// A throwaway server on an ephemeral port. Answers raw bytes per path, 404 otherwise.
    pub(crate) struct FakeServer {
        pub port: u16,
        stop: Arc<AtomicBool>,
    }

    impl FakeServer {
        pub(crate) fn raw(routes: &[(&str, Vec<u8>)]) -> Self {
            let routes: HashMap<String, Vec<u8>> = routes
                .iter()
                .map(|(path, bytes)| (path.to_string(), bytes.clone()))
                .collect();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            thread::spawn(move || {
                while !stopping.load(Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    let mut request = [0u8; 2048];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let text = String::from_utf8_lossy(&request[..read]).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let response = routes
                        .get(&path)
                        .cloned()
                        .unwrap_or_else(|| http_response(404, "<html>no encontrado</html>"));
                    let _ = stream.write_all(&response);
                }
            });
            Self { port, stop }
        }

        pub(crate) fn json(routes: &[(&str, u16, &str)]) -> Self {
            let routes: Vec<(&str, Vec<u8>)> = routes
                .iter()
                .map(|(path, status, body)| (*path, http_response(*status, body)))
                .collect();
            Self::raw(&routes)
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn http_response(status: u16, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    pub(crate) fn closed_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn reads_content_length_and_stops_without_waiting_for_close() {
        let server = FakeServer::json(&[("/a", 200, r#"{"ok":true}"#)]);
        let response = get(server.port, "/a").unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.json().unwrap()["ok"], true);
        assert_eq!(get(server.port, "/missing").unwrap().status, 404);
    }

    #[test]
    fn decodes_chunked_bodies() {
        let body = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\n\r\n";
        let server = FakeServer::raw(&[("/c", body.to_vec())]);
        assert_eq!(get(server.port, "/c").unwrap().json().unwrap()["a"], 1);
    }

    #[test]
    fn a_body_without_a_length_ends_at_close() {
        let server = FakeServer::raw(&[("/n", b"HTTP/1.0 200 OK\r\n\r\nhola".to_vec())]);
        assert_eq!(get(server.port, "/n").unwrap().body, b"hola");
    }

    #[test]
    fn nothing_listening_fails_fast() {
        let started = Instant::now();
        assert!(get(closed_port(), "/").is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn a_server_that_never_answers_is_abandoned_at_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let started = Instant::now();
        assert!(get(port, "/").is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(listener);
    }

    #[test]
    fn malformed_and_oversized_responses_are_errors() {
        let server = FakeServer::raw(&[
            ("/garbage", b"SSH-2.0-OpenSSH_9.6\r\n\r\n".to_vec()),
            (
                "/truncated",
                b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nhola".to_vec(),
            ),
            (
                "/badchunk",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n".to_vec(),
            ),
        ]);
        assert!(get(server.port, "/garbage").is_err());
        assert!(get(server.port, "/truncated").is_err());
        assert!(get(server.port, "/badchunk").is_err());
        assert!(decode_chunked(b"FFFFFFFFFFFFFFFF\r\n").is_err());
    }
}
