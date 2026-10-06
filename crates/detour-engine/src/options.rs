//! Command-line options of `detour-engine`.

use crate::strategy::{SplitPos, Strategy};
use std::net::SocketAddrV4;
use std::path::PathBuf;

pub const USAGE: &str = "\
usage: detour-engine [options]

  --hostlist=FILE       only touch hosts in FILE (default: every host)
  --ports=443,8443      TCP ports carrying TLS (default: 443)
  --split-pos=1,sni+1   where to cut the ClientHello: N, sni, sni+N, sni-N
                        (default: 1,sni+1; 'none' disables splitting)
  --disorder            send the pieces in reverse order
  --fake-ttl=N          send a decoy ClientHello that expires after N hops
  --fake-sni=HOST       hostname used in the decoy (default: www.google.com)
  --dns-redirect=IP[:PORT]
                        send DNS queries for listed hosts to this resolver
  --block-quic          block UDP/443 so browsers retry HTTP/3 over TCP
  --chunk-size=N        split the TLS handshake into N-byte segments (1..128)
  --proxy-split=MODE    proxy engine: how to cut the ClientHello. chunk:N sends
                        the whole of it in N-byte segments, sni sends each
                        hostname byte apart, pos (default) uses --split-pos and
                        --chunk-size
  --tlsrec=POS          proxy engine: also cut the ClientHello into two TLS records
                        at POS (N, sni, sni+N, sni-N). Some servers refuse this
  -v, --verbose         log every rewritten connection";

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub hostlist: Option<PathBuf>,
    pub ports: Vec<u16>,
    pub strategy: Strategy,
    pub dns_redirect: Option<SocketAddrV4>,
    pub doh: Option<std::net::Ipv4Addr>,
    pub verbose: bool,
    /// Block outbound UDP/443 so browsers retry HTTP/3 over TCP.
    pub block_quic: bool,
}

impl Options {
    /// Parses arguments (without the program name). `Ok(None)` means help
    /// was requested.
    pub fn parse<I, S>(args: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut o = Options {
            hostlist: None,
            ports: vec![443],
            strategy: Strategy::default(),
            dns_redirect: None,
            doh: None,
            verbose: false,
            block_quic: false,
        };
        for arg in args {
            let arg = arg.as_ref();
            let (key, value) = match arg.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (arg, None),
            };
            match (key, value) {
                ("-h" | "--help", None) => return Ok(None),
                ("-v" | "--verbose", None) => o.verbose = true,
                ("--disorder", None) => o.strategy.disorder = true,
                ("--block-quic", None) => o.block_quic = true,
                ("--hostlist", Some(v)) => o.hostlist = Some(v.into()),
                ("--ports", Some(v)) => o.ports = parse_ports(v)?,
                ("--split-pos", Some(v)) => o.strategy.split = parse_splits(v)?,
                ("--proxy-split", Some(v)) => o.strategy.stream = v.parse()?,
                ("--tlsrec", Some(v)) => {
                    o.strategy.tls_record = Some(v.parse().map_err(|e| format!("--tlsrec: {e}"))?)
                }
                ("--chunk-size", Some(v)) => {
                    let size: usize = v.parse().map_err(|_| "bad --chunk-size")?;
                    if !(1..=128).contains(&size) { return Err("--chunk-size must be between 1 and 128".into()); }
                    o.strategy.chunk_size = Some(size);
                }
                ("--fake-ttl", Some(v)) => {
                    let ttl: u8 = v.parse().map_err(|_| format!("bad --fake-ttl {v:?}"))?;
                    if ttl == 0 {
                        return Err("--fake-ttl must be at least 1".into());
                    }
                    o.strategy.fake_ttl = Some(ttl);
                }
                ("--fake-sni", Some(v)) if !v.is_empty() => o.strategy.fake_sni = v.into(),
                ("--dns-redirect", Some(v)) => o.dns_redirect = Some(parse_resolver(v)?),
                ("--doh", Some(v)) => o.doh = Some(v.parse().map_err(|_| "--doh expects a resolver IPv4 address")?),
                _ => return Err(format!("unknown or incomplete option {arg:?}")),
            }
        }
        Ok(Some(o))
    }
}

fn parse_ports(v: &str) -> Result<Vec<u16>, String> {
    let ports: Result<Vec<u16>, _> = v.split(',').map(|p| p.trim().parse::<u16>()).collect();
    match ports {
        Ok(p) if !p.is_empty() && !p.contains(&0) => Ok(p),
        _ => Err(format!("bad --ports {v:?}")),
    }
}

fn parse_splits(v: &str) -> Result<Vec<SplitPos>, String> {
    if v == "none" {
        return Ok(Vec::new());
    }
    v.split(',').map(str::parse).collect()
}

fn parse_resolver(v: &str) -> Result<SocketAddrV4, String> {
    let with_port = if v.contains(':') {
        v.to_owned()
    } else {
        format!("{v}:53")
    };
    with_port
        .parse()
        .map_err(|_| format!("bad --dns-redirect {v:?} (expected IPv4[:port])"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let o = Options::parse(Vec::<&str>::new()).unwrap().unwrap();
        assert_eq!(o.ports, [443]);
        assert_eq!(o.strategy, Strategy::default());
        assert!(o.hostlist.is_none() && o.dns_redirect.is_none());
    }

    #[test]
    fn parses_everything() {
        let o = Options::parse([
            "--hostlist=C:\\k dir\\hosts.txt",
            "--ports=443, 8443",
            "--split-pos=2,sni+3",
            "--disorder",
            "--fake-ttl=4",
            "--fake-sni=example.com",
            "--dns-redirect=77.88.8.8:1253",
            "-v",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(o.hostlist.unwrap().to_str(), Some("C:\\k dir\\hosts.txt"));
        assert_eq!(o.ports, [443, 8443]);
        assert_eq!(o.strategy.split, [SplitPos::Abs(2), SplitPos::Sni(3)]);
        assert!(o.strategy.disorder && o.verbose);
        assert_eq!(o.strategy.fake_ttl, Some(4));
        assert_eq!(o.dns_redirect, Some("77.88.8.8:1253".parse().unwrap()));
    }

    #[test]
    fn dns_port_defaults_to_53() {
        let o = Options::parse(["--dns-redirect=1.1.1.1"]).unwrap().unwrap();
        assert_eq!(o.dns_redirect, Some("1.1.1.1:53".parse().unwrap()));
    }

    #[test]
    fn none_disables_splitting() {
        let o = Options::parse(["--split-pos=none", "--fake-ttl=3"])
            .unwrap()
            .unwrap();
        assert!(o.strategy.split.is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "--bogus",
            "--ports=0",
            "--ports=x",
            "--fake-ttl=0",
            "--fake-ttl=300",
            "--split-pos=a",
            "--dns-redirect=nope",
            "--hostlist",
        ] {
            assert!(Options::parse([bad]).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn help() {
        assert_eq!(Options::parse(["--help"]), Ok(None));
    }
}
