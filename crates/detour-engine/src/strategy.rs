//! Decides how to rewrite an outbound ClientHello.

use crate::{packet, tls};
use detour_core::DomainList;
use std::str::FromStr;

/// Where to cut the TLS payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitPos {
    /// Byte offset from the start of the TCP payload.
    Abs(usize),
    /// Offset relative to the first byte of the SNI hostname.
    Sni(isize),
}

impl SplitPos {
    pub(crate) fn resolve(self, sni_start: usize, len: usize) -> Option<usize> {
        let at = match self {
            SplitPos::Abs(n) => isize::try_from(n).ok()?,
            SplitPos::Sni(d) => isize::try_from(sni_start).ok()?.checked_add(d)?,
        };
        let at = usize::try_from(at).ok()?;
        (at > 0 && at < len).then_some(at)
    }
}

impl FromStr for SplitPos {
    type Err = String;

    /// `N`, `sni`, `sni+N` or `sni-N`.
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let bad = || format!("bad split position {s:?} (use N, sni, sni+N or sni-N)");
        match s.strip_prefix("sni") {
            Some("") => Ok(SplitPos::Sni(0)),
            Some(rest) if rest.starts_with(['+', '-']) => {
                rest.parse::<isize>().map(SplitPos::Sni).map_err(|_| bad())
            }
            Some(_) => Err(bad()),
            None => s.parse::<usize>().map(SplitPos::Abs).map_err(|_| bad()),
        }
    }
}

/// How the proxy engine cuts a TLS ClientHello (SpoofDPI's split modes, as
/// used by BypaxDPI). Only the TCP segmentation changes, never the TLS bytes,
/// so every server accepts the result; a packet engine cannot afford the
/// hundreds of segments `Chunk(1)` produces, which is why this is proxy-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamSplit {
    /// The same cuts as the packet engine (`split`, `chunk_size`).
    #[default]
    Positions,
    /// Each byte of the hostname in its own segment.
    Sni,
    /// The whole ClientHello in segments of this many bytes.
    Chunk(usize),
}

impl FromStr for StreamSplit {
    type Err = String;

    /// `pos`, `sni` or `chunk:N`.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("bad proxy split {s:?} (use pos, sni or chunk:N with N from 1 to 128)");
        match s.trim() {
            "pos" => Ok(StreamSplit::Positions),
            "sni" => Ok(StreamSplit::Sni),
            other => {
                let n: usize = other.strip_prefix("chunk:").ok_or_else(bad)?.parse().map_err(|_| bad())?;
                if (1..=128).contains(&n) {
                    Ok(StreamSplit::Chunk(n))
                } else {
                    Err(bad())
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strategy {
    pub split: Vec<SplitPos>,
    pub chunk_size: Option<usize>,
    /// Proxy engine only: how to cut a ClientHello.
    pub stream: StreamSplit,
    /// Proxy engine only: also cut the ClientHello into two TLS records here.
    /// Some servers (many Turkish banks and government sites among them)
    /// reject a ClientHello spread over two records, so no preset uses it.
    pub tls_record: Option<SplitPos>,
    /// Send the segments last-to-first.
    pub disorder: bool,
    /// Send a decoy ClientHello with this TTL ahead of the real one. It
    /// expires in transit: the filter sees it, the server never does.
    pub fake_ttl: Option<u8>,
    pub fake_sni: String,
}

impl Default for Strategy {
    fn default() -> Self {
        Self {
            split: vec![SplitPos::Abs(1), SplitPos::Sni(1)],
            chunk_size: None,
            stream: StreamSplit::Positions,
            tls_record: None,
            disorder: false,
            fake_ttl: None,
            fake_sni: "www.google.com".into(),
        }
    }
}

/// Packets to send in place of the original, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub host: String,
    pub packets: Vec<Vec<u8>>,
}

/// Packet-loop scratch space. Buffers are reused across ClientHellos.
#[derive(Default)]
pub struct PacketPlanner {
    cuts: Vec<usize>,
    packets: Vec<Vec<u8>>,
    used: usize,
}

pub struct Planned<'a> {
    pub host: &'a str,
    pub packets: &'a mut [Vec<u8>],
    pub is_tls: bool,
}

impl Strategy {
    /// Byte offsets at which to cut a payload whose hostname starts at
    /// `sni_start`: chunk boundaries (TLS only) plus the explicit split
    /// positions, sorted and deduplicated.
    fn collect_cuts(&self, out: &mut Vec<usize>, len: usize, sni_start: usize, is_tls: bool) {
        out.clear();
        if let Some(size) = self.chunk_size.filter(|s| *s > 0 && is_tls) {
            out.extend((size..len).step_by(size));
        }
        out.extend(self.split.iter().filter_map(|s| s.resolve(sni_start, len)));
        out.sort_unstable();
        out.dedup();
    }

    /// For stream-based engines: the hostname and cut offsets for the first
    /// bytes of a connection (a TLS ClientHello or an HTTP request head).
    /// `None` when it is neither, the host is not covered, or no cut applies.
    pub fn payload_cuts(
        &self,
        payload: &[u8],
        hosts: Option<&DomainList>,
    ) -> Option<(String, Vec<usize>)> {
        let tls_sni = tls::find_sni(payload);
        let is_tls = tls_sni.is_some();
        let sni = tls_sni.or_else(|| crate::http::find_host(payload))?;
        let host = std::str::from_utf8(&payload[sni.clone()]).ok()?;
        if hosts.is_some_and(|h| !h.matches(host)) {
            return None;
        }
        let len = payload.len();
        let cuts: Vec<usize> = match self.stream {
            StreamSplit::Sni if is_tls => (sni.start..=sni.end).filter(|&c| c > 0 && c < len).collect(),
            StreamSplit::Chunk(size) if is_tls => {
                // The first record is the ClientHello; anything after it goes whole.
                let record_end = (5 + usize::from(u16::from_be_bytes([payload[3], payload[4]]))).min(len);
                let mut cuts: Vec<usize> = (size..record_end).step_by(size).collect();
                if record_end < len {
                    cuts.push(record_end);
                }
                cuts
            }
            _ => {
                let mut cuts = Vec::new();
                self.collect_cuts(&mut cuts, len, sni.start, is_tls);
                cuts
            }
        };
        (!cuts.is_empty()).then(|| (host.to_owned(), cuts))
    }

    /// Returns `None` when the packet should pass through untouched: not a
    /// ClientHello, host not in `hosts` (`None` means every host), or
    /// nothing to do.
    pub fn plan(&self, pkt: &[u8], hosts: Option<&DomainList>) -> Option<Plan> {
        let mut scratch = PacketPlanner::default();
        let host = scratch.plan(self, pkt, hosts)?.host.to_owned();
        scratch.packets.truncate(scratch.used);
        Some(Plan {
            host,
            packets: scratch.packets,
        })
    }
}

impl PacketPlanner {
    pub fn plan<'a>(
        &'a mut self,
        strategy: &Strategy,
        pkt: &'a [u8],
        hosts: Option<&DomainList>,
    ) -> Option<Planned<'a>> {
        let p = packet::parse(pkt).filter(|p| p.protocol == packet::TCP)?;
        self.plan_parsed(strategy, pkt, &p, hosts)
    }

    pub fn plan_parsed<'a>(
        &'a mut self,
        strategy: &Strategy,
        pkt: &'a [u8],
        p: &packet::Parsed,
        hosts: Option<&DomainList>,
    ) -> Option<Planned<'a>> {
        if p.protocol != packet::TCP {
            return None;
        }
        let payload = p.payload(pkt);
        let tls_sni = tls::find_sni(payload);
        let is_tls = tls_sni.is_some();
        let sni = tls_sni.or_else(|| crate::http::find_host(payload))?;
        let host = std::str::from_utf8(&payload[sni.clone()]).ok()?;
        if hosts.is_some_and(|h| !h.matches(host)) {
            return None;
        }

        strategy.collect_cuts(&mut self.cuts, payload.len(), sni.start, is_tls);
        let fake_ttl = if is_tls { strategy.fake_ttl } else { None };
        if self.cuts.is_empty() && fake_ttl.is_none() {
            return None;
        }

        let fake_count = usize::from(fake_ttl.is_some());
        self.used = self.cuts.len() + 1 + fake_count;
        if self.packets.len() < self.used {
            self.packets.resize_with(self.used, Vec::new);
        }
        if let Some(ttl) = fake_ttl {
            let fake = &mut self.packets[0];
            packet::tcp_segment_into(pkt, p, 0, payload.len(), 0, fake);
            let decoy = strategy.fake_sni.bytes().cycle();
            for (dst, b) in fake[p.payload_off + sni.start..p.payload_off + sni.end]
                .iter_mut()
                .zip(decoy)
            {
                *dst = b;
            }
            packet::set_ttl(fake, p, ttl);
        }

        let mut start = 0;
        for (i, end) in self.cuts.iter().copied().chain([payload.len()]).enumerate() {
            // Write directly into send order so disorder mode also retains
            // each segment's buffer capacity between packets.
            let at = if strategy.disorder {
                self.cuts.len() - i
            } else {
                i
            };
            packet::tcp_segment_into(
                pkt,
                p,
                start,
                end,
                i as u16,
                &mut self.packets[fake_count + at],
            );
            start = end;
        }

        Some(Planned {
            host,
            packets: &mut self.packets[..self.used],
            is_tls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::testutil::*;
    use crate::packet::{be32, parse, TCP};

    #[test]
    fn system_wide_http_and_tls_cover_domains_outside_builtin_lists() {
        let strategy = Strategy::default();
        let mut scratch = PacketPlanner::default();
        let tls = hello_packet("www.example.org");
        assert!(scratch.plan(&strategy, &tls, None).unwrap().is_tls);
        let request = b"GET / HTTP/1.1\r\nHost: www.example.org\r\n\r\n";
        let mut l4 = tcp_hdr(50000, 80, 1000);
        l4.extend_from_slice(request);
        let http = ipv4(TCP, [10, 0, 0, 1], [1, 2, 3, 4], &l4);
        let plan = scratch.plan(&strategy, &http, None).unwrap();
        assert!(!plan.is_tls);
        assert_eq!(plan.host, "www.example.org");
        let rebuilt: Vec<u8> = plan
            .packets
            .iter()
            .flat_map(|p| parse(p).unwrap().payload(p).iter().copied())
            .collect();
        assert_eq!(rebuilt, request);
        assert!(scratch
            .plan(&strategy, &http, Some(&DomainList::parse("discord.com")))
            .is_none());
    }

    fn hello_packet(host: &str) -> Vec<u8> {
        let mut l4 = tcp_hdr(50000, 443, 1000);
        l4.extend_from_slice(&tls::build_client_hello(host));
        ipv4(TCP, [10, 0, 0, 1], [1, 2, 3, 4], &l4)
    }

    fn payloads(plan: &Plan) -> Vec<Vec<u8>> {
        plan.packets
            .iter()
            .map(|p| parse(p).unwrap().payload(p).to_vec())
            .collect()
    }

    fn list(text: &str) -> DomainList {
        DomainList::parse(text)
    }

    #[test]
    fn parses_positions() {
        assert_eq!("2".parse(), Ok(SplitPos::Abs(2)));
        assert_eq!("sni".parse(), Ok(SplitPos::Sni(0)));
        assert_eq!("sni+3".parse(), Ok(SplitPos::Sni(3)));
        assert_eq!("sni-1".parse(), Ok(SplitPos::Sni(-1)));
        assert!("sni3".parse::<SplitPos>().is_err());
        assert!("x".parse::<SplitPos>().is_err());
        assert!("-1".parse::<SplitPos>().is_err());
    }

    #[test]
    fn splits_listed_host_and_reassembles() {
        let pkt = hello_packet("updates.discord.com");
        let original = parse(&pkt).unwrap().payload(&pkt).to_vec();
        let plan = Strategy::default()
            .plan(&pkt, Some(&list("discord.com")))
            .unwrap();

        assert_eq!(plan.host, "updates.discord.com");
        assert_eq!(plan.packets.len(), 3);
        let parts = payloads(&plan);
        assert_eq!(parts[0].len(), 1);
        assert_eq!(parts.concat(), original);

        let sni = tls::find_sni(&original).unwrap();
        assert_eq!(parts[0].len() + parts[1].len(), sni.start + 1);
        for p in &plan.packets {
            let hdr = parse(p).unwrap();
            assert_eq!(hdr.total_len, p.len());
        }
        let seqs: Vec<u32> = plan.packets.iter().map(|p| be32(p, 24)).collect();
        assert_eq!(seqs[1], seqs[0] + parts[0].len() as u32);
        assert_eq!(seqs[2], seqs[1] + parts[1].len() as u32);
    }

    #[test]
    fn leaves_other_hosts_alone() {
        let pkt = hello_packet("example.org");
        assert!(Strategy::default()
            .plan(&pkt, Some(&list("discord.com")))
            .is_none());
        assert!(Strategy::default().plan(&pkt, None).is_some());
    }

    #[test]
    fn ignores_non_tls_and_udp() {
        let mut l4 = tcp_hdr(1, 80, 5);
        l4.extend_from_slice(b"GET / HTTP/1.1\r\n\r\n");
        let http = ipv4(TCP, [1; 4], [2; 4], &l4);
        assert!(Strategy::default().plan(&http, None).is_none());
        let dns = ipv4(17, [1; 4], [2; 4], &udp(1, 53, b"x"));
        assert!(Strategy::default().plan(&dns, None).is_none());
    }

    #[test]
    fn disorder_reverses_segments() {
        let pkt = hello_packet("discord.com");
        let plan = Strategy {
            split: vec![SplitPos::Abs(1)],
            disorder: true,
            ..Strategy::default()
        }
        .plan(&pkt, None)
        .unwrap();
        let parts = payloads(&plan);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].len(), 1);
        assert!(be32(&plan.packets[0], 24) > be32(&plan.packets[1], 24));
    }

    #[test]
    fn fake_has_decoy_sni_low_ttl_and_same_length() {
        let pkt = hello_packet("discord.com");
        let plan = Strategy {
            split: vec![SplitPos::Abs(2)],
            fake_ttl: Some(3),
            fake_sni: "www.google.com".into(),
            ..Strategy::default()
        }
        .plan(&pkt, None)
        .unwrap();

        assert_eq!(plan.packets.len(), 3);
        let fake = &plan.packets[0];
        assert_eq!(fake[8], 3);
        assert_eq!(fake.len(), pkt.len());
        let fp = parse(fake).unwrap();
        let range = tls::find_sni(fp.payload(fake)).unwrap();
        assert_eq!(&fp.payload(fake)[range], b"www.google.");
        assert_eq!(plan.packets[1][8], 64);
    }

    #[test]
    fn out_of_range_positions_are_dropped() {
        let pkt = hello_packet("discord.com");
        let none = Strategy {
            split: vec![SplitPos::Abs(0), SplitPos::Abs(9999), SplitPos::Sni(-9999)],
            ..Strategy::default()
        };
        assert!(none.plan(&pkt, None).is_none());
    }
}
