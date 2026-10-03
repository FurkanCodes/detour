//! Minimal blocking DNS-over-UDP client (A records), used by the proxy to
//! look names up through a resolver the ISP does not poison.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(2500);
const TRIES: usize = 2;

static NEXT_ID: AtomicU16 = AtomicU16::new(0x4b00);

pub fn build_query(id: u16, name: &str) -> io::Result<Vec<u8>> {
    let mut q = id.to_be_bytes().to_vec();
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(io::Error::other("invalid hostname"));
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    Ok(q)
}

/// A records from a DNS response whose transaction id is `id`.
pub fn parse_a_records(resp: &[u8], id: u16) -> io::Result<Vec<Ipv4Addr>> {
    let bad = || io::Error::other("malformed DNS response");
    if resp.len() < 12 || resp[..2] != id.to_be_bytes() || resp[2] & 0x80 == 0 {
        return Err(bad());
    }
    let rcode = resp[3] & 0x0f;
    if rcode != 0 {
        return Err(io::Error::other(format!("DNS error code {rcode}")));
    }
    let questions = u16::from_be_bytes([resp[4], resp[5]]);
    let answers = u16::from_be_bytes([resp[6], resp[7]]);
    let mut pos = 12;
    for _ in 0..questions {
        pos = skip_name(resp, pos).ok_or_else(bad)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..answers {
        pos = skip_name(resp, pos).ok_or_else(bad)?;
        let head = resp.get(pos..pos + 10).ok_or_else(bad)?;
        let (kind, len) = (u16::from_be_bytes([head[0], head[1]]), usize::from(u16::from_be_bytes([head[8], head[9]])));
        pos += 10;
        let data = resp.get(pos..pos + len).ok_or_else(bad)?;
        if kind == 1 && len == 4 {
            out.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        pos += len;
    }
    Ok(out)
}

fn skip_name(msg: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *msg.get(pos)?;
        if len == 0 {
            return Some(pos + 1);
        }
        if len & 0xc0 == 0xc0 {
            msg.get(pos + 1)?;
            return Some(pos + 2);
        }
        pos += 1 + usize::from(len);
    }
}

pub fn resolve_a(server: SocketAddrV4, name: &str) -> io::Result<Vec<Ipv4Addr>> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let query = build_query(id, name)?;
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.connect(SocketAddr::V4(server))?;
    let mut buf = [0u8; 1500];
    let mut last = io::Error::other("no response");
    for _ in 0..TRIES {
        socket.send(&query)?;
        match socket.recv(&mut buf) {
            Ok(n) => match parse_a_records(&buf[..n], id) {
                Ok(ips) if !ips.is_empty() => return Ok(ips),
                Ok(_) => last = io::Error::other("no A records"),
                Err(e) => last = e,
            },
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(id: u16, ips: &[[u8; 4]]) -> Vec<u8> {
        let mut r = build_query(id, "discord.com").unwrap();
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = ips.len() as u8;
        for ip in ips {
            r.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
            r.extend_from_slice(ip);
        }
        r
    }

    #[test]
    fn query_has_one_question() {
        let q = build_query(7, "a.b.com.").unwrap();
        assert_eq!(&q[..2], &[0, 7]);
        assert_eq!(crate::dns::parse_query(&q), Some((7, "a.b.com".into())));
        assert!(build_query(1, "bad..name").is_err());
    }

    #[test]
    fn parses_answers_with_compression() {
        let r = response(9, &[[162, 159, 135, 232], [1, 2, 3, 4]]);
        assert_eq!(
            parse_a_records(&r, 9).unwrap(),
            [Ipv4Addr::new(162, 159, 135, 232), Ipv4Addr::new(1, 2, 3, 4)]
        );
    }

    #[test]
    fn rejects_wrong_id_errors_and_truncation() {
        let r = response(9, &[[1, 1, 1, 1]]);
        assert!(parse_a_records(&r, 10).is_err());
        let mut nx = r.clone();
        nx[3] = 0x83;
        assert!(parse_a_records(&nx, 9).is_err());
        assert!(parse_a_records(&r[..r.len() - 2], 9).is_err());
        assert!(parse_a_records(&[], 9).is_err());
    }

    #[test]
    fn resolves_through_a_local_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = match server.local_addr().unwrap() {
            SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            let id = u16::from_be_bytes([buf[0], buf[1]]);
            assert!(n > 12);
            server.send_to(&response(id, &[[9, 9, 9, 9]]), from).unwrap();
        });
        assert_eq!(resolve_a(addr, "discord.com").unwrap(), [Ipv4Addr::new(9, 9, 9, 9)]);
        handle.join().unwrap();
    }
}
