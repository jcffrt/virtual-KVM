//! Windows service plumbing (runs as LocalSystem).

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Notify;
use tracing::{error, info};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "RawKvmAgent";

define_windows_service!(ffi_service_main, service_main);

/// Hand the process to the service control manager. Returns when the
/// service has stopped.
pub fn dispatch() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        error!("service failed: {e:#}");
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}

fn run_service() -> Result<()> {
    let stop = Arc::new(Notify::new());
    let on_stop = stop.clone();
    let handle = service_control_handler::register(SERVICE_NAME, move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
            // notify_one stores a permit, so a stop that comes early is kept.
            on_stop.notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
    ))?;
    info!("service running");

    let result = crate::run_agent(async move { stop.notified().await });
    let code = if result.is_ok() { 0 } else { 1 };
    if let Err(e) = &result {
        error!("agent stopped: {e:#}");
    }
    handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        code,
    ))?;
    result
}
