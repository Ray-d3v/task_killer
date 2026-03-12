use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use tasktui_app::update::{
    DEFAULT_SERVICE_NAME, DownloadedRelease, GITHUB_REPO, UpdateCheck, check_for_updates_for_repo,
    current_version, download_release_artifacts, quote_windows_arg,
};
use tasktui_platform_windows::stop_windows_service;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWDEFAULT;
use windows::core::{PCWSTR, w};

const CREATE_NEW_CONSOLE: u32 = 0x00000010;
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let command = parse_command(std::env::args_os().skip(1).collect())?;
    match command {
        UpdaterCommand::Check { current_version } => run_check(&current_version),
        UpdaterCommand::Install {
            current_version,
            wait_pid,
            restart_app_path,
            service_name,
        } => {
            if !is_running_elevated()? {
                relaunch_self_elevated()?;
                return Ok(());
            }
            run_install(&current_version, wait_pid, restart_app_path.as_deref(), &service_name)
        }
    }
}

fn run_check(current: &Version) -> Result<()> {
    match check_for_updates_for_repo(GITHUB_REPO, current)? {
        UpdateCheck::UpToDate { current } => {
            println!("Task Killer {current} is already up to date.");
        }
        UpdateCheck::UpdateAvailable { current, release } => {
            println!(
                "Update available: {current} -> {} ({})",
                release.version, release.msi_asset_name
            );
        }
    }
    Ok(())
}

fn run_install(
    current: &Version,
    wait_pid: Option<u32>,
    restart_app_path: Option<&Path>,
    service_name: &str,
) -> Result<()> {
    let release = match check_for_updates_for_repo(GITHUB_REPO, current)? {
        UpdateCheck::UpToDate { current } => {
            println!("Task Killer {current} is already up to date.");
            return Ok(());
        }
        UpdateCheck::UpdateAvailable { release, .. } => release,
    };

    println!("Downloading Task Killer {}...", release.version);
    let downloaded = download_release_artifacts(&release)?;

    if let Some(pid) = wait_pid {
        println!("Waiting for tasktui-app.exe to exit...");
        wait_for_pid_exit(pid, Duration::from_secs(30))?;
    }

    println!("Stopping service {}...", service_name);
    ensure_service_stopped(service_name)?;

    println!("Launching MSI installer...");
    run_msi_install(&downloaded)?;

    if let Some(app_path) = restart_app_path {
        println!("Restarting Task Killer...");
        restart_app(app_path)?;
    }

    println!("Update to {} completed.", release.version);
    Ok(())
}

fn run_msi_install(downloaded: &DownloadedRelease) -> Result<()> {
    let log_path = downloaded.msi_path.with_extension("install.log");
    let status = Command::new("msiexec.exe")
        .arg("/i")
        .arg(&downloaded.msi_path)
        .args(["/passive", "/norestart", "/l*v"])
        .arg(&log_path)
        .status()
        .with_context(|| format!("launch msiexec for {}", downloaded.msi_path.display()))?;

    if !status.success() {
        bail!(
            "msiexec failed with status {} (log: {})",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into()),
            log_path.display()
        );
    }

    Ok(())
}

fn ensure_service_stopped(service_name: &str) -> Result<()> {
    match stop_windows_service(service_name) {
        Ok(()) => return Ok(()),
        Err(error) => {
            eprintln!("Warning: graceful stop failed for {service_name}: {error:#}");
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

fn restart_app(app_path: &Path) -> Result<()> {
    Command::new(app_path)
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("restart {}", app_path.display()))?;
    Ok(())
}

fn wait_for_pid_exit(pid: u32, timeout: Duration) -> Result<()> {
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, false, pid)
            .with_context(|| format!("open process {pid} for wait"))?;
        let wait_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let result = WaitForSingleObject(handle, wait_ms);
        let _ = CloseHandle(handle);
        match result.0 {
            0 => Ok(()),
            258 => Err(anyhow!("timed out waiting for process {pid} to exit")),
            other => Err(anyhow!("WaitForSingleObject returned {other:?} for process {pid}")),
        }
    }
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

fn parse_command(args: Vec<OsString>) -> Result<UpdaterCommand> {
    let mut iter = args.into_iter();
    let command = iter
        .next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "install".into());

    let mut current = current_version();
    let mut wait_pid = None;
    let mut restart_app_path = None;
    let mut service_name = DEFAULT_SERVICE_NAME.to_string();

    while let Some(flag) = iter.next() {
        let flag = flag.to_string_lossy();
        match flag.as_ref() {
            "--current-version" => {
                let value = iter.next().ok_or_else(|| anyhow!("missing value for --current-version"))?;
                current = Version::parse(&value.to_string_lossy())
                    .context("parse --current-version as semver")?;
            }
            "--wait-pid" => {
                let value = iter.next().ok_or_else(|| anyhow!("missing value for --wait-pid"))?;
                wait_pid = Some(
                    value
                        .to_string_lossy()
                        .parse::<u32>()
                        .context("parse --wait-pid as u32")?,
                );
            }
            "--restart-app-path" => {
                let value =
                    iter.next().ok_or_else(|| anyhow!("missing value for --restart-app-path"))?;
                restart_app_path = Some(PathBuf::from(value));
            }
            "--service-name" => {
                let value = iter.next().ok_or_else(|| anyhow!("missing value for --service-name"))?;
                service_name = value.to_string_lossy().into_owned();
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    match command.as_str() {
        "check" => Ok(UpdaterCommand::Check {
            current_version: current,
        }),
        "install" => Ok(UpdaterCommand::Install {
            current_version: current,
            wait_pid,
            restart_app_path,
            service_name,
        }),
        other => bail!("unknown updater command: {other}"),
    }
}

fn print_help() {
    println!("Task Killer updater");
    println!();
    println!("Commands:");
    println!("  check");
    println!("  install [--current-version <semver>] [--wait-pid <pid>] [--restart-app-path <path>] [--service-name <name>]");
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
    let exe = std::env::current_exe().context("resolve current updater path")?;
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
            bail!("failed to relaunch updater with elevation (ShellExecuteW code {})", result.0 as usize);
        }
    }

    Ok(())
}

fn to_utf16_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

enum UpdaterCommand {
    Check {
        current_version: Version,
    },
    Install {
        current_version: Version,
        wait_pid: Option<u32>,
        restart_app_path: Option<PathBuf>,
        service_name: String,
    },
}
