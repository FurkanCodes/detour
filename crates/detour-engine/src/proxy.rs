//! Local HTTP/HTTPS proxy engine, for platforms without a packet driver.
//!
//! Apps send `CONNECT host:443` (or a plain proxied HTTP request) to a
//! loopback port. The proxy looks the host up through a resolver of your
//! choice, connects, and writes the first bytes of the connection (the TLS
//! ClientHello or HTTP head) in several small TCP segments so a filter that
//! reads the hostname from the first packet misses it. After that the
//! connection is a plain byte pipe.

use crate::dnsclient;
use crate::strategy::Strategy;
use detour_core::DomainList;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const MAX_CONNECTIONS: usize = 2048;
const MAX_HEAD: usize = 16 * 1024;
/// Splits with fewer pieces than this pause between them.
const MAX_PAUSED_PIECES: usize = 16;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A host's addresses are tried in a staggered race: the next one starts if
/// the previous has not connected within this time (RFC 8305's default).
const DIAL_STAGGER: Duration = Duration::from_millis(250);
const MAX_DIALS: usize = 8;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the rest of a TLS record that arrived in pieces.
const RECORD_TIMEOUT: Duration = Duration::from_millis(250);

pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

/// Looks a hostname up and returns its IPv4 addresses.
pub type ResolveFn = Arc<dyn Fn(&str) -> io::Result<Vec<Ipv4Addr>> + Send + Sync>;

#[derive(Debug, Default)]
pub struct ProxyStats {
    pub tls: AtomicU64,
    pub http: AtomicU64,
    pub dns: AtomicU64,
    pub connections: AtomicU64,
    pub errors: AtomicU64,
}

#[derive(Clone)]
pub struct ProxyConfig {
    pub strategy: Strategy,
    /// `None` means every host.
    pub hosts: Option<DomainList>,
    /// Clean resolvers for covered hosts, asked in order until one gives an
    /// address (some answer a name they cannot place, steered CDN names for
    /// one, with nothing at all). If every one fails the connection fails:
    /// the system resolver would hand back the provider's block page, which
    /// the browser shows as a certificate error. Empty: the system resolver.
    pub resolvers: Vec<Resolver>,
    /// Pause between the pieces of a split write, so each leaves in its own
    /// TCP segment.
    pub piece_delay: Duration,
    pub verbose: bool,
}

#[derive(Clone)]
pub struct Resolver {
    /// Shown in detailed logs.
    pub name: String,
    pub lookup: ResolveFn,
}

impl Resolver {
    /// Plain DNS over UDP.
    pub fn udp(server: SocketAddrV4) -> Self {
        Resolver {
            name: server.to_string(),
            lookup: Arc::new(move |host| dnsclient::resolve_a(server, host)),
        }
    }
}

impl ProxyConfig {
    /// The proxy honours the splitting and DNS options; a decoy packet
    /// (`--fake-ttl`) needs raw packets, which a proxy cannot send.
    pub fn from_options(opts: &crate::Options, hosts: Option<DomainList>) -> Self {
        ProxyConfig {
            strategy: opts.strategy.clone(),
            hosts,
            resolvers: opts.dns_redirect.map(Resolver::udp).into_iter().collect(),
            piece_delay: Duration::from_millis(3),
            verbose: opts.verbose,
        }
    }
}

pub struct Proxy {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    pub stats: Arc<ProxyStats>,
}

impl Proxy {
    /// Starts listening on `127.0.0.1:port` (0 picks a free port).
    pub fn start(cfg: ProxyConfig, port: u16, log: LogFn) -> io::Result<Proxy> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(ProxyStats::default());
        let shared = Arc::new(Shared {
            cfg,
            stats: stats.clone(),
            log,
            active: AtomicUsize::new(0),
            sockets: Mutex::new(Sockets::default()),
        });
        let thread = {
            let (stop, shared) = (stop.clone(), shared.clone());
            std::thread::Builder::new()
                .name("detour-proxy".into())
                .spawn(move || accept_loop(listener, stop, shared))?
        };
        Ok(Proxy {
            addr,
            stop,
            thread: Some(thread),
            shared,
            stats,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Stops listening and cuts every tunnel that is still open, so nothing
    /// keeps flowing through Detour once it is off.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocking accept().
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.shared.close_all();
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Shared {
    cfg: ProxyConfig,
    stats: Arc<ProxyStats>,
    log: LogFn,
    active: AtomicUsize,
    sockets: Mutex<Sockets>,
}

/// Handles to the sockets of every connection in flight, so `stop` can shut
/// them down instead of waiting for the peers to hang up.
#[derive(Default)]
struct Sockets {
    closed: bool,
    next: u64,
    live: HashMap<u64, TcpStream>,
}

impl Shared {
    /// Remembers `stream` until `forget`. Returns `None` (after shutting the
    /// stream) if the proxy is already stopping.
    fn track(&self, stream: &TcpStream) -> Option<u64> {
        let clone = stream.try_clone().ok()?;
        let mut s = self.sockets.lock().unwrap_or_else(|e| e.into_inner());
        if s.closed {
            let _ = stream.shutdown(Shutdown::Both);
            return None;
        }
        s.next += 1;
        let id = s.next;
        s.live.insert(id, clone);
        Some(id)
    }

    fn forget(&self, id: u64) {
        self.sockets.lock().unwrap_or_else(|e| e.into_inner()).live.remove(&id);
    }

    fn close_all(&self) {
        let mut s = self.sockets.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        for (_, stream) in s.live.drain() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Keeps a socket registered while it is held.
struct Tracked<'a> {
    shared: &'a Shared,
    ids: Vec<u64>,
}

impl<'a> Tracked<'a> {
    fn new(shared: &'a Shared) -> Self {
        Tracked { shared, ids: Vec::new() }
    }

    fn add(&mut self, stream: &TcpStream) -> io::Result<()> {
        match self.shared.track(stream) {
            Some(id) => {
                self.ids.push(id);
                Ok(())
            }
            None => Err(io::Error::other("proxy is stopping")),
        }
    }
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        for id in &self.ids {
            self.shared.forget(*id);
        }
    }
}

fn accept_loop(listener: TcpListener, stop: Arc<AtomicBool>, shared: Arc<Shared>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(client) = conn else { continue };
        if shared.active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            shared.stats.errors.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        shared.active.fetch_add(1, Ordering::Relaxed);
        shared.stats.connections.fetch_add(1, Ordering::Relaxed);
        let shared = shared.clone();
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("detour-conn".into())
            .spawn(move || {
                if let Err(e) = handle(client, &worker) {
                    let n = worker.stats.errors.fetch_add(1, Ordering::Relaxed) + 1;
                    if worker.cfg.verbose && (n <= 5 || n.is_multiple_of(50)) {
                        (worker.log)(format!("Proxy connection failed: {e}"));
                    }
                }
                worker.active.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            // The closure never ran, so its slot has to be released here.
            shared.active.fetch_sub(1, Ordering::Relaxed);
            shared.stats.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

enum Request {
    /// `CONNECT host:port`
    Tunnel { host: String, port: u16 },
    /// Plain HTTP; `head` is the request rewritten to origin form.
    Http { host: String, port: u16, head: Vec<u8> },
}

fn handle(mut client: TcpStream, shared: &Shared) -> io::Result<()> {
    let mut tracked = Tracked::new(shared);
    tracked.add(&client)?;
    client.set_nodelay(true)?;
    client.set_read_timeout(Some(HEAD_TIMEOUT))?;
    let (request, leftover) = read_request(&mut client)?;
    let (host, port) = match &request {
        Request::Tunnel { host, port } | Request::Http { host, port, .. } => (host.clone(), *port),
    };

    let target = match connect_target(&host, port, shared) {
        Ok(s) => s,
        Err(e) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
            return Err(e);
        }
    };
    target.set_nodelay(true)?;
    tracked.add(&target)?;
    let mut server = target;

    match request {
        Request::Tunnel { .. } => {
            client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
            client.set_read_timeout(None)?;
            tunnel(client, server, leftover, shared)
        }
        Request::Http { head, .. } => {
            send_first(&mut server, &head, shared, false)?;
            server.write_all(&leftover)?;
            client.set_read_timeout(None)?;
            pipe(client, server)
        }
    }
}

fn read_request(client: &mut TcpStream) -> io::Result<(Request, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::other("request head too large"));
        }
        let n = client.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::other("client closed before sending a request"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let leftover = buf[end..].to_vec();
    let head = &buf[..end];
    let line_end = head.windows(2).position(|w| w == b"\r\n").unwrap_or(0);
    let line = std::str::from_utf8(&head[..line_end]).map_err(|_| io::Error::other("bad request line"))?;
    let mut parts = line.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    if !version.starts_with("HTTP/1.") {
        return Err(io::Error::other("unsupported request"));
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        return Ok((Request::Tunnel { host, port }, leftover));
    }
    let rest = target
        .strip_prefix("http://")
        .ok_or_else(|| io::Error::other("only http:// and CONNECT requests are supported"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_host_port(authority, 80)?;

    let mut out = format!("{method} {path} {version}\r\n").into_bytes();
    let mut has_host = false;
    for header in head[line_end + 2..end - 2].split(|&b| b == b'\n') {
        let header = header.strip_suffix(b"\r").unwrap_or(header);
        if header.is_empty() {
            continue;
        }
        let name = header.split(|&b| b == b':').next().unwrap_or(&[]);
        if name.eq_ignore_ascii_case(b"proxy-connection") || name.eq_ignore_ascii_case(b"connection") {
            continue;
        }
        has_host |= name.eq_ignore_ascii_case(b"host");
        out.extend_from_slice(header);
        out.extend_from_slice(b"\r\n");
    }
    if !has_host {
        out.extend_from_slice(format!("Host: {authority}\r\n").as_bytes());
    }
    // Only the first request is forwarded to this host; ask it to close so
    // later requests on the same client connection are not misrouted.
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    Ok((Request::Http { host, port, head: out }, leftover))
}

fn split_host_port(authority: &str, default: u16) -> io::Result<(String, u16)> {
    let bad = || io::Error::other(format!("bad target {authority:?}"));
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        // [v6]:port or [v6]
        let (host, tail) = rest.split_once(']').ok_or_else(bad)?;
        match tail.strip_prefix(':') {
            Some(p) => (host, p.parse::<u16>().map_err(|_| bad())?),
            None if tail.is_empty() => (host, default),
            None => return Err(bad()),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, p.parse::<u16>().map_err(|_| bad())?),
            _ => (authority, default),
        }
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_ascii_lowercase(), port))
}

fn connect_target(host: &str, port: u16, shared: &Shared) -> io::Result<TcpStream> {
    let covered = shared.cfg.hosts.as_ref().is_none_or(|h| h.matches(host));
    let candidates: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else if covered && !shared.cfg.resolvers.is_empty() {
        let mut last = io::Error::other(format!("cannot resolve {host}"));
        let mut found = None;
        for r in &shared.cfg.resolvers {
            match (r.lookup)(host) {
                Ok(ips) if !ips.is_empty() => {
                    found = Some((ips, &r.name));
                    break;
                }
                Ok(_) => last = io::Error::other(format!("{} has no address for {host}", r.name)),
                Err(e) => last = e,
            }
        }
        let (ips, name) = found.ok_or(last)?;
        shared.stats.dns.fetch_add(1, Ordering::Relaxed);
        if shared.cfg.verbose {
            (shared.log)(format!("DNS {host} -> {name}"));
        }
        ips.into_iter().map(|ip| SocketAddr::new(ip.into(), port)).collect()
    } else {
        (host, port).to_socket_addrs()?.collect()
    };

    if candidates.is_empty() {
        return Err(io::Error::other(format!("cannot resolve {host}")));
    }
    connect_fastest(&candidates)
}

/// Connects to whichever address answers first. An address the provider
/// blackholes costs a quarter second instead of the whole connect timeout.
fn connect_fastest(addrs: &[SocketAddr]) -> io::Result<TcpStream> {
    let addrs = &addrs[..addrs.len().min(MAX_DIALS)];
    if let [only] = addrs {
        return TcpStream::connect_timeout(only, CONNECT_TIMEOUT);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let mut last = io::Error::other("no address could be reached");
    let mut pending = 0;
    for (i, addr) in addrs.iter().copied().enumerate() {
        if i > 0 {
            match rx.recv_timeout(DIAL_STAGGER) {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(e)) => {
                    pending -= 1;
                    last = e;
                }
                Err(_) => {}
            }
        }
        let tx = tx.clone();
        // Losers finish on their own; a late success is dropped (closed)
        // because nobody receives it any more.
        let spawned = std::thread::Builder::new()
            .name("detour-dial".into())
            .spawn(move || drop(tx.send(TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT))));
        match spawned {
            Ok(_) => pending += 1,
            Err(e) => last = e,
        }
    }
    drop(tx);
    while pending > 0 {
        match rx.recv() {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = e,
            Err(_) => break,
        }
        pending -= 1;
    }
    Err(last)
}

/// Writes the first bytes of a connection, split if the strategy covers them.
fn send_first(server: &mut TcpStream, data: &[u8], shared: &Shared, tunnel: bool) -> io::Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    let strategy = &shared.cfg.strategy;
    let sni = crate::tls::find_sni(data);
    let is_tls = tunnel && sni.is_some();
    // Cut points are computed on the original bytes, then shifted to match
    // the extra record header inserted by the TLS record split.
    let record_at = sni
        .as_ref()
        .filter(|_| is_tls)
        .and_then(|sni| strategy.tls_record?.resolve(sni.start, data.len()));
    let covered = strategy.payload_cuts(data, shared.cfg.hosts.as_ref());
    let (host, mut cuts) = match (covered, record_at) {
        (Some(c), _) => c,
        // Only the record split applies: still needs a host-list check.
        (None, Some(_)) => {
            let host = sni.and_then(|r| std::str::from_utf8(&data[r]).ok().map(str::to_owned));
            match host {
                Some(h) if shared.cfg.hosts.as_ref().is_none_or(|l| l.matches(&h)) => (h, Vec::new()),
                _ => return server.write_all(data),
            }
        }
        (None, None) => return server.write_all(data),
    };
    let rewritten;
    let data = match record_at.and_then(|at| crate::tls::split_record(data, at).map(|d| (at, d))) {
        Some((at, split)) => {
            for c in &mut cuts {
                if *c >= at {
                    *c += 5;
                }
            }
            // Keep the two records in separate segments too.
            cuts.push(at);
            cuts.sort_unstable();
            cuts.dedup();
            rewritten = split;
            &rewritten[..]
        }
        None => data,
    };
    if is_tls {
        shared.stats.tls.fetch_add(1, Ordering::Relaxed);
    } else {
        shared.stats.http.fetch_add(1, Ordering::Relaxed);
    }
    if shared.cfg.verbose {
        (shared.log)(format!(
            "{} {host} -> {} pieces",
            if is_tls { "TLS" } else { "HTTP" },
            cuts.len() + 1
        ));
    }
    // A few pieces get a short pause each so they cannot merge into one
    // segment. Hundreds of pieces (chunk mode) go back to back, as SpoofDPI
    // sends them: with Nagle off each write still leaves as its own segment.
    let delay = if cuts.len() < MAX_PAUSED_PIECES { shared.cfg.piece_delay } else { Duration::ZERO };
    let mut start = 0;
    for end in cuts.into_iter().chain([data.len()]) {
        server.write_all(&data[start..end])?;
        if end < data.len() && !delay.is_zero() {
            std::thread::sleep(delay);
        }
        start = end;
    }
    Ok(())
}

fn pipe(client: TcpStream, server: TcpStream) -> io::Result<()> {
    let (mut client_r, mut server_w) = (client.try_clone()?, server.try_clone()?);
    let (mut server_r, mut client_w) = (server, client);
    let upstream = std::thread::Builder::new().name("detour-up".into()).spawn(move || {
        let _ = io::copy(&mut client_r, &mut server_w);
        let _ = server_w.shutdown(Shutdown::Write);
    })?;
    let _ = io::copy(&mut server_r, &mut client_w);
    let _ = client_w.shutdown(Shutdown::Write);
    let _ = upstream.join();
    Ok(())
}

/// Relays a CONNECT tunnel. What the client sends first (the TLS
/// ClientHello) is written split; waiting for it happens on the upload
/// side, so a server that speaks first is relayed at once and a browser that
/// opened the tunnel ahead of time and uses it much later still gets its
/// ClientHello split.
fn tunnel(client: TcpStream, server: TcpStream, leftover: Vec<u8>, shared: &Shared) -> io::Result<()> {
    let (mut client_r, mut server_w) = (client.try_clone()?, server.try_clone()?);
    let (mut server_r, mut client_w) = (server, client);
    std::thread::scope(|scope| {
        let upload = std::thread::Builder::new()
            .name("detour-up".into())
            .spawn_scoped(scope, move || {
                let _ = upload(&mut client_r, &mut server_w, leftover, shared);
                let _ = server_w.shutdown(Shutdown::Write);
            })?;
        let _ = io::copy(&mut server_r, &mut client_w);
        let _ = client_w.shutdown(Shutdown::Write);
        let _ = upload.join();
        Ok(())
    })
}

fn upload(client: &mut TcpStream, server: &mut TcpStream, mut first: Vec<u8>, shared: &Shared) -> io::Result<()> {
    if first.is_empty() {
        let mut buf = vec![0u8; MAX_HEAD];
        let n = client.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        first = buf[..n].to_vec();
    }
    complete_record(client, &mut first);
    send_first(server, &first, shared, true)?;
    io::copy(client, server)?;
    Ok(())
}

/// A ClientHello can arrive in more than one read. Waits briefly for the rest
/// of the first TLS record, so it is split as one piece of data.
fn complete_record(client: &mut TcpStream, data: &mut Vec<u8>) {
    let wanted = |d: &[u8]| (d.len() >= 5 && d[0] == 0x16).then(|| 5 + usize::from(u16::from_be_bytes([d[3], d[4]])));
    let Some(total) = wanted(data) else { return };
    let mut chunk = [0u8; 4096];
    let _ = client.set_read_timeout(Some(RECORD_TIMEOUT));
    while data.len() < total.min(MAX_HEAD) {
        match client.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
        }
    }
    let _ = client.set_read_timeout(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn config(delay_ms: u64) -> ProxyConfig {
        ProxyConfig {
            strategy: Strategy::default(),
            hosts: None,
            resolvers: Vec::new(),
            piece_delay: Duration::from_millis(delay_ms),
            verbose: false,
        }
    }

    fn proxy(cfg: ProxyConfig) -> Proxy {
        Proxy::start(cfg, 0, Arc::new(|_| {})).unwrap()
    }

    /// A server that records every read() until the peer closes, answers
    /// "pong" once `expect` bytes arrived, and returns the reads.
    fn recording_server(expect: usize) -> (u16, JoinHandle<Vec<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reads = Vec::new();
            let mut total = 0;
            let mut buf = [0u8; 4096];
            while total < expect {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
                reads.push(buf[..n].to_vec());
            }
            s.write_all(b"pong").unwrap();
            reads
        });
        (port, handle)
    }

    #[test]
    fn tunnel_splits_client_hello_and_relays_replies() {
        let hello = crate::tls::build_client_hello("discord.com");
        let (port, server) = recording_server(hello.len());
        let mut p = proxy(config(25));

        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).unwrap();
        let mut ok = [0u8; 39];
        c.read_exact(&mut ok).unwrap();
        assert!(ok.starts_with(b"HTTP/1.1 200"));

        c.write_all(&hello).unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).unwrap();
        assert_eq!(&pong, b"pong");

        let reads = server.join().unwrap();
        assert_eq!(reads.concat(), hello, "bytes must arrive intact and in order");
        assert!(reads.len() >= 3, "expected a split ClientHello, got {} read(s)", reads.len());
        assert_eq!(reads[0].len(), 1);
        assert_eq!(p.stats.tls.load(Ordering::Relaxed), 1);
        p.stop();
    }

    #[test]
    fn unlisted_hosts_are_not_split() {
        let hello = crate::tls::build_client_hello("example.org");
        let (port, server) = recording_server(hello.len());
        let mut cfg = config(25);
        cfg.hosts = Some(DomainList::parse("discord.com"));
        let mut p = proxy(cfg);

        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        let mut ok = [0u8; 39];
        c.read_exact(&mut ok).unwrap();
        c.write_all(&hello).unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).unwrap();

        let reads = server.join().unwrap();
        assert_eq!(reads.concat(), hello);
        assert_eq!(reads.len(), 1);
        assert_eq!(p.stats.tls.load(Ordering::Relaxed), 0);
        p.stop();
    }

    #[test]
    fn plain_http_is_rewritten_to_origin_form_and_split_at_the_host() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = received.clone();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap();
                seen.lock().unwrap().push(buf[..n].to_vec());
                if buf[..n].ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi").unwrap();
        });
        let mut p = proxy(config(25));

        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(
            format!("GET http://127.0.0.1:{port}/x?y=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Connection: keep-alive\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).unwrap();
        assert!(reply.ends_with("hi"), "{reply}");
        server.join().unwrap();

        let all: Vec<u8> = received.lock().unwrap().concat();
        let text = String::from_utf8(all).unwrap();
        assert!(text.starts_with("GET /x?y=1 HTTP/1.1\r\n"), "{text}");
        assert!(text.contains(&format!("Host: 127.0.0.1:{port}\r\n")));
        assert!(text.ends_with("Connection: close\r\n\r\n"));
        assert!(!text.to_lowercase().contains("proxy-connection"));
        assert!(received.lock().unwrap().len() >= 2, "head should be split");
        p.stop();
    }

    #[test]
    fn bad_gateway_when_the_target_is_unreachable() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let mut p = proxy(config(1));
        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = c.read_to_string(&mut reply);
        assert!(reply.starts_with("HTTP/1.1 502"), "{reply}");
        // The connection thread counts the error right after replying.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while p.stats.errors.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(p.stats.errors.load(Ordering::Relaxed) >= 1);
        p.stop();
    }

    #[test]
    fn stop_releases_the_port() {
        let mut p = proxy(config(1));
        let addr = p.addr();
        assert!(p.is_running());
        p.stop();
        assert!(!p.is_running());
        assert!(TcpListener::bind(addr).is_ok());
    }

    #[test]
    fn stop_cuts_tunnels_that_are_still_open() {
        // A server that accepts and then just holds the connection open.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf);
            held_tx.send(()).unwrap();
            // Returns when the proxy shuts its side down.
            let n = s.read(&mut buf);
            matches!(n, Ok(0) | Err(_))
        });
        let mut p = proxy(config(1));

        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        let mut ok = [0u8; 39];
        c.read_exact(&mut ok).unwrap();
        c.write_all(b"hello").unwrap();
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        p.stop();

        // The client sees the tunnel die at once, not after a timeout...
        let started = std::time::Instant::now();
        let mut buf = [0u8; 8];
        assert!(matches!(c.read(&mut buf), Ok(0) | Err(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
        // ...and so does the server it was connected to.
        assert!(holder.join().unwrap(), "upstream socket should have been closed");
        assert!(p.shared.sockets.lock().unwrap().live.is_empty());
    }

    fn open_tunnel(p: &Proxy, port: u16) -> TcpStream {
        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        let mut ok = [0u8; 39];
        c.read_exact(&mut ok).unwrap();
        c
    }

    #[test]
    fn a_server_that_speaks_first_is_relayed_without_waiting_for_the_client() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"220 hello").unwrap();
            let mut buf = [0u8; 16];
            let n = s.read(&mut buf).unwrap();
            buf[..n].to_vec()
        });
        let mut p = proxy(config(1));
        let mut c = open_tunnel(&p, port);

        let started = std::time::Instant::now();
        let mut banner = [0u8; 9];
        c.read_exact(&mut banner).unwrap();
        assert_eq!(&banner, b"220 hello");
        assert!(started.elapsed() < Duration::from_secs(2), "banner was held back");

        c.write_all(b"QUIT\r\n").unwrap();
        assert_eq!(server.join().unwrap(), b"QUIT\r\n");
        p.stop();
    }

    #[test]
    fn a_client_hello_sent_long_after_the_tunnel_opened_is_still_split() {
        let hello = crate::tls::build_client_hello("discord.com");
        let (port, server) = recording_server(hello.len());
        let mut p = proxy(config(5));
        let mut c = open_tunnel(&p, port);

        // Browsers open tunnels ahead of time; the handshake can come much later.
        std::thread::sleep(Duration::from_millis(600));
        c.write_all(&hello).unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).unwrap();

        let reads = server.join().unwrap();
        assert_eq!(reads.concat(), hello);
        assert!(reads.len() >= 3, "expected a split ClientHello, got {} read(s)", reads.len());
        p.stop();
    }

    #[test]
    fn a_client_hello_that_arrives_in_two_reads_is_split_as_one_record() {
        let hello = crate::tls::build_client_hello("discord.com");
        let (port, server) = recording_server(hello.len() + 5);
        let mut cfg = config(5);
        cfg.strategy.tls_record = Some("sni+1".parse().unwrap());
        let mut p = proxy(cfg);
        let mut c = open_tunnel(&p, port);

        let mid = hello.len() / 2;
        c.write_all(&hello[..mid]).unwrap();
        c.flush().unwrap();
        std::thread::sleep(Duration::from_millis(60));
        c.write_all(&hello[mid..]).unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).unwrap();

        let reads = server.join().unwrap();
        let got = reads.concat();
        assert_eq!(got.len(), hello.len() + 5, "one extra TLS record header expected");
        // Two TLS records now start the stream: [0x16, ver, len] twice.
        let first_len = usize::from(u16::from_be_bytes([got[3], got[4]]));
        assert_eq!(got[5 + first_len], 0x16, "second record header must follow the first");
        p.stop();
    }

    #[test]
    fn next_resolver_is_asked_when_one_gives_no_address() {
        let (port, server) = recording_server(5);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut cfg = config(1);
        // Nothing listens on this UDP port, so the first lookup fails.
        let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let dead_addr = match dead.local_addr().unwrap() {
            SocketAddr::V4(a) => a,
            SocketAddr::V6(_) => unreachable!(),
        };
        drop(dead);
        let seen = asked.clone();
        cfg.resolvers = vec![
            Resolver::udp(dead_addr),
            Resolver {
                name: "second".into(),
                lookup: Arc::new(move |name: &str| {
                    seen.lock().unwrap().push(name.to_owned());
                    Ok(vec![Ipv4Addr::LOCALHOST])
                }),
            },
        ];
        let mut p = proxy(cfg);

        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(format!("CONNECT steered.example:{port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        let mut ok = [0u8; 39];
        c.read_exact(&mut ok).unwrap();
        assert!(ok.starts_with(b"HTTP/1.1 200"));
        c.write_all(b"hello").unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).unwrap();
        assert_eq!(&pong, b"pong");
        assert_eq!(server.join().unwrap().concat(), b"hello");
        assert_eq!(*asked.lock().unwrap(), ["steered.example"]);
        p.stop();
    }

    #[test]
    fn never_falls_back_to_the_system_resolver() {
        // "localhost" resolves through the system, so reaching the target
        // would mean the proxy fell back to it.
        let (port, _server) = recording_server(1);
        let mut cfg = config(1);
        cfg.resolvers = vec![Resolver {
            name: "down".into(),
            lookup: Arc::new(|_: &str| Err(io::Error::other("unreachable"))),
        }];
        let mut p = proxy(cfg);
        let mut c = TcpStream::connect(p.addr()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = c.read_to_string(&mut reply);
        assert!(reply.starts_with("HTTP/1.1 502"), "{reply}");
        p.stop();
    }

    #[test]
    fn chunk_mode_relays_the_whole_hello_intact() {
        let hello = crate::tls::build_client_hello("discord.com");
        let (port, server) = recording_server(hello.len());
        let mut cfg = config(0);
        cfg.strategy.stream = crate::StreamSplit::Chunk(1);
        let mut p = proxy(cfg);
        let mut c = open_tunnel(&p, port);
        c.write_all(&hello).unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).unwrap();
        assert_eq!(server.join().unwrap().concat(), hello);
        assert_eq!(p.stats.tls.load(Ordering::Relaxed), 1);
        p.stop();
    }

    #[test]
    fn parses_targets() {
        assert_eq!(split_host_port("Example.com:8443", 443).unwrap(), ("example.com".into(), 8443));
        assert_eq!(split_host_port("example.com", 80).unwrap(), ("example.com".into(), 80));
        assert_eq!(split_host_port("[::1]:8443", 443).unwrap(), ("::1".into(), 8443));
        assert_eq!(split_host_port("[::1]", 443).unwrap(), ("::1".into(), 443));
        assert!(split_host_port("[::1", 443).is_err());
        assert!(split_host_port("host:notaport", 80).is_err());
        assert!(split_host_port("", 80).is_err());
    }
}
