use anyhow::{Context, Result};
use std::env;
use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tasktui_core::{
    AdminCommand, AdminResult, ProcessAction, ApiResponse, ServiceAction,
    TasktuiError, validate_api_version, validate_service_name,
};
use tasktui_platform_windows::{
    connect_to_pipe, create_secure_named_pipe, force_kill_process, read_pipe_request,
    request_close_process, restart_process, restart_windows_service, resume_process, set_process_priority,
    start_windows_service, stop_windows_service, suspend_process, write_pipe_message,
};
use windows::Win32::Foundation::HANDLE;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    Error as WindowsServiceError,
};

const SERVICE_NAME: &str = "tasktui-service";

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<()> {
    match env::args().nth(1).as_deref() {
        Some("--console") => run_console_mode(),
        Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!("unknown argument: {other}")),
        None => match run_service_dispatcher() {
            Ok(()) => Ok(()),
            Err(error) if is_not_started_by_scm(&error) => Err(anyhow::anyhow!(
                "service dispatcher is only available under SCM. Use `tasktui-service.exe --console` for local debugging."
            )),
            Err(error) => Err(error),
        },
    }
}

pub fn run_service_dispatcher() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context("start service dispatcher")
}

fn run_console_mode() -> Result<()> {
    println!("starting {SERVICE_NAME} in console mode on {}", tasktui_core::PIPE_NAME);
    let running = Arc::new(AtomicBool::new(true));
    pipe_server_loop(running)
}

fn print_help() {
    println!("tasktui-service");
    println!("  --console   Run the privileged pipe server in the foreground for local debugging");
    println!("  --help      Show this help");
}

fn is_not_started_by_scm(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<WindowsServiceError>())
        .and_then(|service_error| match service_error {
            WindowsServiceError::Winapi(io_error) => io_error.raw_os_error(),
            _ => None,
        })
        == Some(1063)
}

pub fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        eprintln!("service error: {error:?}");
    }
}

fn run_service() -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let running_for_handler = Arc::clone(&running);
    let status_handle = service_control_handler::register(SERVICE_NAME, move |control_event| match control_event {
        ServiceControl::Stop => {
            running_for_handler.store(false, Ordering::SeqCst);
            let _ = connect_to_pipe();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }).context("register service control handler")?;

    status_handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .context("set running status")?;

    pipe_server_loop(running)?;

    status_handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .context("set stopped status")?;

    Ok(())
}

pub fn pipe_server_loop(running: Arc<AtomicBool>) -> Result<()> {
    while running.load(Ordering::SeqCst) {
        let server = create_secure_named_pipe()?;
        server.connect()?;
        if let Err(error) = handle_client(server.handle()) {
            eprintln!("pipe client error: {error:?}");
        }
    }
    Ok(())
}

pub fn handle_client(handle: HANDLE) -> Result<()> {
    let request = read_pipe_request(handle)?;
    let response = process_request(&request);
    write_pipe_message(handle, &response)?;
    Ok(())
}

pub fn process_request(request: &tasktui_core::ApiRequest) -> ApiResponse {
    match validate_api_version(&request.version).and_then(|_| dispatch_command(&request.command)) {
        Ok(result) => ApiResponse::success(request.request_id.clone(), result),
        Err(error) => ApiResponse::failure(request.request_id.clone(), error),
    }
}

pub fn dispatch_command(command: &AdminCommand) -> Result<AdminResult, TasktuiError> {
    match command {
        AdminCommand::Ping => Ok(AdminResult::Pong),
        AdminCommand::ForceKillProcess { pid } => {
            force_kill_process(*pid).map_err(map_anyhow)?;
            Ok(AdminResult::ProcessClosed { pid: *pid, forced: true })
        }
        AdminCommand::RequestCloseProcess { pid } => {
            request_close_process(*pid).map_err(map_anyhow)?;
            Ok(AdminResult::ProcessClosed { pid: *pid, forced: false })
        }
        AdminCommand::RestartProcess { pid } => {
            restart_process(*pid).map_err(map_anyhow)?;
            Ok(AdminResult::ProcessRestarted { pid: *pid })
        }
        AdminCommand::SuspendProcess { pid } => {
            suspend_process(*pid).map_err(map_anyhow)?;
            Ok(AdminResult::ProcessStateChanged {
                pid: *pid,
                action: ProcessAction::Suspended,
            })
        }
        AdminCommand::ResumeProcess { pid } => {
            resume_process(*pid).map_err(map_anyhow)?;
            Ok(AdminResult::ProcessStateChanged {
                pid: *pid,
                action: ProcessAction::Resumed,
            })
        }
        AdminCommand::SetPriority { pid, priority } => {
            set_process_priority(*pid, *priority).map_err(map_anyhow)?;
            Ok(AdminResult::ProcessPriorityChanged {
                pid: *pid,
                priority: *priority,
            })
        }
        AdminCommand::StartService { service_name } => {
            validate_service_name(service_name)?;
            start_windows_service(service_name).map_err(map_anyhow)?;
            Ok(AdminResult::ServiceStateChanged {
                service_name: service_name.clone(),
                action: ServiceAction::Started,
            })
        }
        AdminCommand::StopService { service_name } => {
            validate_service_name(service_name)?;
            stop_windows_service(service_name).map_err(map_anyhow)?;
            Ok(AdminResult::ServiceStateChanged {
                service_name: service_name.clone(),
                action: ServiceAction::Stopped,
            })
        }
        AdminCommand::RestartService { service_name, timeout_ms } => {
            validate_service_name(service_name)?;
            restart_windows_service(service_name, Duration::from_millis((*timeout_ms).into()))
                .map_err(map_anyhow)?;
            Ok(AdminResult::ServiceStateChanged {
                service_name: service_name.clone(),
                action: ServiceAction::Restarted,
            })
        }
    }
}

fn map_anyhow(error: anyhow::Error) -> TasktuiError {
    match error.downcast::<TasktuiError>() {
        Ok(tasktui_error) => tasktui_error,
        Err(other) => TasktuiError::Message(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tasktui_core::{API_VERSION, ApiRequest};

    #[test]
    fn invalid_version_returns_failure_response() {
        let request = ApiRequest {
            request_id: "req-1".into(),
            version: "v0".into(),
            command: AdminCommand::Ping,
        };

        let response = process_request(&request);

        assert_eq!(response.request_id, "req-1");
        assert!(!response.ok);
        assert_eq!(response.error, Some(TasktuiError::InvalidVersion));
        assert_eq!(response.result, None);
    }

    #[test]
    fn ping_returns_pong() {
        let request = ApiRequest {
            request_id: "req-2".into(),
            version: API_VERSION.into(),
            command: AdminCommand::Ping,
        };

        let response = process_request(&request);

        assert!(response.ok);
        assert_eq!(response.result, Some(AdminResult::Pong));
        assert_eq!(response.error, None);
    }
}
