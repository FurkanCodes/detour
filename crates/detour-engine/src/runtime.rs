//! Runs the packet loop on a background thread.

use crate::divert::{self, Api, Handle};
use crate::strategy::PacketPlanner;
use crate::{packet, DnsRedirect, Options};
use detour_core::DomainList;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Default)]
pub struct Stats {
    /// ClientHellos rewritten.
    pub tls: AtomicU64,
    pub http: AtomicU64,
    /// DNS queries redirected.
    pub dns: AtomicU64,
    /// Captured packets passed through the engine, not total network traffic.
    pub packets: AtomicU64,
    pub errors: AtomicU64,
}

pub struct Engine {
    handle: Arc<Handle>,
    thread: Option<JoinHandle<Result<(), String>>>,
    pub stats: Arc<Stats>,
}

impl Engine {
    /// Opens WinDivert and starts the packet loop. `hosts == None` means
    /// every host. Errors from opening the driver are returned directly.
    pub fn start(
        api: &Arc<Api>,
        mut opts: Options,
        hosts: Option<DomainList>,
        log: LogFn,
    ) -> Result<Engine, String> {
        let rewrites_tcp = !opts.ports.is_empty()
            && (!opts.strategy.split.is_empty()
                || opts.strategy.chunk_size.is_some()
                || opts.strategy.fake_ttl.is_some());
        if !rewrites_tcp
            && opts.dns_redirect.is_none()
            && opts.doh.is_none()
            && !opts.block_quic
        {
            return Err(
                "nothing to do: enable --split-pos, --chunk-size, --fake-ttl or --dns-redirect"
                    .into(),
            );
        }
        // Answering every lookup through an unreachable DoH server would
        // take all DNS down, so check it once before capturing anything.
        if let Some(server) = opts.doh {
            if let Err(e) = crate::doh::probe(server) {
                log(format!(
                    "Encrypted DNS via {server} is unreachable ({e}); using plain DNS instead"
                ));
                opts.doh = None;
            }
        }
        let handle = Arc::new(
            Handle::open(api, &filter(&opts)).map_err(|e| divert::explain_open_error(&e))?,
        );
        let stats = Arc::new(Stats::default());
        let changes_dns = opts.doh.is_some() || opts.dns_redirect.is_some();
        let thread = {
            let (handle, stats) = (handle.clone(), stats.clone());
            std::thread::Builder::new()
                .name("detour-engine".into())
                .spawn(move || packet_loop(&handle, &opts, hosts.as_ref(), &stats, &log))
                .map_err(|e| e.to_string())?
        };
        // Drop answers the ISP's resolver already poisoned.
        if changes_dns {
            crate::doh::flush_cache();
        }
        Ok(Engine {
            handle,
            thread: Some(thread),
            stats,
        })
    }

    pub fn handle(&self) -> Arc<Handle> {
        self.handle.clone()
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Waits for the loop to end and returns why it failed, if it did.
    pub fn join(&mut self) -> Result<(), String> {
        match self.thread.take() {
            Some(t) => t
                .join()
                .unwrap_or_else(|_| Err("engine thread panicked".into())),
            None => Ok(()),
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.handle.shutdown();
        self.join()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The WinDivert filter for `opts`. No ports means no TCP is captured: only
/// DNS (and UDP/443, when blocked) is.
pub fn filter(opts: &Options) -> String {
    let mut parts = Vec::new();
    if !opts.ports.is_empty() {
        let ports = opts
            .ports
            .iter()
            .map(|p| format!("tcp.DstPort == {p}"))
            .collect::<Vec<_>>()
            .join(" or ");
        // ClientHello: TLS handshake record (0x16) carrying handshake type 1.
        parts.push(format!(
            "(outbound and ({ports}) and tcp.PayloadLength > 5 and tcp.Payload[0] == 22 and tcp.Payload[5] == 1)"
        ));
    }
    if opts.ports.contains(&80) {
        parts.push("(outbound and tcp.DstPort == 80 and tcp.PayloadLength > 16 and (tcp.Payload[0] == 71 or tcp.Payload[0] == 80 or tcp.Payload[0] == 72 or tcp.Payload[0] == 68 or tcp.Payload[0] == 79 or tcp.Payload[0] == 67 or tcp.Payload[0] == 84))".into());
    }
    if opts.doh.is_some() || opts.dns_redirect.is_some() {
        parts.push("(outbound and udp.DstPort == 53)".into());
    }
    // The plain redirect also serves as the DoH fallback, so its answers
    // are captured in both modes.
    if let Some(r) = &opts.dns_redirect {
        parts.push(format!(
            "(inbound and ip and udp.SrcPort == {} and ip.SrcAddr == {})",
            r.port(),
            r.ip()
        ));
    }
    if opts.block_quic {
        parts.push("(outbound and udp.DstPort == 443)".into());
    }
    format!("!impostor and ({})", parts.join(" or "))
}

fn packet_loop(
    handle: &Arc<Handle>,
    opts: &Options,
    hosts: Option<&DomainList>,
    stats: &Arc<Stats>,
    log: &LogFn,
) -> Result<(), String> {
    let dns = opts
        .dns_redirect
        .map(|r| Arc::new(Mutex::new(DnsRedirect::new(r))));
    let encrypted = opts
        .doh
        .map(|server| {
            crate::doh::Pool::new(crate::doh::PoolContext {
                handle: handle.clone(),
                server,
                fallback: dns.clone(),
                stats: stats.clone(),
                log: log.clone(),
                verbose: opts.verbose,
            })
        })
        .transpose()
        .map_err(|e| format!("encrypted DNS startup: {e}"))?;
    let mut buf = vec![0u8; 65535];
    let mut planner = PacketPlanner::default();
    loop {
        let (len, mut addr) = match handle.recv(&mut buf) {
            Ok(r) => r,
            Err(e) if e.raw_os_error() == Some(divert::ERROR_NO_DATA) => return Ok(()),
            Err(e) if e.raw_os_error() == Some(divert::ERROR_INSUFFICIENT_BUFFER) => continue,
            Err(e) => return Err(format!("receive failed: {e}")),
        };
        let pkt = &mut buf[..len];
        stats.packets.fetch_add(1, Ordering::Relaxed);

        let mut replacement = None;
        if let Some(p) = packet::parse(pkt) {
            if opts.block_quic
                && addr.outbound()
                && p.protocol == packet::UDP
                && p.dst_port(pkt) == 443
            {
                continue;
            }
            if p.protocol == packet::TCP && addr.outbound() {
                replacement = planner.plan_parsed(&opts.strategy, pkt, &p, hosts);
            } else if p.protocol == packet::UDP {
                if addr.outbound()
                    && encrypted
                        .as_ref()
                        .is_some_and(|pool| pool.submit(pkt, &p, addr, hosts))
                {
                    continue;
                }
                // Reached by plain-DNS mode, and by DoH mode when its queue
                // is full: redirect rather than let a poisoned answer through.
                if let Some(dns) = &dns {
                    let mut dns = dns.lock().unwrap_or_else(|e| e.into_inner());
                    let now = Instant::now();
                    if addr.outbound() {
                        if let Some(name) = DnsRedirect::v6_query_to_drop(pkt, hosts) {
                            if opts.verbose {
                                log(format!("DNS {name} over IPv6 held back"));
                            }
                            continue;
                        }
                        if let Some(name) = dns.rewrite_query(pkt, hosts, now) {
                            stats.dns.fetch_add(1, Ordering::Relaxed);
                            if opts.verbose {
                                log(format!("DNS {name} -> {}", dns.resolver()));
                            }
                        }
                    } else {
                        dns.rewrite_response(pkt, now);
                    }
                }
            }
        }

        match replacement {
            Some(plan) => {
                if plan.is_tls {
                    stats.tls.fetch_add(1, Ordering::Relaxed);
                } else {
                    stats.http.fetch_add(1, Ordering::Relaxed);
                }
                if opts.verbose {
                    log(format!(
                        "{} {} -> {} packets",
                        if plan.is_tls { "TLS" } else { "HTTP" },
                        plan.host,
                        plan.packets.len()
                    ));
                }
                for p in plan.packets {
                    if let Err(e) = handle.send(p, &mut addr) {
                        let n = stats.errors.fetch_add(1, Ordering::Relaxed) + 1;
                        if n == 1 || n.is_multiple_of(100) {
                            log(format!("send failed ({n} total): {e}"));
                        }
                    }
                }
            }
            None => {
                if let Err(e) = handle.send(pkt, &mut addr) {
                    let n = stats.errors.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 || n.is_multiple_of(100) {
                        log(format!("send failed ({n} total): {e}"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Windows Administrator and the signed WinDivert driver"]
    fn driver_starts_and_stops_without_rewriting_traffic() {
        let dll = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/windivert/WinDivert.dll");
        let api = Api::load(&dll).expect("load bundled driver API");
        let all = Options::parse([
            "--ports=80,443,8443",
            "--block-quic",
            "--dns-redirect=77.88.8.8:1253",
        ])
        .unwrap()
        .unwrap();
        api.validate_filter(&filter(&all))
            .expect("system-wide filter is valid");
        let mut dns_only = all.clone();
        dns_only.ports.clear();
        api.validate_filter(&filter(&dns_only))
            .expect("DNS-only filter is valid");
        let opts = Options::parse(["--ports=1"]).unwrap().unwrap();
        let mut engine = Engine::start(&api, opts, Some(DomainList::new()), Arc::new(|_| {}))
            .expect("start engine");
        assert!(engine.is_running());
        let started = Instant::now();
        engine.stop().expect("stop engine");
        assert!(!engine.is_running());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn filter_matches_options() {
        let o = Options::parse(["--ports=443,8443", "--dns-redirect=77.88.8.8:1253"])
            .unwrap()
            .unwrap();
        let f = filter(&o);
        assert!(f.starts_with("!impostor and ("));
        assert!(f.contains("tcp.DstPort == 443 or tcp.DstPort == 8443"));
        assert!(f.contains("udp.SrcPort == 1253 and ip.SrcAddr == 77.88.8.8"));
        let plain = filter(&Options::parse(Vec::<&str>::new()).unwrap().unwrap());
        assert!(!plain.contains("udp"));
        let quic = filter(&Options::parse(["--block-quic"]).unwrap().unwrap());
        assert!(quic.contains("outbound and udp.DstPort == 443"));
        let doh = filter(
            &Options::parse(["--doh=1.1.1.1", "--dns-redirect=77.88.8.8:1253"])
                .unwrap()
                .unwrap(),
        );
        assert!(doh.contains("outbound and udp.DstPort == 53"));
        assert!(
            doh.contains("udp.SrcPort == 1253 and ip.SrcAddr == 77.88.8.8"),
            "the DoH fallback's answers must be captured"
        );
        let mut dns_only = o.clone();
        dns_only.ports.clear();
        let f = filter(&dns_only);
        assert!(!f.contains("tcp"), "{f}");
        assert!(f.starts_with("!impostor and ((outbound and udp.DstPort == 53)"), "{f}");
    }
}
