//! Runs the proxy engine in the foreground: `proxy_demo PORT [engine options]`,
//! then `curl --proxy http://127.0.0.1:PORT https://example.com`.
//!
//! Like the Windows app, `--doh=IP` asks that server over HTTPS before the
//! plain resolver (`--dns-redirect`).

use detour_engine::proxy::{Proxy, ProxyConfig};
use detour_engine::Options;
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8899);
    let opts = Options::parse(args)
        .expect("bad options")
        .expect("no help here");
    let mut cfg = ProxyConfig::from_options(&opts, None);
    #[cfg(windows)]
    if let Some(server) = opts.doh {
        let client = Arc::new(detour_engine::doh::Client::new(server).expect("encrypted DNS"));
        cfg.resolvers.insert(
            0,
            detour_engine::proxy::Resolver {
                name: format!("https://{server}"),
                lookup: Arc::new(move |name| client.resolve_a(name)),
            },
        );
    }
    let proxy = Proxy::start(cfg, port, Arc::new(|line| eprintln!("{line}"))).expect("start");
    eprintln!("proxy listening on {}", proxy.addr());
    while proxy.is_running() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
