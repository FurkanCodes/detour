//! Windows service management for Detour.

use crate::data::Ctx;
use detour_service::paths::SERVICE_NAME;
use std::error::Error;
use std::ffi::OsString;
use std::time::{Duration, Instant};
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const WAIT: Duration = Duration::from_secs(20);

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager> {
    ServiceManager::local_computer(None::<&str>, access).map_err(friendly)
}

fn open(access: ServiceAccess) -> Result<Option<Service>> {
    let m = manager(ServiceManagerAccess::CONNECT)?;
    match m.open_service(SERVICE_NAME, access) {
        Ok(s) => Ok(Some(s)),
        Err(e) if os_code(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => Ok(None),
        Err(e) => Err(friendly(e)),
    }
}

fn os_code(e: &windows_service::Error) -> Option<i32> {
    match e {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

fn friendly(e: windows_service::Error) -> Box<dyn Error> {
    if os_code(&e) == Some(ERROR_ACCESS_DENIED) {
        "access denied: run this command from an elevated (Administrator) terminal".into()
    } else {
        Box::new(e)
    }
}

pub fn can_manage() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE).is_ok()
}

/// `None` if the service is not installed.
pub fn state() -> Result<Option<ServiceState>> {
    match open(ServiceAccess::QUERY_STATUS)? {
        Some(s) => Ok(Some(s.query_status().map_err(friendly)?.current_state)),
        None => Ok(None),
    }
}

pub fn state_name(s: ServiceState) -> &'static str {
    match s {
        ServiceState::Stopped => "stopped",
        ServiceState::StartPending => "starting",
        ServiceState::StopPending => "stopping",
        ServiceState::Running => "running",
        ServiceState::ContinuePending => "resuming",
        ServiceState::PausePending => "pausing",
        ServiceState::Paused => "paused",
    }
}

pub fn install(ctx: &Ctx) -> Result<()> {
    let exe = ctx.install_dir.join("detour-service.exe");
    if !exe.is_file() {
        return Err(format!("{} not found", exe.display()).into());
    }
    let m = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: "Detour".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec![
            "--service".into(),
            "--config".into(),
            ctx.config_path().into_os_string(),
            "--install-dir".into(),
            ctx.install_dir.clone().into_os_string(),
        ],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let service = m
        .create_service(
            &info,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|e| match os_code(&e) {
            Some(1073) => "the service is already installed (run `detour uninstall` first)".into(),
            _ => friendly(e),
        })?;
    service.set_description("Bypasses DPI-based blocking of selected sites.")?;
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(10),
            };
            3
        ]),
    })?;
    Ok(())
}

pub fn start() -> Result<()> {
    let s = open(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?
        .ok_or("service is not installed (run `detour install`)")?;
    if s.query_status()?.current_state == ServiceState::Running {
        return Ok(());
    }
    s.start::<OsString>(&[]).map_err(friendly)?;
    wait_for(&s, ServiceState::Running)
}

pub fn stop() -> Result<()> {
    let Some(s) = open(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)? else {
        return Ok(());
    };
    if s.query_status()?.current_state != ServiceState::Stopped {
        let _ = s.stop();
        wait_for(&s, ServiceState::Stopped)?;
    }
    Ok(())
}

pub fn uninstall() -> Result<bool> {
    stop()?;
    let Some(s) = open(ServiceAccess::DELETE)? else {
        return Ok(false);
    };
    s.delete().map_err(friendly)?;
    Ok(true)
}

fn wait_for(s: &Service, want: ServiceState) -> Result<()> {
    let deadline = Instant::now() + WAIT;
    loop {
        let st = s.query_status()?.current_state;
        if st == want {
            return Ok(());
        }
        if want == ServiceState::Running && st == ServiceState::Stopped {
            return Err("the service stopped right after starting; see run\\service.log and run\\engine.log in the data folder".into());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "timed out waiting for the service to be {}",
                state_name(want)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
