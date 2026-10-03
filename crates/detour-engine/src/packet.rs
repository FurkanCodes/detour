//! Minimal IPv4/IPv6 + TCP/UDP header parsing and TCP segment building.
//! Checksums are left stale: the caller recomputes them before sending.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpVersion {
    V4,
    V6,
}

pub const TCP: u8 = 6;
pub const UDP: u8 = 17;

#[derive(Debug, Clone, Copy)]
pub struct Parsed {
    pub version: IpVersion,
    pub protocol: u8,
    pub ip_hdr_len: usize,
    pub payload_off: usize,
    /// Packet length according to the IP header (trailing padding excluded).
    pub total_len: usize,
}

impl Parsed {
    pub fn payload<'a>(&self, pkt: &'a [u8]) -> &'a [u8] {
        &pkt[self.payload_off..self.total_len]
    }

    pub fn src_port(&self, pkt: &[u8]) -> u16 {
        be16(pkt, self.ip_hdr_len)
    }

    pub fn dst_port(&self, pkt: &[u8]) -> u16 {
        be16(pkt, self.ip_hdr_len + 2)
    }
}

/// Parses an IP packet carrying TCP or UDP. Fragments, other protocols and
/// IPv6 extension headers yield `None`.
pub fn parse(pkt: &[u8]) -> Option<Parsed> {
    let (version, ip_hdr_len, protocol, total_len) = match pkt.first()? >> 4 {
        4 => {
            let ihl = usize::from(pkt[0] & 0x0f) * 4;
            if ihl < 20 || pkt.len() < ihl {
                return None;
            }
            let total = usize::from(be16(pkt, 2));
            let frag = be16(pkt, 6) & 0x3fff; // MF flag + fragment offset
            if frag != 0 || total < ihl || total > pkt.len() {
                return None;
            }
            (IpVersion::V4, ihl, pkt[9], total)
        }
        6 => {
            if pkt.len() < 40 {
                return None;
            }
            let total = 40 + usize::from(be16(pkt, 4));
            if total > pkt.len() {
                return None;
            }
            (IpVersion::V6, 40, pkt[6], total)
        }
        _ => return None,
    };

    let l4_hdr_len = match protocol {
        TCP => {
            if total_len < ip_hdr_len + 20 {
                return None;
            }
            usize::from(pkt[ip_hdr_len + 12] >> 4) * 4
        }
        UDP => 8,
        _ => return None,
    };
    let payload_off = ip_hdr_len + l4_hdr_len;
    if l4_hdr_len < 8 || (protocol == TCP && l4_hdr_len < 20) || payload_off > total_len {
        return None;
    }
    Some(Parsed {
        version,
        protocol,
        ip_hdr_len,
        payload_off,
        total_len,
    })
}

/// Copies the headers plus `payload[start..end]` into a new packet, fixing
/// the length fields and TCP sequence number. `id_delta` is added to the
/// IPv4 identification so split segments are distinguishable.
pub fn tcp_segment(pkt: &[u8], p: &Parsed, start: usize, end: usize, id_delta: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(p.payload_off + end - start);
    tcp_segment_into(pkt, p, start, end, id_delta, &mut out);
    out
}

/// Writes into a retained buffer, avoiding allocations after warm-up.
pub fn tcp_segment_into(
    pkt: &[u8],
    p: &Parsed,
    start: usize,
    end: usize,
    id_delta: u16,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.reserve(p.payload_off + end - start);
    out.extend_from_slice(&pkt[..p.payload_off]);
    out.extend_from_slice(&pkt[p.payload_off + start..p.payload_off + end]);

    let len = out.len();
    match p.version {
        IpVersion::V4 => {
            put16(out, 2, len as u16);
            let id = be16(out, 4).wrapping_add(id_delta);
            put16(out, 4, id);
        }
        IpVersion::V6 => put16(out, 4, (len - 40) as u16),
    }
    let seq_at = p.ip_hdr_len + 4;
    let seq = be32(out, seq_at).wrapping_add(start as u32);
    out[seq_at..seq_at + 4].copy_from_slice(&seq.to_be_bytes());
}

pub fn set_ttl(pkt: &mut [u8], p: &Parsed, ttl: u8) {
    match p.version {
        IpVersion::V4 => pkt[8] = ttl,
        IpVersion::V6 => pkt[7] = ttl,
    }
}

pub fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

pub fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// Builds an IPv4 packet with a 20-byte header and the given L4 header + payload.
    pub fn ipv4(proto: u8, src: [u8; 4], dst: [u8; 4], l4: &[u8]) -> Vec<u8> {
        let mut p = vec![0x45, 0, 0, 0, 0x12, 0x34, 0x40, 0, 64, proto, 0, 0];
        p.extend_from_slice(&src);
        p.extend_from_slice(&dst);
        p.extend_from_slice(l4);
        let len = p.len() as u16;
        put16(&mut p, 2, len);
        p
    }

    pub fn ipv6(proto: u8, l4: &[u8]) -> Vec<u8> {
        let mut p = vec![0x60, 0, 0, 0, 0, 0, proto, 64];
        p.extend_from_slice(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        p.extend_from_slice(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        p.extend_from_slice(l4);
        let len = l4.len() as u16;
        put16(&mut p, 4, len);
        p
    }

    pub fn tcp_hdr(sport: u16, dport: u16, seq: u32) -> Vec<u8> {
        let mut h = vec![0u8; 20];
        put16(&mut h, 0, sport);
        put16(&mut h, 2, dport);
        h[4..8].copy_from_slice(&seq.to_be_bytes());
        h[12] = 5 << 4;
        h[13] = 0x18; // PSH|ACK
        h
    }

    pub fn udp(sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; 8];
        put16(&mut h, 0, sport);
        put16(&mut h, 2, dport);
        put16(&mut h, 4, (8 + payload.len()) as u16);
        h.extend_from_slice(payload);
        h
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn parses_ipv4_tcp() {
        let mut l4 = tcp_hdr(50000, 443, 1000);
        l4.extend_from_slice(b"hello");
        let pkt = ipv4(TCP, [10, 0, 0, 1], [1, 2, 3, 4], &l4);
        let p = parse(&pkt).unwrap();
        assert_eq!(p.version, IpVersion::V4);
        assert_eq!(p.payload(&pkt), b"hello");
        assert_eq!(p.dst_port(&pkt), 443);
        assert_eq!(p.src_port(&pkt), 50000);
    }

    #[test]
    fn ignores_trailing_padding() {
        let mut pkt = ipv4(UDP, [1; 4], [2; 4], &udp(1, 53, b"abc"));
        pkt.extend_from_slice(&[0, 0, 0, 0]);
        let p = parse(&pkt).unwrap();
        assert_eq!(p.payload(&pkt), b"abc");
    }

    #[test]
    fn rejects_fragments_and_garbage() {
        let mut pkt = ipv4(TCP, [1; 4], [2; 4], &tcp_hdr(1, 2, 3));
        put16(&mut pkt, 6, 0x2000); // more-fragments
        assert!(parse(&pkt).is_none());
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x45, 0, 0]).is_none());
        assert!(parse(&[0x70; 40]).is_none());
    }

    #[test]
    fn parses_ipv6_udp() {
        let l4 = udp(1, 53, b"xy");
        let mut pkt = vec![0x60, 0, 0, 0, 0, l4.len() as u8, UDP, 64];
        pkt.extend_from_slice(&[0; 32]);
        pkt.extend_from_slice(&l4);
        let p = parse(&pkt).unwrap();
        assert_eq!(p.version, IpVersion::V6);
        assert_eq!(p.payload(&pkt), b"xy");
    }

    #[test]
    fn segment_fixes_lengths_and_seq() {
        let mut l4 = tcp_hdr(50000, 443, u32::MAX - 1);
        l4.extend_from_slice(b"0123456789");
        let pkt = ipv4(TCP, [1; 4], [2; 4], &l4);
        let p = parse(&pkt).unwrap();

        let seg = tcp_segment(&pkt, &p, 4, 10, 1);
        let sp = parse(&seg).unwrap();
        assert_eq!(sp.payload(&seg), b"456789");
        assert_eq!(be16(&seg, 2) as usize, seg.len());
        assert_eq!(be16(&seg, 4), 0x1235);
        assert_eq!(be32(&seg, 20 + 4), (u32::MAX - 1).wrapping_add(4));
    }
}
