use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionHealth {
    Running,
    Paused,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusBlock {
    pub health: ConnectionHealth,
    pub detail: String,
}

impl StatusBlock {
    pub fn stopped(detail: impl Into<String>) -> Self {
        Self {
            health: ConnectionHealth::Stopped,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub bundle_id: String,
    pub host_address: String,
    pub port: u16,
    pub force_qr_code: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bundle_id: "vgood.clearxr.client".to_string(),
            host_address: "127.0.0.1".to_string(),
            port: 55_000,
            force_qr_code: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BarcodePayload {
    pub client_token: String,
    pub certificate_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SessionInformation {
    pub session_id: String,
    pub client_id: String,
    pub barcode: BarcodePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub config: AppConfig,
    pub bonjour: StatusBlock,
    pub session_management: StatusBlock,
    pub cloudxr: StatusBlock,
    pub server_id: Option<String>,
    pub qr_data_url: Option<String>,
    pub notes: Vec<String>,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            config: AppConfig::default(),
            bonjour: StatusBlock::stopped("Not started"),
            session_management: StatusBlock::stopped("Not started"),
            cloudxr: StatusBlock::stopped("Not started"),
            server_id: None,
            qr_data_url: None,
            notes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenXrRegistrationStatus {
    pub runtime_is_active: bool,
    pub runtime_manifest_path: String,
    pub active_runtime_path: Option<String>,
    pub runtime_detail: String,
    pub layer_is_registered: bool,
    pub layer_manifest_path: String,
    pub layer_registration_scope: Option<String>,
    pub layer_detail: String,
}
