use anyhow::{Context, Result, anyhow};
use std::net::Ipv4Addr;
use std::mem::{size_of, zeroed};
use std::time::{Duration, Instant};
use tasktui_core::{
    ApiRequest, ApiResponse, PIPE_NAME, ProcessPriority, ServiceRow, TasktuiError, TcpPortOwner,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, GetLastError, HLOCAL,
    HANDLE, HWND, LPARAM, LocalFree,
};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
};
use windows::Win32::Networking::WinSock::AF_INET;
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    SetNamedPipeHandleState,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW,
    QueryServiceConfigW, QueryServiceStatusEx, ENUM_SERVICE_STATUS_PROCESSW,
    QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT,
    SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_AUTO_START,
    SERVICE_BOOT_START, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_DISABLED,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
    SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
    SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_SYSTEM_START, SERVICE_WIN32,
    StartServiceW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, IsProcessCritical, OpenProcess, OpenThread,
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
    GetPriorityClass, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_TERMINATE, ResumeThread,
    SetPriorityClass, SuspendThread, THREAD_SUSPEND_RESUME, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_STYLE, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible,
    SMTO_ABORTIFHUNG, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SendMessageTimeoutW, SetWindowLongPtrW, SetWindowPos, SHOW_WINDOW_CMD, SW_SHOWDEFAULT,
    WM_CLOSE, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::core::{BOOL, PCWSTR, w};

const PIPE_TIMEOUT_MS: u32 = 5_000;
const SECURITY_DESCRIPTOR_SDDL: PCWSTR = w!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)");

pub struct NamedPipeServer {
    handle: HANDLE,
    security_descriptor: PSECURITY_DESCRIPTOR,
}

impl NamedPipeServer {
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    pub fn connect(&self) -> Result<()> {
        unsafe {
            match ConnectNamedPipe(self.handle, None) {
                Ok(_) => Ok(()),
                Err(err) => {
                    let last = GetLastError();
                    if last.0 == 535 {
                        Ok(())
                    } else {
                        Err(anyhow!(err)).context("connect named pipe")
                    }
                }
            }
        }
    }
}

impl Drop for NamedPipeServer {
    fn drop(&mut self) {
        unsafe {
            let _ = DisconnectNamedPipe(self.handle);
            let _ = CloseHandle(self.handle);
            if !self.security_descriptor.0.is_null() {
                let _ = LocalFree(Some(HLOCAL(self.security_descriptor.0)));
            }
        }
    }
}

fn to_utf16_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn from_wide_ptr(ptr: PCWSTR) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0usize;
        while *ptr.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr.0, len))
    }
}

pub fn list_tcp_port_owners() -> Result<Vec<TcpPortOwner>> {
    unsafe {
        let mut size = 0_u32;
        let initial = GetExtendedTcpTable(None, &mut size, true, AF_INET.0.into(), TCP_TABLE_OWNER_PID_ALL, 0);
        if initial != windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER.0 {
            return Err(anyhow!("GetExtendedTcpTable sizing failed with code {initial}"));
        }

        let mut buffer = vec![0_u8; size as usize];
        let result = GetExtendedTcpTable(
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            true,
            AF_INET.0.into(),
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
        if result != 0 {
            return Err(anyhow!("GetExtendedTcpTable failed with code {result}"));
        }

        let table = &*(buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>());
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        Ok(rows.iter().map(map_tcp_row).collect())
    }
}

fn map_tcp_row(row: &MIB_TCPROW_OWNER_PID) -> TcpPortOwner {
    TcpPortOwner {
        pid: row.dwOwningPid,
        local_addr: Ipv4Addr::from(row.dwLocalAddr.to_be_bytes()).to_string(),
        local_port: u16::from_be(row.dwLocalPort as u16),
        remote_addr: Ipv4Addr::from(row.dwRemoteAddr.to_be_bytes()).to_string(),
        remote_port: u16::from_be(row.dwRemotePort as u16),
        state: tcp_state_label(row.dwState).into(),
    }
}

fn tcp_state_label(state: u32) -> &'static str {
    match state {
        1 => "closed",
        2 => "listen",
        3 => "syn_sent",
        4 => "syn_received",
        5 => "established",
        6 => "fin_wait_1",
        7 => "fin_wait_2",
        8 => "close_wait",
        9 => "closing",
        10 => "last_ack",
        11 => "time_wait",
        12 => "delete_tcb",
        _ => "unknown",
    }
}

pub fn list_windows_services() -> Result<Vec<ServiceRow>> {
    unsafe {
        let scm = open_scm()?;
        let result = (|| {
            let mut bytes_needed = 0u32;
            let mut services_returned = 0u32;
            let mut resume_handle = 0u32;
            let first = EnumServicesStatusExW(
                scm,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                None,
                &mut bytes_needed,
                &mut services_returned,
                Some(&mut resume_handle),
                None,
            );
            if first.is_ok() && bytes_needed == 0 {
                return Ok(Vec::new());
            }

            let mut buffer = vec![0u8; bytes_needed as usize];
            EnumServicesStatusExW(
                scm,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                Some(buffer.as_mut_slice()),
                &mut bytes_needed,
                &mut services_returned,
                Some(&mut resume_handle),
                None,
            )
            .context("enumerate services")?;

            let entries = std::slice::from_raw_parts(
                buffer.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                services_returned as usize,
            );

            let mut rows = Vec::with_capacity(entries.len());
            for entry in entries {
                let service_name = from_wide_ptr(PCWSTR(entry.lpServiceName.0));
                let display_name = from_wide_ptr(PCWSTR(entry.lpDisplayName.0));
                let start_type = query_service_start_type(scm, &service_name)
                    .unwrap_or_else(|_| "unknown".into());
                rows.push(ServiceRow {
                    display_name,
                    service_name,
                    status: service_state_label(entry.ServiceStatusProcess.dwCurrentState.0),
                    start_type,
                });
            }
            rows.sort_by(|left, right| {
                left.display_name
                    .to_ascii_lowercase()
                    .cmp(&right.display_name.to_ascii_lowercase())
                    .then_with(|| left.service_name.cmp(&right.service_name))
            });
            Ok(rows)
        })();
        let _ = CloseServiceHandle(scm);
        result
    }
}

fn query_service_start_type(scm: SC_HANDLE, service_name: &str) -> Result<String> {
    unsafe {
        let service = open_service_handle(scm, service_name, SERVICE_QUERY_CONFIG)?;
        let result = (|| {
            let mut needed = 0u32;
            let _ = QueryServiceConfigW(service, None, 0, &mut needed);
            let mut buffer = vec![0u8; needed as usize];
            QueryServiceConfigW(
                service,
                Some(buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()),
                needed,
                &mut needed,
            )
                .context("query service config")?;
            let config = &*(buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>());
            Ok(service_start_type_label(config.dwStartType.0))
        })();
        let _ = CloseServiceHandle(service);
        result
    }
}

fn service_state_label(state: u32) -> String {
    match state {
        x if x == SERVICE_RUNNING.0 => "running",
        x if x == SERVICE_STOPPED.0 => "stopped",
        x if x == SERVICE_START_PENDING.0 => "start_pending",
        x if x == SERVICE_STOP_PENDING.0 => "stop_pending",
        3 => "stop_pending",
        4 => "running",
        5 => "continue_pending",
        6 => "pause_pending",
        7 => "paused",
        _ => "unknown",
    }
    .into()
}

fn service_start_type_label(start_type: u32) -> String {
    match start_type {
        x if x == SERVICE_AUTO_START.0 => "auto",
        x if x == SERVICE_DEMAND_START.0 => "manual",
        x if x == SERVICE_DISABLED.0 => "disabled",
        x if x == SERVICE_BOOT_START.0 => "boot",
        x if x == SERVICE_SYSTEM_START.0 => "system",
        _ => "unknown",
    }
    .into()
}

pub fn create_secure_named_pipe() -> Result<NamedPipeServer> {
    unsafe {
        let mut security_descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            SECURITY_DESCRIPTOR_SDDL,
            1,
            &mut security_descriptor,
            None,
        )
        .context("convert SDDL")?;

        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security_descriptor.0.cast(),
            bInheritHandle: false.into(),
        };

        let pipe_name = to_utf16_null(PIPE_NAME);
        let handle = CreateNamedPipeW(
            PCWSTR(pipe_name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            PIPE_TIMEOUT_MS,
            Some(&attributes),
        );
        if handle.is_invalid() {
            return Err(anyhow!("create named pipe: {}", windows::core::Error::from_thread()));
        }

        Ok(NamedPipeServer { handle, security_descriptor })
    }
}

pub fn connect_to_pipe() -> Result<HANDLE> {
    unsafe {
        let pipe_name = to_utf16_null(PIPE_NAME);
        match CreateFileW(
            PCWSTR(pipe_name.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            Ok(handle) => {
                let mode = PIPE_READMODE_MESSAGE;
                SetNamedPipeHandleState(handle, Some(&mode), None, None).context("set pipe read mode")?;
                Ok(handle)
            }
            Err(err) => {
                if GetLastError() == ERROR_FILE_NOT_FOUND {
                    Err(TasktuiError::ServiceUnavailable.into())
                } else {
                    Err(anyhow!(err)).context("connect to named pipe")
                }
            }
        }
    }
}

pub fn send_request(request: &ApiRequest) -> Result<ApiResponse> {
    let handle = connect_to_pipe()?;
    let response = (|| {
        write_pipe_message(handle, request)?;
        read_pipe_message(handle)
    })();
    unsafe {
        let _ = CloseHandle(handle);
    }
    response
}

pub fn open_path_in_explorer(path: &std::path::Path) -> Result<()> {
    let target = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(path);
    let wide_target: Vec<u16> = target.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe {
        let result = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide_target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SHOW_WINDOW_CMD(SW_SHOWDEFAULT.0),
        );
        if result.0 as usize <= 32 {
            return Err(anyhow!("ShellExecuteW failed with code {}", result.0 as usize));
        }
    }
    Ok(())
}

pub fn hide_console_title_bar() -> Result<()> {
    unsafe {
        let hwnd = GetConsoleWindow();
        if hwnd.0.is_null() {
            return Ok(());
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let stripped_style =
            style & !(WS_CAPTION.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0 | WS_SYSMENU.0);
        if stripped_style == style {
            return Ok(());
        }
        SetWindowLongPtrW(hwnd, GWL_STYLE, stripped_style as isize);
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        )
        .context("apply console title bar style")?;
    }
    Ok(())
}

pub fn read_pipe_message(handle: HANDLE) -> Result<ApiResponse> {
    let bytes = read_pipe_bytes(handle)?;
    serde_json::from_slice::<ApiResponse>(&bytes).context("deserialize response")
}

pub fn read_pipe_request(handle: HANDLE) -> Result<ApiRequest> {
    let bytes = read_pipe_bytes(handle)?;
    serde_json::from_slice::<ApiRequest>(&bytes).context("deserialize request")
}

fn read_pipe_bytes(handle: HANDLE) -> Result<Vec<u8>> {
    unsafe {
        let mut buffer = vec![0_u8; 4096];
        let mut output = Vec::new();
        loop {
            let mut read = 0_u32;
            match ReadFile(
                handle,
                Some(buffer.as_mut_slice()),
                Some(&mut read),
                None,
            ) {
                Ok(_) => {
                    output.extend_from_slice(&buffer[..read as usize]);
                    break;
                }
                Err(err) => {
                    let last = GetLastError();
                    if last == ERROR_MORE_DATA {
                        output.extend_from_slice(&buffer[..read as usize]);
                        continue;
                    }
                    if last == ERROR_BROKEN_PIPE {
                        return Err(TasktuiError::ServiceUnavailable.into());
                    }
                    return Err(anyhow!(err)).context("read pipe");
                }
            }
        }
        Ok(output)
    }
}

pub fn write_pipe_message<T: serde::Serialize>(handle: HANDLE, value: &T) -> Result<()> {
    unsafe {
        let payload = serde_json::to_vec(value).context("serialize payload")?;
        let mut written = 0_u32;
        WriteFile(
            handle,
            Some(payload.as_slice()),
            Some(&mut written),
            None,
        )
        .context("write pipe")?;
        if written as usize != payload.len() {
            return Err(anyhow!("partial pipe write"));
        }
        Ok(())
    }
}

pub fn force_kill_process(pid: u32) -> Result<()> {
    if pid == 0 || pid == 4 || pid == unsafe { GetCurrentProcessId() } {
        return Err(TasktuiError::AccessDenied.into());
    }

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .context("open process")?;
        let result = (|| {
            let mut is_critical = BOOL(0);
            if IsProcessCritical(handle, &mut is_critical).is_ok() && is_critical.as_bool() {
                return Err(TasktuiError::AccessDenied.into());
            }
            TerminateProcess(handle, 1).context("terminate process")?;
            let _ = WaitForSingleObject(handle, 5_000);
            Ok(())
        })();
        let _ = CloseHandle(handle);
        result
    }
}

pub fn suspend_process(pid: u32) -> Result<()> {
    if pid == 0 || pid == 4 || pid == unsafe { GetCurrentProcessId() } {
        return Err(TasktuiError::AccessDenied.into());
    }
    let thread_ids = enumerate_thread_ids_for_pid(pid)?;
    if thread_ids.is_empty() {
        return Err(TasktuiError::Unsupported.into());
    }
    for thread_id in thread_ids {
        unsafe {
            let handle = OpenThread(THREAD_SUSPEND_RESUME, false, thread_id).context("open thread")?;
            let previous = SuspendThread(handle);
            let _ = CloseHandle(handle);
            if previous == u32::MAX {
                return Err(anyhow!("SuspendThread failed for thread {thread_id}"));
            }
        }
    }
    Ok(())
}

pub fn resume_process(pid: u32) -> Result<()> {
    if pid == 0 || pid == 4 || pid == unsafe { GetCurrentProcessId() } {
        return Err(TasktuiError::AccessDenied.into());
    }
    let thread_ids = enumerate_thread_ids_for_pid(pid)?;
    if thread_ids.is_empty() {
        return Err(TasktuiError::Unsupported.into());
    }
    for thread_id in thread_ids {
        unsafe {
            let handle = OpenThread(THREAD_SUSPEND_RESUME, false, thread_id).context("open thread")?;
            let mut previous = ResumeThread(handle);
            while previous != u32::MAX && previous > 1 {
                previous = ResumeThread(handle);
            }
            let _ = CloseHandle(handle);
            if previous == u32::MAX {
                return Err(anyhow!("ResumeThread failed for thread {thread_id}"));
            }
        }
    }
    Ok(())
}

pub fn set_process_priority(pid: u32, priority: ProcessPriority) -> Result<()> {
    if pid == 0 || pid == 4 || pid == unsafe { GetCurrentProcessId() } {
        return Err(TasktuiError::AccessDenied.into());
    }
    let priority_class = match priority {
        ProcessPriority::Idle => IDLE_PRIORITY_CLASS,
        ProcessPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        ProcessPriority::Normal => NORMAL_PRIORITY_CLASS,
        ProcessPriority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        ProcessPriority::High => HIGH_PRIORITY_CLASS,
    };
    unsafe {
        let handle = OpenProcess(
            PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
        .context("open process for priority")?;
        let result = SetPriorityClass(handle, priority_class).context("set priority class");
        let _ = CloseHandle(handle);
        result?;
    }
    Ok(())
}

pub fn get_process_priority(pid: u32) -> Result<ProcessPriority> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .context("open process for priority query")?;
        let result = (|| {
            let class = GetPriorityClass(handle);
            if class == 0 {
                return Err(anyhow!("GetPriorityClass failed"));
            }
            match class {
                x if x == IDLE_PRIORITY_CLASS.0 => Ok(ProcessPriority::Idle),
                x if x == BELOW_NORMAL_PRIORITY_CLASS.0 => Ok(ProcessPriority::BelowNormal),
                x if x == NORMAL_PRIORITY_CLASS.0 => Ok(ProcessPriority::Normal),
                x if x == ABOVE_NORMAL_PRIORITY_CLASS.0 => Ok(ProcessPriority::AboveNormal),
                x if x == HIGH_PRIORITY_CLASS.0 => Ok(ProcessPriority::High),
                _ => Err(TasktuiError::Unsupported.into()),
            }
        })();
        let _ = CloseHandle(handle);
        result
    }
}

fn enumerate_thread_ids_for_pid(pid: u32) -> Result<Vec<u32>> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).context("create thread snapshot")?;
        let result = {
            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut thread_ids = Vec::new();
            if Thread32First(snapshot, &mut entry).is_ok() {
                loop {
                    if entry.th32OwnerProcessID == pid {
                        thread_ids.push(entry.th32ThreadID);
                    }
                    if Thread32Next(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            Ok(thread_ids)
        };
        let _ = CloseHandle(snapshot);
        result
    }
}

#[derive(Default)]
struct EnumWindowsState {
    pid: u32,
    windows: Vec<HWND>,
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = unsafe { &mut *(lparam.0 as *mut EnumWindowsState) };
    let mut window_pid = 0_u32;
    let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
    if window_pid == state.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        state.windows.push(hwnd);
    }
    BOOL(1)
}

pub fn enumerate_top_level_windows_for_pid(pid: u32) -> Result<Vec<HWND>> {
    let mut state = Box::new(EnumWindowsState { pid, windows: Vec::new() });
    unsafe {
        EnumWindows(Some(enum_windows_proc), LPARAM((&mut *state as *mut EnumWindowsState) as isize))
            .context("enumerate windows")?;
    }
    Ok(state.windows)
}

pub fn list_visible_top_level_window_pids() -> Result<Vec<u32>> {
    #[derive(Default)]
    struct VisibleWindowPidState {
        pids: Vec<u32>,
    }

    unsafe extern "system" fn collect_visible_window_pids(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut VisibleWindowPidState) };
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let mut pid = 0_u32;
            let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != 0 && !state.pids.contains(&pid) {
                state.pids.push(pid);
            }
        }
        BOOL(1)
    }

    let mut state = Box::<VisibleWindowPidState>::default();
    unsafe {
        EnumWindows(
            Some(collect_visible_window_pids),
            LPARAM((&mut *state as *mut VisibleWindowPidState) as isize),
        )
        .context("enumerate visible windows")?;
    }
    Ok(state.pids)
}

pub fn request_close_process(pid: u32) -> Result<()> {
    let windows = enumerate_top_level_windows_for_pid(pid)?;
    if windows.is_empty() {
        return Err(TasktuiError::NotClosable.into());
    }
    unsafe {
        for hwnd in windows {
            let mut result = 0_usize;
            let _ = SendMessageTimeoutW(
                hwnd,
                WM_CLOSE,
                Default::default(),
                Default::default(),
                SMTO_ABORTIFHUNG,
                2_000,
                Some(&mut result),
            );
        }
    }
    Ok(())
}

pub fn open_scm() -> Result<SC_HANDLE> {
    unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE).context("open scm") }
}

fn open_service_handle(scm: SC_HANDLE, service_name: &str, access: u32) -> Result<SC_HANDLE> {
    let wide = to_utf16_null(service_name);
    unsafe { OpenServiceW(scm, PCWSTR(wide.as_ptr()), access).context("open service") }
}

fn query_service_status(service: SC_HANDLE) -> Result<SERVICE_STATUS_PROCESS> {
    unsafe {
        let mut process: SERVICE_STATUS_PROCESS = zeroed();
        let mut needed = 0_u32;
        let buffer = std::slice::from_raw_parts_mut(
            (&mut process as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        );
        QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            Some(buffer),
            &mut needed,
        )
        .context("query service status")?;
        Ok(process)
    }
}

pub fn start_windows_service(service_name: &str) -> Result<()> {
    let scm = open_scm()?;
    let service = open_service_handle(scm, service_name, SERVICE_START | SERVICE_QUERY_STATUS)?;
    let result = (|| {
        let status = query_service_status(service)?;
        if status.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        unsafe { StartServiceW(service, None).context("start service")?; }
        wait_for_service_state(service, SERVICE_RUNNING, Duration::from_secs(30))
    })();
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
    }
    result
}

pub fn stop_windows_service(service_name: &str) -> Result<()> {
    let scm = open_scm()?;
    let service = open_service_handle(scm, service_name, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let result = (|| {
        let status = query_service_status(service)?;
        if status.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        unsafe {
            let mut service_status = SERVICE_STATUS::default();
            ControlService(service, SERVICE_CONTROL_STOP, &mut service_status).context("stop service")?;
        }
        wait_for_service_state(service, SERVICE_STOPPED, Duration::from_secs(30))
    })();
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
    }
    result
}

pub fn restart_windows_service(service_name: &str, timeout: Duration) -> Result<()> {
    let scm = open_scm()?;
    let service = open_service_handle(scm, service_name, SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let result = (|| {
        let status = query_service_status(service)?;
        if status.dwCurrentState != SERVICE_STOPPED && status.dwCurrentState != SERVICE_STOP_PENDING {
            unsafe {
                let mut service_status = SERVICE_STATUS::default();
                ControlService(service, SERVICE_CONTROL_STOP, &mut service_status)
                    .context("stop service for restart")?;
            }
            wait_for_service_state(service, SERVICE_STOPPED, timeout)?;
        }
        if status.dwCurrentState == SERVICE_STOP_PENDING {
            wait_for_service_state(service, SERVICE_STOPPED, timeout)?;
        }
        unsafe { StartServiceW(service, None).context("start service after stop")?; }
        wait_for_service_state(service, SERVICE_RUNNING, timeout)
    })();
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
    }
    result
}

fn wait_for_service_state(
    service: SC_HANDLE,
    target_state: windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = query_service_status(service)?;
        if status.dwCurrentState == target_state {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(TasktuiError::Timeout.into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
