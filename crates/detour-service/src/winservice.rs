//! Hosts the supervisor inside the Windows Service Control Manager.

use crate::paths::SERVICE_NAME;
use crate::runner::{self, Launch, Outcome};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;
use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_dispatcher;

static LAUNCH: OnceLock<Launch> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Blocks until the service stops. Fails if not started by the SCM.
pub fn run(launch: Launch) -> Result<(), windows_service::Error> {
    let _ = LAUNCH.set(launch);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_args: Vec<OsString>) {
    let launch = LAUNCH.get().expect("launch set before dispatch").clone();
    let log_path = launch
        .config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("run")
        .join("service.log");
    let log = move |msg: &str| append_log(&log_path, msg);

    if let Err(e) = host(&launch, &log) {
        log(&format!("service failed: {e}"));
    }
}

fn host(
    launch: &Launch,
    log: &(impl Fn(&str) + Clone + Send + 'static),
) -> Result<(), windows_service::Error> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handler = move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    report(
        &status,
        ServiceState::StartPending,
        ServiceExitCode::Win32(0),
    )?;

    report(&status, ServiceState::Running, ServiceExitCode::Win32(0))?;
    let result = runner::run(launch, &stop_rx, Box::new(log.clone()));
    let exit = match result {
        Ok(Outcome::Disabled) => {
            // Stay installed but idle until stopped.
            let _ = stop_rx.recv();
            ServiceExitCode::Win32(0)
        }
        Ok(Outcome::Stopped) => ServiceExitCode::Win32(0),
        Err(e) => {
            log(&format!("{e}"));
            ServiceExitCode::ServiceSpecific(1)
        }
    };
    report(&status, ServiceState::Stopped, exit)
}

fn report(
    handle: &ServiceStatusHandle,
    state: ServiceState,
    exit_code: ServiceExitCode,
) -> Result<(), windows_service::Error> {
    let running = state == ServiceState::Running;
    handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code,
        checkpoint: 0,
        wait_hint: Duration::from_secs(if state == ServiceState::StartPending {
            10
        } else {
            0
        }),
        process_id: None,
    })
}

fn append_log(path: &PathBuf, msg: &str) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{}] {msg}", utc(secs));
    }
}

/// `YYYY-MM-DD HH:MM:SSZ` from Unix seconds (Hinnant's civil-from-days).
fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc;

    #[test]
    fn formats_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13:20Z");
        assert_eq!(utc(951_782_400 + 86_399), "2000-02-29 23:59:59Z");
    }
}
