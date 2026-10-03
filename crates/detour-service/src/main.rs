use detour_service::paths;
use detour_service::{runner, Launch, Outcome};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

const USAGE: &str = "\
usage: detour-service [--config PATH] [--install-dir PATH] [--service]

  --config PATH       detour.toml (default: %ProgramData%\\Detour\\detour.toml)
  --install-dir PATH  directory engine paths are relative to (default: this exe's folder)
  --service           run under the Windows service manager (used by `detour install`)

Without --service it runs in the foreground; press Ctrl+C to stop.";

static STOP: OnceLock<Mutex<Sender<()>>> = OnceLock::new();

#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

extern "system" fn on_ctrl(_event: u32) -> i32 {
    if let Some(tx) = STOP.get().and_then(|m| m.lock().ok()) {
        let _ = tx.send(());
    }
    1
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("detour: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut config_path = None;
    let mut install_dir = None;
    let mut service = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                config_path = Some(PathBuf::from(args.next().ok_or("--config needs a path")?))
            }
            Some("--install-dir") => {
                install_dir = Some(PathBuf::from(
                    args.next().ok_or("--install-dir needs a path")?,
                ))
            }
            Some("--service") => service = true,
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => return Err(format!("unexpected argument {arg:?}\n{USAGE}").into()),
        }
    }

    let config_path = match config_path {
        Some(p) => p,
        None => paths::config_file(&paths::default_data_dir().ok_or("ProgramData is not set")?),
    };
    let install_dir = match install_dir {
        Some(d) => d,
        None => std::env::current_exe()?
            .parent()
            .ok_or("cannot locate install directory")?
            .to_owned(),
    };
    let launch = Launch {
        config_path,
        install_dir,
    };

    if service {
        detour_service::winservice::run(launch)?;
        return Ok(());
    }

    let (stop_tx, stop_rx) = mpsc::channel();
    let _ = STOP.set(Mutex::new(stop_tx));
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    let outcome = runner::run(&launch, &stop_rx, Box::new(|m| eprintln!("detour: {m}")))?;
    if outcome == Outcome::Stopped {
        eprintln!("detour: stopped");
    }
    Ok(())
}
