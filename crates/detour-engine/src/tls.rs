//! Locates the SNI hostname inside a TLS ClientHello.

use std::ops::Range;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        (end <= self.buf.len()).then(|| self.pos = end)
    }

    fn u8(&mut self) -> Option<u8> {
        let v = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn u16(&mut self) -> Option<u16> {
        let hi = self.u8()?;
        let lo = self.u8()?;
        Some(u16::from_be_bytes([hi, lo]))
    }
}

/// Returns the byte range of the SNI hostname within `payload` (the TCP
/// payload of the first packet of a connection). The record may extend past
/// the packet; only the SNI itself has to be present.
pub fn find_sni(payload: &[u8]) -> Option<Range<usize>> {
    let mut r = Reader {
        buf: payload,
        pos: 0,
    };
    if r.u8()? != 0x16 {
        return None; // not a handshake record
    }
    r.skip(4)?; // record version + length
    if r.u8()? != 0x01 {
        return None; // not a ClientHello
    }
    r.skip(3 + 2 + 32)?; // handshake length, client version, random
    let session_id = usize::from(r.u8()?);
    r.skip(session_id)?;
    let ciphers = usize::from(r.u16()?);
    r.skip(ciphers)?;
    let compression = usize::from(r.u8()?);
    r.skip(compression)?;
    let ext_len = usize::from(r.u16()?);
    let end = (r.pos + ext_len).min(payload.len());

    while r.pos + 4 <= end {
        let ext_type = r.u16()?;
        let len = usize::from(r.u16()?);
        if ext_type == 0 {
            r.skip(2)?; // server name list length
            if r.u8()? != 0 {
                return None; // not a host_name entry
            }
            let name_len = usize::from(r.u16()?);
            let start = r.pos;
            r.skip(name_len)?;
            return Some(start..start + name_len);
        }
        r.skip(len)?;
    }
    None
}

/// Rewrites the first TLS record of `payload` as two records, cut at payload
/// offset `at`. Servers reassemble fragmented handshake messages; a filter
/// that reads only the first record never sees the whole hostname.
/// `None` if the record is incomplete or `at` is not inside its body.
pub fn split_record(payload: &[u8], at: usize) -> Option<Vec<u8>> {
    if payload.len() < 6 || payload[0] != 0x16 {
        return None;
    }
    let body_len = usize::from(u16::from_be_bytes([payload[3], payload[4]]));
    let end = 5 + body_len;
    if at <= 5 || at >= end || end > payload.len() {
        return None;
    }
    let mut out = Vec::with_capacity(payload.len() + 5);
    for body in [&payload[5..at], &payload[at..end]] {
        out.extend_from_slice(&payload[..3]);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
    }
    out.extend_from_slice(&payload[end..]);
    Some(out)
}

#[cfg(test)]
pub(crate) fn build_client_hello(host: &str) -> Vec<u8> {
    let mut sni = vec![0u8];
    sni.extend_from_slice(&(host.len() as u16).to_be_bytes());
    sni.extend_from_slice(host.as_bytes());
    let mut sni_ext = (sni.len() as u16).to_be_bytes().to_vec();
    sni_ext.extend_from_slice(&sni);

    let mut exts = vec![0x00, 0x0a, 0x00, 0x04, 0x00, 0x02, 0x00, 0x1d]; // supported_groups
    exts.extend_from_slice(&[0, 0]);
    exts.extend_from_slice(&(sni_ext.len() as u16).to_be_bytes());
    exts.extend_from_slice(&sni_ext);

    let mut body = vec![3, 3];
    body.extend_from_slice(&[7; 32]);
    body.push(0); // empty session id
    body.extend_from_slice(&[0, 4, 0x13, 0x01, 0x13, 0x02]);
    body.extend_from_slice(&[1, 0]);
    body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    body.extend_from_slice(&exts);

    let mut hs = vec![1];
    hs.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    hs.extend_from_slice(&body);

    let mut rec = vec![0x16, 3, 1];
    rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
    rec.extend_from_slice(&hs);
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_split_keeps_every_byte_and_adds_a_header() {
        let hello = build_client_hello("discord.com");
        let sni = find_sni(&hello).unwrap();
        let at = sni.start + 3;
        let split = split_record(&hello, at).unwrap();
        assert_eq!(split.len(), hello.len() + 5);
        assert_eq!(split[0], 0x16);
        let first = usize::from(u16::from_be_bytes([split[3], split[4]]));
        assert_eq!(first, at - 5);
        assert_eq!(split[5 + first], 0x16);
        let second = usize::from(u16::from_be_bytes([split[5 + first + 3], split[5 + first + 4]]));
        assert_eq!(5 + first + 5 + second, split.len());
        let body: Vec<u8> = [&split[5..5 + first], &split[10 + first..]].concat();
        assert_eq!(body, &hello[5..]);
        // The record header now sits inside the hostname, so a filter that
        // reads it as one string no longer sees "discord.com".
        let seen = find_sni(&split).map(|r| split[r].to_vec());
        assert_ne!(seen.as_deref(), Some(&b"discord.com"[..]));
    }

    #[test]
    fn record_split_rejects_bad_positions() {
        let hello = build_client_hello("discord.com");
        assert!(split_record(&hello, 5).is_none());
        assert!(split_record(&hello, hello.len()).is_none());
        assert!(split_record(&hello[..hello.len() - 1], 20).is_none());
        assert!(split_record(b"GET / HTTP/1.1", 8).is_none());
    }

    #[test]
    fn finds_hostname() {
        let hello = build_client_hello("discord.com");
        let range = find_sni(&hello).unwrap();
        assert_eq!(&hello[range], b"discord.com");
    }

    #[test]
    fn survives_truncation_after_sni() {
        let mut hello = build_client_hello("roblox.com");
        let range = find_sni(&hello).unwrap();
        hello.truncate(range.end);
        assert_eq!(&hello[find_sni(&hello).unwrap()], b"roblox.com");
    }

    #[test]
    fn rejects_truncated_inside_sni_and_non_tls() {
        let hello = build_client_hello("discord.com");
        let range = find_sni(&hello).unwrap();
        assert!(find_sni(&hello[..range.end - 1]).is_none());
        assert!(find_sni(b"GET / HTTP/1.1\r\n").is_none());
        assert!(find_sni(&[]).is_none());
        let mut not_hello = hello.clone();
        not_hello[5] = 2; // ServerHello
        assert!(find_sni(&not_hello).is_none());
    }
}
