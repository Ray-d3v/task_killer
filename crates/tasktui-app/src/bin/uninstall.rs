use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};
use tasktui_app::update::{DEFAULT_SERVICE_NAME, quote_windows_arg};
use tasktui_platform_windows::stop_windows_service;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWDEFAULT};
use windows::core::{PCWSTR, w};
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

const PRODUCT_NAME: &str = "Task Killer";
const UNINSTALL_REGISTRY_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let details = format!("{error:#}");
            eprintln!("{details}");
            write_uninstall_log("ERROR", &details);
            show_error_dialog(&details);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    if !is_running_elevated()? {
        relaunch_self_elevated()?;
        return Ok(());
    }

    log_info("Stopping tasktui-service before uninstall...");
    ensure_service_stopped(DEFAULT_SERVICE_NAME)?;
    delete_service(DEFAULT_SERVICE_NAME)?;

    let uninstall_command = find_uninstall_command()?;
    log_info("Launching Task Killer uninstaller...");

    let status = Command::new("cmd")
        .args(["/C", &uninstall_command])
        .status()
        .with_context(|| format!("launch uninstall command: {uninstall_command}"))?;

    if !status.success() {
        return Err(anyhow!(
            "uninstall command exited with status {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into())
        ));
    }

    log_info("Task Killer uninstall completed.");
    Ok(())
}

fn find_uninstall_command() -> Result<String> {
    for access in [KEY_READ | KEY_WOW64_64KEY, KEY_READ | KEY_WOW64_32KEY] {
        if let Some(command) = find_uninstall_command_in_view(access)? {
            return Ok(command);
        }
    }
    Err(anyhow!("Task Killer uninstall information was not found in the registry"))
}

fn find_uninstall_command_in_view(access: u32) -> Result<Option<String>> {
    let hkcu = RegKey::predef(HKEY_LOCAL_MACHINE);
    let uninstall_root = match hkcu.open_subkey_with_flags(UNINSTALL_REGISTRY_PATH, access) {
        Ok(key) => key,
        Err(_) => return Ok(None),
    };

    for subkey_name in uninstall_root.enum_keys().flatten() {
        let subkey = match uninstall_root.open_subkey_with_flags(&subkey_name, access) {
            Ok(key) => key,
            Err(_) => continue,
        };

        let display_name: String = match subkey.get_value("DisplayName") {
            Ok(value) => value,
            Err(_) => continue,
        };

        if display_name != PRODUCT_NAME {
            continue;
        }

        if let Ok(command) = subkey.get_value::<String, _>("QuietUninstallString")
            && !command.trim().is_empty()
        {
            return Ok(Some(command));
        }

        if let Ok(command) = subkey.get_value::<String, _>("UninstallString")
            && !command.trim().is_empty()
        {
            return Ok(Some(command));
        }
    }

    Ok(None)
}

fn ensure_service_stopped(service_name: &str) -> Result<()> {
    match stop_windows_service(service_name) {
        Ok(()) => return Ok(()),
        Err(error) => {
            log_warn(&format!(
                "graceful stop failed for {service_name}: {error:#}"
            ));
        }
    }

    if let Some(pid) = query_service_pid(service_name)? {
        let status = Command::new("taskkill.exe")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
            .with_context(|| format!("force kill service process {pid}"))?;
        if !status.success() {
            bail!(
                "failed to force-stop {service_name} (PID {pid}), taskkill exited with {}",
                status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
        }
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if query_service_pid(service_name)?.is_none() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }

    Err(anyhow!("service {service_name} is still running after forced stop"))
}

fn delete_service(service_name: &str) -> Result<()> {
    log_info(&format!("Deleting service {}...", service_name));
    let status = Command::new("sc.exe")
        .args(["delete", service_name])
        .status()
        .with_context(|| format!("delete service {service_name}"))?;

    if !status.success() {
        bail!(
            "failed to delete {service_name}, sc.exe exited with {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if service_is_gone(service_name)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }

    Err(anyhow!("service {service_name} is still present after delete"))
}

fn service_is_gone(service_name: &str) -> Result<bool> {
    let output = Command::new("sc.exe")
        .args(["query", service_name])
        .output()
        .with_context(|| format!("query service state for {service_name}"))?;
    Ok(!output.status.success())
}

fn query_service_pid(service_name: &str) -> Result<Option<u32>> {
    let output = Command::new("sc.exe")
        .args(["queryex", service_name])
        .output()
        .with_context(|| format!("query service PID for {service_name}"))?;
    if !output.status.success() {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("PID") {
            let pid = value
                .split(':')
                .nth(1)
                .map(str::trim)
                .unwrap_or_default()
                .parse::<u32>()
                .unwrap_or(0);
            return Ok((pid != 0).then_some(pid));
        }
    }

    Ok(None)
}

fn is_running_elevated() -> Result<bool> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).context("open process token")?;
        let result = (|| {
            let mut elevation = TOKEN_ELEVATION::default();
            let mut returned = 0u32;
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
            .context("query token elevation")?;
            Ok(elevation.TokenIsElevated != 0)
        })();
        let _ = CloseHandle(token);
        result
    }
}

fn relaunch_self_elevated() -> Result<()> {
    let exe = std::env::current_exe().context("resolve current uninstaller path")?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let params = args
        .iter()
        .map(quote_windows_arg)
        .collect::<Vec<_>>()
        .join(" ");
    let exe_wide = to_utf16_null(exe.as_os_str().to_string_lossy().as_ref());
    let params_wide = to_utf16_null(&params);
    let workdir = exe
        .parent()
        .map(|path| path.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    let workdir_wide = to_utf16_null(&workdir);

    unsafe {
        let result: HINSTANCE = ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR(params_wide.as_ptr()),
            PCWSTR(workdir_wide.as_ptr()),
            SW_SHOWDEFAULT,
        );
        if result.0 as usize <= 32 {
            bail!(
                "failed to relaunch uninstaller with elevation (ShellExecuteW code {})",
                result.0 as usize
            );
        }
    }

    Ok(())
}

fn log_info(message: &str) {
    println!("{message}");
    write_uninstall_log("INFO", message);
}

fn log_warn(message: &str) {
    eprintln!("Warning: {message}");
    write_uninstall_log("WARN", message);
}

fn write_uninstall_log(level: &str, message: &str) {
    let log_path = uninstall_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = writeln!(file, "[{level}] {message}");
    }
}

fn uninstall_log_path() -> PathBuf {
    std::env::temp_dir().join("task_killer-uninstall.log")
}

fn show_error_dialog(details: &str) {
    let message = format!(
        "Task Killer uninstall failed.\n\n{}\n\nLog: {}",
        details,
        uninstall_log_path().display()
    );
    let message_wide = to_utf16_null(&message);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message_wide.as_ptr()),
            w!("Task Killer Uninstaller"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn to_utf16_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
