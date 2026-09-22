//! A small blocking HTTP/1.1 client for plain-text loopback calls.
//!
//! It is used to probe a running instance, to ask one to shut down, and to
//! read LibreHardwareMonitor's local web server. None of those need TLS, so
//! this works in builds without the HTTPS client.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// Perform one request and return the status and body.
pub fn request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> std::io::Result<Response> {
    let addr = resolve(host, port)?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\nAccept: application/json\r\nContent-Length: 0\r\n"
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;

    let mut raw = Vec::new();
    // Bodies here are small; the cap stops a misbehaving server from
    // consuming memory without bound.
    stream.take(8 * 1024 * 1024).read_to_end(&mut raw)?;
    parse_response(&raw)
}

fn resolve(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "host did not resolve"))
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_owned())
}

/// Split a raw HTTP/1.x response into status and decoded body.
pub fn parse_response(raw: &[u8]) -> std::io::Result<Response> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| invalid("no header terminator"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| invalid("empty response"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| invalid("malformed status line"))?;

    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if name == "content-length" {
            length = value.parse().ok();
        }
    }

    let decoded = if chunked {
        decode_chunked(body)?
    } else if let Some(len) = length {
        body[..len.min(body.len())].to_vec()
    } else {
        body.to_vec()
    };
    Ok(Response { status, body: String::from_utf8_lossy(&decoded).into_owned() })
}

fn decode_chunked(mut body: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| invalid("truncated chunk header"))?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim().to_owned();
        let size = usize::from_str_radix(&size_text, 16).map_err(|_| invalid("bad chunk size"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size {
            return Err(invalid("truncated chunk"));
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbodyEXTRA";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "body");
    }

    #[test]
    fn parses_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap().body, "Wikipedia");
    }

    #[test]
    fn reads_to_close_without_length() {
        let raw = b"HTTP/1.0 404 Not Found\r\nServer: x\r\n\r\nmissing";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 404);
        assert_eq!(r.body, "missing");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_response(b"not http").is_err());
    }
}
