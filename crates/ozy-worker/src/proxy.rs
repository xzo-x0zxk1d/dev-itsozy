//! Loopback forward proxy that every Chromium request must pass through.
//! Each destination is validated with the same policy as the API (public IPv4,
//! ports 80/443 only) and the connection is pinned to the validated address,
//! which also defeats DNS rebinding from inside the page.
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional},
    net::{TcpListener, TcpStream},
};

const MAX_HEAD: usize = 16 * 1024;

pub async fn start() -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(120), handle(client)).await;
            });
        }
    });
    Ok(address)
}

/// Resolves and validates a destination, returning the pinned socket address.
pub async fn validated(host: &str, port: u16) -> Result<SocketAddr, &'static str> {
    let scheme = if port == 443 { "https" } else { "http" };
    let url = ozy_core::net::validate_url(&format!("{scheme}://{host}:{port}/")).map_err(|_| "destination blocked")?;
    let addresses = ozy_core::net::resolve_public(&url).await.map_err(|_| "destination blocked")?;
    addresses.first().copied().ok_or("destination blocked")
}

fn split_host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if authority.starts_with('[') {
        return None; // IPv6 literals are never permitted.
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), default_port)),
    }
}

/// Parses the first request line: CONNECT host:port, or an absolute http:// URI.
pub fn parse_target(head: &str) -> Option<(bool, String, u16, String)> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method == "CONNECT" {
        let (host, port) = split_host_port(target, 443)?;
        return Some((true, host, port, String::new()));
    }
    let rest = target.strip_prefix("http://")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let (host, port) = split_host_port(authority, 80)?;
    let path = if path.is_empty() { "/" } else { path };
    // Rewrite to origin-form and force one request per upstream connection.
    let mut rewritten = format!("{method} {path} {version}\r\n");
    for header in head.split("\r\n").skip(1) {
        let lower = header.to_ascii_lowercase();
        if header.is_empty() || lower.starts_with("proxy-") || lower.starts_with("connection:") || lower.starts_with("keep-alive:") {
            continue;
        }
        rewritten.push_str(header);
        rewritten.push_str("\r\n");
    }
    rewritten.push_str("Connection: close\r\n\r\n");
    Some((false, host, port, rewritten))
}

async fn handle(mut client: TcpStream) -> std::io::Result<()> {
    let mut buffer = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buffer.len() > MAX_HEAD {
            return client.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n").await;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let Some((tunnel, host, port, rewritten)) = parse_target(&head) else {
        return client.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n").await;
    };
    let address = match validated(&host, port).await {
        Ok(address) => address,
        Err(_) => return client.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n").await,
    };
    let mut upstream = match tokio::time::timeout(std::time::Duration::from_secs(10), TcpStream::connect(address)).await {
        Ok(Ok(stream)) => stream,
        _ => return client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n").await,
    };
    if tunnel {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    } else {
        upstream.write_all(rewritten.as_bytes()).await?;
    }
    let body_start = &buffer[head_end..];
    if !body_start.is_empty() {
        upstream.write_all(body_start).await?;
    }
    copy_bidirectional(&mut client, &mut upstream).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_connect_and_absolute_form() {
        let (tunnel, host, port, _) = parse_target("CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n").unwrap();
        assert!(tunnel && host == "example.com" && port == 443);
        let (tunnel, host, port, rewritten) = parse_target(
            "GET http://example.com/a?b=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\n\r\n",
        )
        .unwrap();
        assert!(!tunnel && host == "example.com" && port == 80);
        assert!(rewritten.starts_with("GET /a?b=1 HTTP/1.1\r\n"));
        assert!(!rewritten.to_ascii_lowercase().contains("proxy-connection"));
        assert!(rewritten.ends_with("Connection: close\r\n\r\n"));
        assert!(parse_target("CONNECT [::1]:443 HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_target("GET ftp://x/ HTTP/1.1\r\n\r\n").is_none());
    }
    #[tokio::test]
    async fn blocks_private_and_odd_ports() {
        assert!(validated("127.0.0.1", 443).await.is_err());
        assert!(validated("169.254.169.254", 80).await.is_err());
        assert!(validated("10.0.0.8", 443).await.is_err());
        assert!(validated("example.com", 22).await.is_err());
        assert!(validated("localhost", 80).await.is_err());
    }
}
