# task_killer

Windows task-management TUI MVP with privilege separation.

The project uses a normal-user TUI plus a privileged Windows Service connected through a local Named Pipe. Display and filtering stay in the TUI. Administrative actions are delegated to the service.

## Workspace

- `crates/tasktui-app`
  - `ratatui` TUI and helper CLI binaries
- `crates/tasktui-core`
  - shared IPC types, API version, and errors
- `crates/tasktui-platform-windows`
  - Win32, SCM, shell, and Named Pipe wrappers
- `crates/tasktui-service`
  - privileged Windows Service backend

## MVP features

- Multi-tab Task Manager style layout
- Processes, Performance, Services, and Network views
- Search and filter
- Sort by CPU, memory, PID, or name
- High-load row highlighting for CPU and memory
- Process tree view
- TCP port owner list with process names
- Service browser with status and start type
- Summary cards plus CPU and memory history sparklines
- Right-click context menus for process, service, and network actions
- Persistent detail panes per tab
- Graceful close request
- Force kill
- Suspend and resume
- Priority change
- Start, stop, and restart Windows services
- Open selected executable folder

## Requirements

- Windows 10 or Windows 11 x64
- Rust toolchain
- Administrator privileges once for service installation

## Build

```powershell
cargo build --release
```

Artifacts:

- `target/release/tasktui-app.exe`
- `target/release/tasktui-service.exe`
- `target/release/tasktuictl.exe`

## Checks

```powershell
cargo test
cargo clippy --workspace --all-targets -- -D warnings
```

## Install the service

Run from an elevated PowerShell:

```powershell
.\scripts\install-service.ps1
```

Remove it with:

```powershell
.\scripts\uninstall-service.ps1
```

## Run the TUI

After the service is installed:

```powershell
.\target\release\tasktui-app.exe
```

At startup the TUI sends `Ping` and shows service reachability in the status area.
When launched directly in the classic console window, the app also hides the native title bar. This does not remove the surrounding UI of hosts such as Windows Terminal.

## Local service debugging

Run the backend in the foreground without SCM:

```powershell
.\scripts\run-service-console.ps1
```

Or directly:

```powershell
.\target\debug\tasktui-service.exe --console
```

Use `--help` for service CLI help:

```powershell
cargo run -p tasktui-service -- --help
```

## IPC smoke test

Use the helper CLI without opening the TUI:

```powershell
cargo run -p tasktui-app --bin tasktuictl -- ping
```

Or:

```powershell
.\scripts\smoke-ping.ps1
```

Available helper commands:

- `ping`
- `list-ports`
- `open-path <pid>`
- `close-pid <pid>`
- `kill-pid <pid>`
- `restart-pid <pid>`
- `suspend-pid <pid>`
- `resume-pid <pid>`
- `set-priority <pid> <idle|below_normal|normal|above_normal|high>`
- `start-service <service_name>`
- `stop-service <service_name>`
- `restart-service <service_name> [timeout_ms]`

## TUI key bindings

- `Tab` / `Shift+Tab` switch between `Processes`, `Performance`, `Services`, and `Network`
- `/` search the active list tab
- `s` cycle process sort mode
- `t` toggle process tree view
- `f` cycle the network state filter
- `Enter` toggle list/detail pane focus on `Processes`, `Services`, and `Network`
- `o` open the executable folder for the selected process or network owner
- `z` suspend the selected process
- `x` resume the selected process
- `r` restart the selected process
- `4/5/6/7/8` open priority confirmation for idle, below_normal, normal, above_normal, high
- `Up` / `Down` move selection
- `k` request graceful close
- `K` open force-kill confirmation
- `1` start the selected service
- `2` stop the selected service
- `3` restart the selected service
- `q` quit
- `Y` / `N` confirm or cancel force kill and service actions
- Mouse:
  - left click selects rows and tabs
  - right click opens a context menu
  - wheel scroll moves the current selection
  - when a context menu is open, background input is blocked until the menu closes

## IPC contract

- Pipe: `\\.\pipe\tasktui.v1`
- Encoding: UTF-8 JSON
- Request: `ApiRequest { request_id, version, command }`
- Response: `ApiResponse { request_id, ok, result, error }`

Allowed administrative commands:

- `Ping`
- `ForceKillProcess { pid }`
- `RequestCloseProcess { pid }`
- `RestartProcess { pid }`
- `SuspendProcess { pid }`
- `ResumeProcess { pid }`
- `SetPriority { pid, priority }`
- `StartService { service_name }`
- `StopService { service_name }`
- `RestartService { service_name, timeout_ms }`

## Current limitations

- General process restart is not implemented
- `watch`, auto-restart rules, `kill-port`, and dump capture are not implemented
- Protected processes and protected services may still be inaccessible
- `RequestCloseProcess` only works for processes with a closable top-level window
- Performance view currently tracks only whole-system CPU and memory

## References

- [How User Account Control works](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)
- [Named Pipe security and access rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- [ControlService](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-controlservice)
- [Process Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights)
- [ShellExecuteW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecutew)
