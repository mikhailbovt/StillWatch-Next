use std::{ffi::OsString, sync::OnceLock, time::Duration};

use anyhow::{Result, anyhow};
use tokio::sync::watch;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

use crate::{Config, runtime, serve};

const NAME: &str = "StillWatchCore";
static CONFIG: OnceLock<Config> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

pub fn start(config: Config) -> Result<()> {
    CONFIG
        .set(config)
        .map_err(|_| anyhow!("service configuration already installed"))?;
    service_dispatcher::start(NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_: Vec<OsString>) {
    if let Err(error) = run() {
        eprintln!("StillWatch service failed: {error:#}");
    }
}

fn run() -> Result<()> {
    let config = CONFIG
        .get()
        .ok_or_else(|| anyhow!("service configuration unavailable"))?
        .clone();
    let (stop, shutdown) = watch::channel(false);
    let handle = service_control_handler::register(NAME, move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop.send_replace(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    status(handle, ServiceState::StartPending, 1, false)?;
    let result = runtime().and_then(|runtime| runtime.block_on(async move {
        let (ready, mut startup) = watch::channel(false);
        let reporter = tokio::spawn(async move {
            let mut checkpoint: u32 = 2;
            loop {
                tokio::select! {
                    _ = startup.changed() => break,
                    _ = tokio::time::sleep(Duration::from_secs(4)) => {
                        if status(handle, ServiceState::StartPending, checkpoint, false).is_err() { break; }
                        checkpoint = checkpoint.saturating_add(1);
                    }
                }
            }
        });
        let result = serve(config, shutdown, move || {
            ready.send_replace(true);
            status(handle, ServiceState::Running, 0, false)
        }, move || {
            let _ = status(handle, ServiceState::StopPending, 1, false);
        }).await;
        reporter.abort();
        result
    }));
    status(handle, ServiceState::Stopped, 0, result.is_err())?;
    result
}

fn status(
    handle: ServiceStatusHandle,
    state: ServiceState,
    checkpoint: u32,
    failed: bool,
) -> Result<()> {
    let pending = matches!(
        state,
        ServiceState::StartPending | ServiceState::StopPending
    );
    handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(if failed { 1 } else { 0 }),
        checkpoint,
        wait_hint: if pending {
            Duration::from_secs(15)
        } else {
            Duration::ZERO
        },
        process_id: None,
    })?;
    Ok(())
}
