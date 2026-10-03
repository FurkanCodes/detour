//! `detour-engine`: the WinDivert packet engine as a command-line tool.

#[cfg(windows)]
#[path = "main_windows.rs"]
mod windows;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "detour-engine drives the WinDivert packet driver and only runs on Windows. \
         On other systems use the Detour app, which runs a local proxy instead."
    );
    std::process::exit(1);
}
