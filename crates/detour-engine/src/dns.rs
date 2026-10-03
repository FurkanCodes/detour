//! Redirects DNS queries for listed hosts to another resolver, working
//! around ISPs that poison answers for blocked domains. Responses get the
//! original resolver's address written back so the app never notices.

use crate::packet::{self, be16, put16, IpVersion};
use detour_core::DomainList;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

const NAT_TTL: Duration = Duration::from_secs(30);
const MAX_PENDING: usize = 4096;
const PRUNE_INTERVAL: Duration = Duration::from_secs(1);

/// Returns the transaction id and lower-cased name of the first question of
/// a DNS query.
pub fn parse_query(payload: &[u8]) -> Option<(u16, String)> {
    if payload.len() < 13 || payload[2] & 0x80 != 0 || be16(payload, 4) == 0 {
        return None;
    }
    let mut name = String::new();
    let mut pos = 12;
    loop {
        let len = usize::from(*payload.get(pos)?);
        if len == 0 {
            break;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        let label = payload.get(pos + 1..pos + 1 + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.extend(label.iter().map(|&b| char::from(b.to_ascii_lowercase())));
        pos += 1 + len;
    }
    (!name.is_empty()).then(|| (be16(payload, 0), name))
}

#[derive(Debug)]
pub struct DnsRedirect {
    resolver: SocketAddrV4,
    /// (client ip, client port, txid) -> (original resolver, first seen)
    nat: HashMap<(Ipv4Addr, u16, u16), (SocketAddrV4, Instant)>,
    last_prune: Option<Instant>,
}

impl DnsRedirect {
    pub fn new(resolver: SocketAddrV4) -> Self {
        Self {
            resolver,
            nat: HashMap::new(),
            last_prune: None,
        }
    }

    pub fn resolver(&self) -> SocketAddrV4 {
        self.resolver
    }

    /// Points an outbound query at the replacement resolver if its name is
    /// in `hosts`. Returns the name when the packet was rewritten.
    pub fn rewrite_query(
        &mut self,
        pkt: &mut [u8],
        hosts: Option<&DomainList>,
        now: Instant,
    ) -> Option<String> {
        let p = udp_v4(pkt)?;
        let (txid, name) = parse_query(p.payload(pkt))?;
        if hosts.is_some_and(|h| !h.matches(&name)) {
            return None;
        }
        let client = (ipv4_at(pkt, 12), p.src_port(pkt));
        let original = SocketAddrV4::new(ipv4_at(pkt, 16), p.dst_port(pkt));
        if original == self.resolver {
            return None;
        }

        if self
            .last_prune
            .is_none_or(|last| now.saturating_duration_since(last) >= PRUNE_INTERVAL)
        {
            self.nat
                .retain(|_, (_, t)| now.saturating_duration_since(*t) < NAT_TTL);
            self.last_prune = Some(now);
        }
        let key = (client.0, client.1, txid);
        if self.nat.len() >= MAX_PENDING && !self.nat.contains_key(&key) {
            // Leave overflow traffic untouched rather than grow without bound
            // or drop DNS and interrupt the rest of the user's connection.
            return None;
        }
        self.nat.insert(key, (original, now));

        pkt[16..20].copy_from_slice(&self.resolver.ip().octets());
        put16(pkt, p.ip_hdr_len + 2, self.resolver.port());
        Some(name)
    }

    /// Windows asks every configured DNS server, including IPv6 ones that
    /// cannot be redirected, and the poisoned answer from those can win.
    /// Returns the name of an IPv6 query for a listed host: the caller
    /// should drop it so the client falls back to the redirected IPv4 server.
    pub fn v6_query_to_drop(pkt: &[u8], hosts: Option<&DomainList>) -> Option<String> {
        let p = packet::parse(pkt)
            .filter(|p| p.version == IpVersion::V6 && p.protocol == packet::UDP)?;
        if p.dst_port(pkt) != 53 {
            return None;
        }
        let (_, name) = parse_query(p.payload(pkt))?;
        hosts.is_none_or(|h| h.matches(&name)).then_some(name)
    }

    /// Restores the original resolver as the source of a redirected answer.
    pub fn rewrite_response(&mut self, pkt: &mut [u8], now: Instant) -> bool {
        let Some(p) = udp_v4(pkt) else {
            return false;
        };
        if ipv4_at(pkt, 12) != *self.resolver.ip() || p.src_port(pkt) != self.resolver.port() {
            return false;
        }
        let payload = p.payload(pkt);
        if payload.len() < 2 {
            return false;
        }
        let key = (ipv4_at(pkt, 16), p.dst_port(pkt), be16(payload, 0));
        match self.nat.get(&key) {
            Some(&(original, seen)) if now.duration_since(seen) < NAT_TTL => {
                pkt[12..16].copy_from_slice(&original.ip().octets());
                put16(pkt, p.ip_hdr_len, original.port());
                true
            }
            _ => false,
        }
    }
}

fn udp_v4(pkt: &[u8]) -> Option<packet::Parsed> {
    packet::parse(pkt).filter(|p| p.version == IpVersion::V4 && p.protocol == packet::UDP)
}

fn ipv4_at(pkt: &[u8], at: usize) -> Ipv4Addr {
    Ipv4Addr::new(pkt[at], pkt[at + 1], pkt[at + 2], pkt[at + 3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::testutil::*;

    fn query(name: &str, txid: u16) -> Vec<u8> {
        let mut q = txid.to_be_bytes().to_vec();
        q.extend_from_slice(&[0x01, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        q
    }

    fn resolver() -> SocketAddrV4 {
        "77.88.8.8:1253".parse().unwrap()
    }

    #[test]
    fn parses_names() {
        assert_eq!(
            parse_query(&query("Discord.COM", 7)),
            Some((7, "discord.com".into()))
        );
        let mut response = query("a.com", 1);
        response[2] |= 0x80;
        assert!(parse_query(&response).is_none());
        assert!(parse_query(&query("a.com", 1)[..14]).is_none());
        assert!(parse_query(b"").is_none());
    }

    #[test]
    fn redirects_listed_queries_and_restores_answers() {
        let hosts = DomainList::parse("discord.com");
        let mut dns = DnsRedirect::new(resolver());
        let now = Instant::now();

        let mut q = ipv4(
            17,
            [10, 0, 0, 5],
            [192, 168, 1, 1],
            &udp(40000, 53, &query("gateway.discord.com", 0xbeef)),
        );
        let name = dns.rewrite_query(&mut q, Some(&hosts), now).unwrap();
        assert_eq!(name, "gateway.discord.com");
        assert_eq!(&q[16..20], &[77, 88, 8, 8]);
        assert_eq!(be16(&q, 20 + 2), 1253);

        let mut a = ipv4(
            17,
            [77, 88, 8, 8],
            [10, 0, 0, 5],
            &udp(1253, 40000, &query("gateway.discord.com", 0xbeef)),
        );
        assert!(dns.rewrite_response(&mut a, now));
        assert_eq!(&a[12..16], &[192, 168, 1, 1]);
        assert_eq!(be16(&a, 20), 53);
    }

    #[test]
    fn leaves_unlisted_and_unmatched_alone() {
        let hosts = DomainList::parse("discord.com");
        let mut dns = DnsRedirect::new(resolver());
        let now = Instant::now();

        let mut other = ipv4(
            17,
            [10, 0, 0, 5],
            [192, 168, 1, 1],
            &udp(40000, 53, &query("example.org", 1)),
        );
        let before = other.clone();
        assert!(dns.rewrite_query(&mut other, Some(&hosts), now).is_none());
        assert_eq!(other, before);

        let mut stray = ipv4(
            17,
            [77, 88, 8, 8],
            [10, 0, 0, 5],
            &udp(1253, 40001, &query("discord.com", 9)),
        );
        assert!(!dns.rewrite_response(&mut stray, now));
    }

    #[test]
    fn drops_only_ipv6_dns_queries_for_listed_hosts() {
        let hosts = DomainList::parse("discord.com");
        let v6 = |name: &str, port: u16| ipv6(17, &udp(40000, port, &query(name, 1)));

        assert_eq!(
            DnsRedirect::v6_query_to_drop(&v6("gateway.discord.com", 53), Some(&hosts)),
            Some("gateway.discord.com".into())
        );
        assert!(DnsRedirect::v6_query_to_drop(&v6("example.org", 53), Some(&hosts)).is_none());
        assert!(DnsRedirect::v6_query_to_drop(&v6("discord.com", 5353), Some(&hosts)).is_none());
        let v4 = ipv4(
            17,
            [1; 4],
            [2; 4],
            &udp(40000, 53, &query("discord.com", 1)),
        );
        assert!(DnsRedirect::v6_query_to_drop(&v4, Some(&hosts)).is_none());
    }

    #[test]
    fn answers_expire() {
        let mut dns = DnsRedirect::new(resolver());
        let t = Instant::now();
        let mut q = ipv4(
            17,
            [10, 0, 0, 5],
            [8, 8, 8, 8],
            &udp(40000, 53, &query("discord.com", 3)),
        );
        dns.rewrite_query(&mut q, None, t).unwrap();
        let mut a = ipv4(
            17,
            [77, 88, 8, 8],
            [10, 0, 0, 5],
            &udp(1253, 40000, &query("discord.com", 3)),
        );
        assert!(!dns.rewrite_response(&mut a, t + NAT_TTL));
    }

    #[test]
    fn query_already_aimed_at_resolver_is_untouched() {
        let mut dns = DnsRedirect::new(resolver());
        let mut q = ipv4(
            17,
            [10, 0, 0, 5],
            [77, 88, 8, 8],
            &udp(40000, 1253, &query("discord.com", 3)),
        );
        assert!(dns.rewrite_query(&mut q, None, Instant::now()).is_none());
    }

    #[test]
    fn pending_queries_are_bounded_and_overflow_passes_through() {
        let mut dns = DnsRedirect::new(resolver());
        let now = Instant::now();
        for txid in 0..5000 {
            let mut pkt = ipv4(
                17,
                [10, 0, 0, 5],
                [8, 8, 8, 8],
                &udp(40000, 53, &query("discord.com", txid)),
            );
            let before = pkt.clone();
            if txid >= 4096 {
                assert!(dns.rewrite_query(&mut pkt, None, now).is_none());
                assert_eq!(pkt, before);
            } else {
                assert!(dns.rewrite_query(&mut pkt, None, now).is_some());
            }
        }
        assert!(dns.nat.len() <= 4096);
        let mut pkt = ipv4(
            17,
            [10, 0, 0, 5],
            [8, 8, 8, 8],
            &udp(40000, 53, &query("discord.com", 6000)),
        );
        assert!(dns.rewrite_query(&mut pkt, None, now + NAT_TTL).is_some());
        assert_eq!(dns.nat.len(), 1);
    }
}
