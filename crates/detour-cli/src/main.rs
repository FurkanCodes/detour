mod data;
mod svc;

use data::Ctx;
use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;
use windows_service::service::ServiceState;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
detour - bypass DPI-based blocking of selected sites

usage: detour [--data-dir PATH] <command>

  install                 set up the data folder, install the Windows service and start it
  uninstall               stop and remove the service (your data folder is kept)
  start | stop | restart  control the service
  status                  show service state and the active preset
  doctor                  check that everything needed is in place

  preset list             show available presets
  preset use <name>       switch preset (restarts the service if running)

  list show [name]        show a domain list (default: custom)
  list add <domain> [--list name]
  list remove <domain> [--list name]

Service commands need an elevated (Administrator) terminal.
Data folder: %ProgramData%\\Detour (override with --data-dir).";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("detour: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut data_dir = None;
    if let Some(i) = args.iter().position(|a| a == "--data-dir") {
        if i + 1 >= args.len() {
            return Err("--data-dir needs a path".into());
        }
        data_dir = Some(PathBuf::from(args.remove(i + 1)));
        args.remove(i);
    }
    let ctx = Ctx {
        data_dir: match data_dir {
            Some(d) => d,
            None => detour_service::paths::default_data_dir().ok_or("ProgramData is not set")?,
        },
        install_dir: std::env::current_exe()?
            .parent()
            .ok_or("cannot locate install directory")?
            .to_owned(),
    };

    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["help" | "-h" | "--help"] => println!("{USAGE}"),
        ["install"] => install(&ctx)?,
        ["uninstall"] => {
            if svc::uninstall()? {
                println!("service removed (data kept in {})", ctx.data_dir.display());
            } else {
                println!("service was not installed");
            }
        }
        ["start"] => {
            svc::start()?;
            println!("running");
        }
        ["stop"] => {
            svc::stop()?;
            println!("stopped");
        }
        ["restart"] => {
            svc::stop()?;
            svc::start()?;
            println!("running");
        }
        ["status"] => status(&ctx)?,
        ["doctor"] => return doctor(&ctx),
        ["preset", "list"] => preset_list(&ctx)?,
        ["preset", "use", name] => {
            let p = data::use_preset(&ctx, name)?;
            println!("preset set to {name:?} ({})", p.name);
            restart_if_running()?;
        }
        ["list", "show"] => list_show(&ctx, "custom")?,
        ["list", "show", name] => list_show(&ctx, name)?,
        ["list", op @ ("add" | "remove"), rest @ ..] => list_edit(&ctx, op, rest)?,
        other => return Err(format!("unknown command {:?}\n\n{USAGE}", other.join(" ")).into()),
    }
    Ok(())
}

fn install(ctx: &Ctx) -> Result<()> {
    for f in data::ensure_data_dir(ctx)? {
        println!("created {}", f.display());
    }
    let cfg = ctx.config()?;
    let preset = detour_core::Preset::load(&cfg.preset_path(&ctx.data_dir))
        .map_err(|e| format!("{e}\nrun `detour preset list` and `detour preset use <name>`"))?;
    svc::install(ctx)?;
    println!("service installed (starts automatically with Windows)");
    svc::start()?;
    println!("running with preset {:?}", preset.name);
    Ok(())
}

fn status(ctx: &Ctx) -> Result<()> {
    match svc::state()? {
        Some(s) => println!("service: {}", svc::state_name(s)),
        None => println!("service: not installed"),
    }
    match ctx.config() {
        Ok(cfg) => {
            let name = detour_core::Preset::load(&cfg.preset_path(&ctx.data_dir))
                .map(|p| p.name)
                .unwrap_or_else(|e| format!("({e})"));
            println!("preset:  {} - {name}", cfg.preset);
            println!("data:    {}", ctx.data_dir.display());
        }
        Err(_) => println!("data:    not set up yet (run `detour install`)"),
    }
    Ok(())
}

fn doctor(ctx: &Ctx) -> Result<()> {
    let mut problems = 0;
    let mut check = |ok: bool, what: String, fix: &str| {
        println!("[{}] {what}", if ok { "ok" } else { "!!" });
        if !ok {
            println!("     -> {fix}");
            problems += 1;
        }
    };
    for f in [
        "detour-service.exe",
        "detour-engine.exe",
        "WinDivert.dll",
        "WinDivert64.sys",
    ] {
        check(
            data::file_exists(&ctx.install_dir.join(f)),
            format!("{f} in {}", ctx.install_dir.display()),
            "keep all files from the release folder together",
        );
    }
    check(
        svc::can_manage(),
        "administrator rights".into(),
        "open an elevated terminal",
    );

    match ctx.config() {
        Err(e) => check(false, format!("config: {e}"), "run `detour install`"),
        Ok(cfg) => {
            check(true, format!("config {}", ctx.config_path().display()), "");
            match detour_core::Preset::load(&cfg.preset_path(&ctx.data_dir)) {
                Err(e) => check(false, format!("preset: {e}"), "run `detour preset list`"),
                Ok(p) => {
                    check(true, format!("preset {:?}", p.name), "");
                    check(
                        data::file_exists(&ctx.install_dir.join(&p.engine.path)),
                        format!("engine {}", p.engine.path.display()),
                        "the preset points to a missing engine",
                    );
                    for l in &p.lists {
                        let ok =
                            data::read_list(ctx, l).is_ok_and(|d| !d.is_empty() || l == "custom");
                        check(
                            ok,
                            format!("list {l:?}"),
                            "restore it from the release folder's lists/",
                        );
                    }
                }
            }
        }
    }
    if problems == 0 {
        println!("\nall good");
        Ok(())
    } else {
        Err(format!("{problems} problem(s) found").into())
    }
}

fn preset_list(ctx: &Ctx) -> Result<()> {
    let active = ctx.config()?.preset;
    for p in data::presets(ctx)? {
        let mark = if p.stem == active { "*" } else { " " };
        match p.preset {
            Ok(preset) => println!(
                "{mark} {:<14} {}\n  {:<14} {}",
                p.stem, preset.name, "", preset.description
            ),
            Err(e) => println!("{mark} {:<14} (invalid: {e})", p.stem),
        }
    }
    Ok(())
}

fn list_show(ctx: &Ctx, name: &str) -> Result<()> {
    let list = data::read_list(ctx, name)?;
    for d in list.sorted() {
        println!("{d}");
    }
    eprintln!("{} domain(s)", list.len());
    Ok(())
}

fn list_edit(ctx: &Ctx, op: &str, rest: &[&str]) -> Result<()> {
    let (domain, list) = match rest {
        [d] => (*d, "custom"),
        [d, "--list", l] | ["--list", l, d] => (*d, *l),
        _ => return Err(format!("usage: detour list {op} <domain> [--list name]").into()),
    };
    if op == "add" {
        let (d, added) = data::add_domain(ctx, list, domain)?;
        println!(
            "{}",
            if added {
                format!("added {d} to {list}")
            } else {
                format!("{d} is already in {list}")
            }
        );
        if added {
            restart_if_running()?;
        }
    } else {
        let (d, removed) = data::remove_domain(ctx, list, domain)?;
        println!(
            "{}",
            if removed {
                format!("removed {d} from {list}")
            } else {
                format!("{d} is not in {list}")
            }
        );
        if removed {
            restart_if_running()?;
        }
    }
    Ok(())
}

/// The engine reads its hostlist at startup, so changes need a restart.
fn restart_if_running() -> Result<()> {
    if svc::state().ok().flatten() == Some(ServiceState::Running) {
        svc::stop()?;
        svc::start()?;
        println!("service restarted to apply the change");
    }
    Ok(())
}
