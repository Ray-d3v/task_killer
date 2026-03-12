use anyhow::{Context, Result, anyhow};
use std::process::{Command, ExitCode};
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

const PRODUCT_NAME: &str = "Task Killer";
const UNINSTALL_REGISTRY_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

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
    let uninstall_command = find_uninstall_command()?;
    println!("Launching Task Killer uninstaller...");

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
