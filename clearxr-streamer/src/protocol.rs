#![allow(dead_code)]

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const SUPPORTED_PROTOCOL_VERSION: &str = "1";
pub const MESSAGE_LENGTH_PREFIX_BYTES: usize = 4;
pub const BUNDLE_ID_KEY: &str = "Application-Identifier";
pub const SESSION_STATUS_WAITING: &str = "WAITING";
pub const SESSION_STATUS_CONNECTING: &str = "CONNECTING";
pub const SESSION_STATUS_CONNECTED: &str = "CONNECTED";
pub const SESSION_STATUS_PAUSED: &str = "PAUSED";
pub const SESSION_STATUS_DISCONNECTED: &str = "DISCONNECTED";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventEnvelope {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestConnectionMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "ProtocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "StreamingProvider")]
    pub streaming_provider: String,
    #[serde(rename = "StreamingProviderVersion")]
    pub streaming_provider_version: String,
    #[serde(rename = "UserInterfaceIdiom")]
    pub user_interface_idiom: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
    #[serde(rename = "ClientID")]
    pub client_id: String,
}

impl RequestConnectionMessage {
    pub fn new(session_id: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            event: "RequestConnection".to_string(),
            protocol_version: SUPPORTED_PROTOCOL_VERSION.to_string(),
            streaming_provider: "CloudXR".to_string(),
            streaming_provider_version: "6.x".to_string(),
            user_interface_idiom: "visionOS".to_string(),
            session_id: session_id.into(),
            client_id: client_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcknowledgeConnectionMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
    #[serde(rename = "ServerID")]
    pub server_id: String,
    #[serde(
        rename = "CertificateFingerprint",
        skip_serializing_if = "Option::is_none"
    )]
    pub certificate_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestBarcodePresentationMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcknowledgeBarcodePresentationMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionStatusDidChangeMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
    #[serde(rename = "Status")]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaStreamIsReadyMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestSessionDisconnectMessage {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
}

pub async fn read_frame<R>(reader: &mut R) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut length_prefix = [0_u8; MESSAGE_LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut length_prefix).await?;

    let payload_len = u32::from_le_bytes(length_prefix) as usize;
    let mut payload = vec![0_u8; payload_len];
    reader.read_exact(&mut payload).await?;

    Ok(payload)
}

pub async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let length_prefix = (payload.len() as u32).to_le_bytes();
    writer.write_all(&length_prefix).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::{read_frame, write_frame, RequestConnectionMessage};

    #[tokio::test]
    async fn frame_round_trip_preserves_payload() {
        let payload = br#"{"Event":"ping"}"#;
        let (mut writer, mut reader) = tokio::io::duplex(64);

        write_frame(&mut writer, payload).await.unwrap();
        let decoded = read_frame(&mut reader).await.unwrap();

        assert_eq!(decoded, payload);
    }

    #[test]
    fn request_connection_uses_expected_event_name() {
        let message = RequestConnectionMessage::new("session-123", "client-456");
        let json = serde_json::to_string(&message).unwrap();

        assert!(json.contains("\"Event\":\"RequestConnection\""));
        assert!(json.contains("\"ProtocolVersion\":\"1\""));
    }
}
