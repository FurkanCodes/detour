//! DNS-over-HTTPS bridge. HTTPS runs on bounded workers, never on the packet loop.
use crate::divert::{Address, Handle};
use crate::dns::DnsRedirect;
use std::sync::Mutex;
use std::time::Instant;
use crate::packet::{self, IpVersion};
use crate::runtime::{LogFn, Stats};
use crossbeam_channel::{bounded, Sender};
use detour_core::DomainList;
use std::ffi::c_void;
use std::io;
use std::net::Ipv4Addr;
use std::ptr::{null, null_mut};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use windows_sys::Win32::Networking::WinHttp::*;

/// The last WinHTTP error. Windows cannot format these codes itself.
fn last_error() -> io::Error {
    let code = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let text = match code {
        12002 => "timed out",
        12007 => "server name not resolved",
        12029 => "cannot connect",
        12030 => "connection dropped",
        12175 => "TLS certificate or handshake failed",
        12038 | 12037 | 12045 => "certificate rejected",
        _ => return io::Error::other(format!("WinHTTP error {code}")),
    };
    io::Error::other(format!("{text} (WinHTTP {code})"))
}

struct Internet(*mut c_void);
impl Internet {
    fn checked(handle: *mut c_void) -> io::Result<Self> {
        if handle.is_null() { Err(last_error()) } else { Ok(Self(handle)) }
    }
}
impl Drop for Internet {
    fn drop(&mut self) { unsafe { WinHttpCloseHandle(self.0); } }
}
// Handles have a single owner and may be transferred between WinHTTP threads.
unsafe impl Send for Internet {}
fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain([0]).collect() }
fn checked(ok: i32) -> io::Result<()> {
    if ok == 0 { Err(last_error()) } else { Ok(()) }
}

pub struct Client {
    connection: Internet,
    _session: Internet,
}
impl Client {
    pub fn new(server: Ipv4Addr) -> io::Result<Self> {
        let agent = wide("Detour/0.2");
        let session = Internet::checked(unsafe {
            WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, null(), null(), 0)
        })?;
        checked(unsafe { WinHttpSetTimeouts(session.0, 2000, 3000, 3000, 5000) })?;
        let host = wide(&server.to_string());
        let connection = Internet::checked(unsafe { WinHttpConnect(session.0, host.as_ptr(), 443, 0) })?;
        Ok(Self { connection, _session: session })
    }

    pub fn query(&self, query: &[u8]) -> io::Result<Vec<u8>> {
        if query.len() < 12 || query.len() > 4096 { return Err(io::Error::other("invalid DNS query size")); }
        let method = wide("POST");
        let path = wide("/dns-query");
        let request = Internet::checked(unsafe {
            WinHttpOpenRequest(self.connection.0, method.as_ptr(), path.as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE)
        })?;
        let headers = wide("Content-Type: application/dns-message\r\nAccept: application/dns-message\r\n");
        checked(unsafe {
            WinHttpSendRequest(request.0, headers.as_ptr(), (headers.len() - 1) as u32,
                query.as_ptr().cast(), query.len() as u32, query.len() as u32, 0)
        })?;
        checked(unsafe { WinHttpReceiveResponse(request.0, null_mut()) })?;
        let mut status = 0u32;
        let mut size = 4u32;
        checked(unsafe {
            WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(), (&mut status as *mut u32).cast(), &mut size, null_mut())
        })?;
        if status != 200 { return Err(io::Error::other(format!("encrypted DNS returned HTTP {status}"))); }
        let mut result = Vec::new();
        loop {
            let mut buffer = [0u8; 4096];
            let mut read = 0;
            checked(unsafe { WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), buffer.len() as u32, &mut read) })?;
            if read == 0 { break; }
            if result.len() + read as usize > 65000 { return Err(io::Error::other("oversized DNS response")); }
            result.extend_from_slice(&buffer[..read as usize]);
        }
        if result.len() < 12 || result[..2] != query[..2] || result[2] & 0x80 == 0 {
            return Err(io::Error::other("invalid DNS-over-HTTPS response"));
        }
        Ok(result)
    }
}

impl Client {
    /// The IPv4 addresses of `name`, looked up over HTTPS.
    pub fn resolve_a(&self, name: &str) -> io::Result<Vec<Ipv4Addr>> {
        static NEXT_ID: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0x5a00);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let answer = self.query(&crate::dnsclient::build_query(id, name)?)?;
        crate::dnsclient::parse_a_records(&answer, id)
    }
}

/// A minimal A query for `example.com`.
fn probe_query() -> Vec<u8> {
    let mut q = vec![0x4b, 0x4c, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["example", "com"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    q
}

/// Checks that `server` answers DNS-over-HTTPS before the engine relies on it.
pub fn probe(server: Ipv4Addr) -> io::Result<()> {
    Client::new(server)?.query(&probe_query()).map(drop)
}

struct Job { packet: Vec<u8>, address: Address }

pub struct PoolContext {
    pub handle: Arc<Handle>,
    pub server: Ipv4Addr,
    /// Plain redirect used when an HTTPS lookup fails.
    pub fallback: Option<Arc<Mutex<DnsRedirect>>>,
    pub stats: Arc<Stats>,
    pub log: LogFn,
    pub verbose: bool,
}

pub struct Pool { sender: Sender<Job>, stopped: Arc<AtomicBool> }
impl Pool {
    pub fn new(ctx: PoolContext) -> io::Result<Self> {
        let PoolContext { handle, server, fallback, stats, log, verbose } = ctx;
        let (sender, receiver) = bounded::<Job>(128);
        let stopped = Arc::new(AtomicBool::new(false));
        for index in 0..4 {
            let client = Client::new(server)?;
            let (rx, stop, handle, stats, log, fallback) = (receiver.clone(), stopped.clone(), handle.clone(), stats.clone(), log.clone(), fallback.clone());
            std::thread::Builder::new().name(format!("detour-dns-{index}")).spawn(move || {
                while let Ok(job) = rx.recv() {
                    if stop.load(Ordering::Acquire) { break; }
                    let Some(parsed) = packet::parse(&job.packet) else { continue; };
                    let query = parsed.payload(&job.packet);
                    let response = match client.query(query) {
                        Ok(answer) => {
                            stats.dns.fetch_add(1, Ordering::Relaxed);
                            if verbose {
                                if let Some((_, name)) = crate::dns::parse_query(query) { log(format!("Encrypted DNS {name}")); }
                            }
                            answer
                        }
                        Err(error) => {
                            let count = stats.errors.fetch_add(1, Ordering::Relaxed) + 1;
                            if count == 1 || count.is_multiple_of(20) { log(format!("Encrypted DNS failed: {error}")); }
                            // Prefer the plain redirect; only answer SERVFAIL
                            // when there is none, never the ISP's poisoned reply.
                            if let Some(dns) = &fallback {
                                let mut redirected = job.packet.clone();
                                let rewritten = dns
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .rewrite_query(&mut redirected, None, Instant::now())
                                    .is_some();
                                if rewritten {
                                    let mut address = job.address;
                                    if let Err(error) = handle.send(&mut redirected, &mut address) {
                                        if !stop.load(Ordering::Acquire) { log(format!("DNS fallback failed: {error}")); }
                                    }
                                    continue;
                                }
                            }
                            let mut answer = query.to_vec();
                            answer[2] = (answer[2] & 1) | 0x80;
                            answer[3] = 0x82; // SERVFAIL, never silently fall back to poisoned UDP.
                            answer[6..10].fill(0);
                            answer
                        }
                    };
                    if stop.load(Ordering::Acquire) { break; }
                    if let Some(mut packet) = response_packet(&job.packet, &response) {
                        let mut address = job.address;
                        address.set_inbound();
                        if let Err(error) = handle.send(&mut packet, &mut address) {
                            if !stop.load(Ordering::Acquire) { log(format!("DNS response failed: {error}")); }
                        }
                    }
                }
            })?;
        }
        Ok(Self { sender, stopped })
    }

    /// Queues an outbound DNS query. False means the caller handles the
    /// packet: not DNS, host not covered, or the queue is full.
    pub fn submit(&self, packet: &[u8], parsed: &packet::Parsed, address: Address, hosts: Option<&DomainList>) -> bool {
        if parsed.protocol != packet::UDP || parsed.dst_port(packet) != 53 { return false; }
        let query = parsed.payload(packet);
        if query.len() > 4096 { return false; }
        let Some((_, name)) = crate::dns::parse_query(query) else { return false; };
        if hosts.is_some_and(|h| !h.matches(&name)) { return false; }
        self.sender.try_send(Job { packet: packet.to_vec(), address }).is_ok()
    }
}
impl Drop for Pool {
    fn drop(&mut self) { self.stopped.store(true, Ordering::Release); }
}

/// Return the answer using the resolver address and client port the OS expects.
fn response_packet(query: &[u8], answer: &[u8]) -> Option<Vec<u8>> {
    let p = packet::parse(query)?;
    if p.protocol != packet::UDP || answer.len() < 12 || p.payload_off + answer.len() > 65535 { return None; }
    let mut out = query[..p.payload_off].to_vec();
    out.extend_from_slice(answer);
    let len = out.len();
    match p.version {
        IpVersion::V4 => {
            out[12..20].rotate_left(4);
            packet::put16(&mut out, 2, len as u16);
        }
        IpVersion::V6 => {
            out[8..40].rotate_left(16);
            packet::put16(&mut out, 4, (len - 40) as u16);
        }
    }
    out[p.ip_hdr_len..p.ip_hdr_len + 4].rotate_left(2);
    packet::put16(&mut out, p.ip_hdr_len + 4, (8 + answer.len()) as u16);
    Some(out)
}

#[link(name = "dnsapi")]
extern "system" { fn DnsFlushResolverCache() -> i32; }
pub fn flush_cache() { unsafe { DnsFlushResolverCache(); } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::testutil::*;
    #[test]
    fn probe_query_is_a_valid_dns_question() {
        assert_eq!(crate::dns::parse_query(&probe_query()), Some((0x4b4c, "example.com".into())));
    }

    #[test]
    fn replies_preserve_ipv4_and_ipv6_client_endpoints() {
        let answer = [0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        let payload = udp(42000, 53, &[0; 12]);
        for request in [ipv4(17, [10, 0, 0, 2], [192, 168, 1, 1], &payload), ipv6(17, &payload)] {
            let response = response_packet(&request, &answer).unwrap();
            let p = packet::parse(&response).unwrap();
            assert_eq!(p.src_port(&response), 53);
            assert_eq!(p.dst_port(&response), 42000);
            assert_eq!(p.payload(&response), answer);
            assert_eq!(packet::be16(&response, p.ip_hdr_len + 4), 20);
            match p.version {
                IpVersion::V4 => { assert_eq!(&response[12..16], &request[16..20]); assert_eq!(&response[16..20], &request[12..16]); }
                IpVersion::V6 => { assert_eq!(&response[8..24], &request[24..40]); assert_eq!(&response[24..40], &request[8..24]); }
            }
        }
    }
}
