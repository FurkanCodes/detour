use detour_core::DomainList;
use detour_engine::divert::{Api, Handle};
use detour_engine::options::USAGE;
use detour_engine::runtime::Engine;
use detour_engine::Options;
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};

static ACTIVE: OnceLock<Arc<Handle>> = OnceLock::new();

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

extern "system" fn on_ctrl(_event: u32) -> i32 {
    if let Some(h) = ACTIVE.get() {
        h.shutdown();
    }
    1
}

pub fn main() -> ExitCode {
    let opts = match Options::parse(std::env::args().skip(1)) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("detour-engine: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("detour-engine: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(opts: Options) -> Result<(), String> {
    let hosts = match &opts.hostlist {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read hostlist {}: {e}", path.display()))?;
            let list = DomainList::parse(&text);
            if list.is_empty() {
                return Err(format!("hostlist {} has no valid domains", path.display()));
            }
            Some(list)
        }
        None => None,
    };

    let dll = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("WinDivert.dll");
    let api = Api::load(&dll).map_err(|e| e.to_string())?;

    let summary = format!(
        "running ({} hosts, ports {:?}{})",
        hosts.as_ref().map_or(0, DomainList::len),
        opts.ports,
        opts.dns_redirect
            .map(|r| format!(", dns -> {r}"))
            .unwrap_or_default()
    );
    let mut engine = Engine::start(&api, opts, hosts, Arc::new(|line| eprintln!("{line}")))?;
    let _ = ACTIVE.set(engine.handle());
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    eprintln!("detour-engine: {summary}");
    engine.join()
}
