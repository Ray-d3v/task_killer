use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

pub const API_VERSION: &str = "v1";
pub const PIPE_NAME: &str = r"\\.\pipe\tasktui.v1";
const MAX_SERVICE_NAME_LEN: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TcpPortOwner {
    pub pid: u32,
    pub local_addr: String,
    pub local_port: u16,
    pub remote_addr: String,
    pub remote_port: u16,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRow {
    pub display_name: String,
    pub service_name: String,
    pub status: String,
    pub start_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiRequest {
    pub request_id: String,
    pub version: String,
    pub command: AdminCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiResponse {
    pub request_id: String,
    pub ok: bool,
    pub result: Option<AdminResult>,
    pub error: Option<TasktuiError>,
}

impl ApiResponse {
    pub fn success(request_id: String, result: AdminResult) -> Self {
        Self { request_id, ok: true, result: Some(result), error: None }
    }

    pub fn failure(request_id: String, error: TasktuiError) -> Self {
        Self { request_id, ok: false, result: None, error: Some(error) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminCommand {
    Ping,
    ForceKillProcess { pid: u32 },
    RequestCloseProcess { pid: u32 },
    SuspendProcess { pid: u32 },
    ResumeProcess { pid: u32 },
    SetPriority { pid: u32, priority: ProcessPriority },
    StartService { service_name: String },
    StopService { service_name: String },
    RestartService { service_name: String, timeout_ms: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminResult {
    Pong,
    ProcessClosed { pid: u32, forced: bool },
    ProcessStateChanged { pid: u32, action: ProcessAction },
    ProcessPriorityChanged { pid: u32, priority: ProcessPriority },
    ServiceStateChanged { service_name: String, action: ServiceAction },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessAction {
    Suspended,
    Resumed,
}

impl fmt::Display for ProcessAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Suspended => write!(f, "suspended"),
            Self::Resumed => write!(f, "resumed"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPriority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

impl fmt::Display for ProcessPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::BelowNormal => write!(f, "below_normal"),
            Self::Normal => write!(f, "normal"),
            Self::AboveNormal => write!(f, "above_normal"),
            Self::High => write!(f, "high"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Started,
    Stopped,
    Restarted,
}

impl fmt::Display for ServiceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Started => write!(f, "started"),
            Self::Stopped => write!(f, "stopped"),
            Self::Restarted => write!(f, "restarted"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum TasktuiError {
    #[error("invalid API version")]
    InvalidVersion,
    #[error("service not reachable")]
    ServiceUnavailable,
    #[error("service name is invalid")]
    InvalidServiceName,
    #[error("process cannot be closed gracefully")]
    NotClosable,
    #[error("operation is not allowed")]
    AccessDenied,
    #[error("operation timed out")]
    Timeout,
    #[error("operation is unsupported")]
    Unsupported,
    #[error("{0}")]
    Message(String),
}

pub fn validate_api_version(version: &str) -> Result<(), TasktuiError> {
    if version == API_VERSION { Ok(()) } else { Err(TasktuiError::InvalidVersion) }
}

pub fn validate_service_name(service_name: &str) -> Result<(), TasktuiError> {
    if service_name.is_empty() || service_name.len() > MAX_SERVICE_NAME_LEN {
        return Err(TasktuiError::InvalidServiceName);
    }
    if service_name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        Ok(())
    } else {
        Err(TasktuiError::InvalidServiceName)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let request = ApiRequest {
            request_id: "abc".into(),
            version: API_VERSION.into(),
            command: AdminCommand::RestartService { service_name: "Spooler".into(), timeout_ms: 30_000 },
        };
        let json = serde_json::to_string(&request).expect("serialize request");
        let parsed: ApiRequest = serde_json::from_str(&json).expect("deserialize request");
        assert_eq!(parsed, request);
    }

    #[test]
    fn rejects_wrong_version() {
        assert_eq!(validate_api_version("v2").expect_err("must reject"), TasktuiError::InvalidVersion);
    }

    #[test]
    fn validates_service_names() {
        assert!(validate_service_name("Spooler").is_ok());
        assert_eq!(validate_service_name("bad name").expect_err("must reject"), TasktuiError::InvalidServiceName);
    }
}
