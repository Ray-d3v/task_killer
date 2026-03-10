use anyhow::{Result, anyhow};
use std::env;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tasktui_core::{API_VERSION, AdminCommand, AdminResult, ApiRequest, ProcessPriority, TasktuiError};
use tasktui_platform_windows::{list_tcp_port_owners, open_path_in_explorer, send_request};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args)? {
        ParsedCommand::Admin(command) => {
            let request = ApiRequest {
                request_id: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .to_string(),
                version: API_VERSION.into(),
                command,
            };

            let response = send_request(&request)?;
            if !response.ok {
                let error = response.error.unwrap_or(TasktuiError::ServiceUnavailable);
                return Err(anyhow!("admin operation failed: {error}"));
            }

            match response.result {
                Some(AdminResult::Pong) => println!("pong"),
                Some(AdminResult::ProcessClosed { pid, forced }) => {
                    if forced {
                        println!("terminated process {pid}");
                    } else {
                        println!("requested close for process {pid}");
                    }
                }
                Some(AdminResult::ProcessRestarted { pid }) => {
                    println!("restarted process {pid}");
                }
                Some(AdminResult::ProcessStateChanged { pid, action }) => {
                    println!("process {pid} {action}");
                }
                Some(AdminResult::ProcessPriorityChanged { pid, priority }) => {
                    println!("process {pid} priority set to {priority}");
                }
                Some(AdminResult::ServiceStateChanged {
                    service_name,
                    action,
                }) => {
                    println!("service {service_name} {action}");
                }
                None => println!("ok"),
            }
        }
        ParsedCommand::OpenPath { pid } => {
            let path = resolve_process_path(pid)?;
            open_path_in_explorer(Path::new(&path))?;
            println!("opened executable folder for {path}");
        }
        ParsedCommand::ListPorts => {
            for row in list_tcp_port_owners()? {
                println!(
                    "{:<6} {:<21} {:<21} {}",
                    row.pid,
                    format!("{}:{}", row.local_addr, row.local_port),
                    format!("{}:{}", row.remote_addr, row.remote_port),
                    row.state
                );
            }
        }
    }
    Ok(())
}

enum ParsedCommand {
    Admin(AdminCommand),
    OpenPath { pid: u32 },
    ListPorts,
}

fn parse_args(args: &[String]) -> Result<ParsedCommand> {
    match args {
        [] => {
            print_help();
            std::process::exit(0);
        }
        [flag] if flag == "--help" || flag == "-h" => {
            print_help();
            std::process::exit(0);
        }
        [command] if command == "list-ports" => Ok(ParsedCommand::ListPorts),
        [command] if command == "ping" => Ok(ParsedCommand::Admin(AdminCommand::Ping)),
        [command, pid] if command == "kill-pid" => Ok(ParsedCommand::Admin(AdminCommand::ForceKillProcess {
            pid: parse_pid(pid)?,
        })),
        [command, pid] if command == "close-pid" => Ok(ParsedCommand::Admin(AdminCommand::RequestCloseProcess {
            pid: parse_pid(pid)?,
        })),
        [command, pid] if command == "restart-pid" => Ok(ParsedCommand::Admin(AdminCommand::RestartProcess {
            pid: parse_pid(pid)?,
        })),
        [command, pid] if command == "suspend-pid" => Ok(ParsedCommand::Admin(AdminCommand::SuspendProcess {
            pid: parse_pid(pid)?,
        })),
        [command, pid] if command == "resume-pid" => Ok(ParsedCommand::Admin(AdminCommand::ResumeProcess {
            pid: parse_pid(pid)?,
        })),
        [command, pid, priority] if command == "set-priority" => Ok(ParsedCommand::Admin(AdminCommand::SetPriority {
            pid: parse_pid(pid)?,
            priority: parse_priority(priority)?,
        })),
        [command, pid] if command == "open-path" => Ok(ParsedCommand::OpenPath {
            pid: parse_pid(pid)?,
        }),
        [command, service_name] if command == "start-service" => Ok(ParsedCommand::Admin(AdminCommand::StartService {
            service_name: service_name.clone(),
        })),
        [command, service_name] if command == "stop-service" => Ok(ParsedCommand::Admin(AdminCommand::StopService {
            service_name: service_name.clone(),
        })),
        [command, service_name] if command == "restart-service" => {
            Ok(ParsedCommand::Admin(AdminCommand::RestartService {
                service_name: service_name.clone(),
                timeout_ms: 30_000,
            }))
        }
        [command, service_name, timeout_ms] if command == "restart-service" => {
            Ok(ParsedCommand::Admin(AdminCommand::RestartService {
                service_name: service_name.clone(),
                timeout_ms: timeout_ms
                    .parse()
                    .map_err(|_| anyhow!("invalid timeout_ms: {timeout_ms}"))?,
            }))
        }
        _ => Err(anyhow!("invalid arguments. Use --help for usage.")),
    }
}

fn parse_pid(value: &str) -> Result<u32> {
    value.parse().map_err(|_| anyhow!("invalid pid: {value}"))
}

fn parse_priority(value: &str) -> Result<ProcessPriority> {
    match value {
        "idle" => Ok(ProcessPriority::Idle),
        "below_normal" => Ok(ProcessPriority::BelowNormal),
        "normal" => Ok(ProcessPriority::Normal),
        "above_normal" => Ok(ProcessPriority::AboveNormal),
        "high" => Ok(ProcessPriority::High),
        _ => Err(anyhow!("invalid priority: {value}")),
    }
}

fn print_help() {
    println!("tasktuictl");
    println!("  ping");
    println!("  list-ports");
    println!("  open-path <pid>");
    println!("  close-pid <pid>");
    println!("  kill-pid <pid>");
    println!("  restart-pid <pid>");
    println!("  suspend-pid <pid>");
    println!("  resume-pid <pid>");
    println!("  set-priority <pid> <idle|below_normal|normal|above_normal|high>");
    println!("  start-service <service_name>");
    println!("  stop-service <service_name>");
    println!("  restart-service <service_name> [timeout_ms]");
}

fn resolve_process_path(pid: u32) -> Result<String> {
    let mut system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
    );
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::everything(),
    );
    let process = system
        .process(Pid::from_u32(pid))
        .ok_or_else(|| anyhow!("process not found: {pid}"))?;
    let path = process
        .exe()
        .ok_or_else(|| anyhow!("process has no executable path: {pid}"))?;
    Ok(path.display().to_string())
}
