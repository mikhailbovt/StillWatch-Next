use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ApiVersion, UtcTimestamp};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Owner,
    Operator,
    Viewer,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DeviceScope {
    CamerasWrite,
    EventsAck,
    EvidenceRead,
    StreamsRead,
    ProviderEvaluate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PairingProfile {
    Lan,
    Proxy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PairingRequest {
    pub role: DeviceRole,
    /// An empty list grants access to all cameras for an owner and none for other roles.
    pub camera_ids: Vec<Uuid>,
    pub scopes: Vec<DeviceScope>,
}

impl PairingRequest {
    pub fn valid(&self) -> bool {
        self.camera_ids.len() <= 128
            && self.scopes.len() <= 5
            && self
                .camera_ids
                .iter()
                .enumerate()
                .all(|(i, id)| !id.is_nil() && !self.camera_ids[..i].contains(id))
            && self
                .scopes
                .iter()
                .enumerate()
                .all(|(i, scope)| !self.scopes[..i].contains(scope))
            && match self.role {
                DeviceRole::Owner => self.camera_ids.is_empty() && self.scopes.is_empty(),
                DeviceRole::Operator => self.scopes.iter().all(|scope| {
                    !matches!(
                        scope,
                        DeviceScope::CamerasWrite | DeviceScope::ProviderEvaluate
                    )
                }),
                DeviceRole::Viewer => self.scopes.iter().all(|scope| {
                    matches!(scope, DeviceScope::EvidenceRead | DeviceScope::StreamsRead)
                }),
            }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PairingOffer {
    pub api: ApiVersion,
    pub endpoint: String,
    pub core_instance_id: Uuid,
    pub tls_certificate_sha256: String,
    /// Base64url DER for the explicitly pinned, self-signed endpoint certificate.
    pub tls_certificate_der: String,
    pub challenge: Uuid,
    pub secret: String,
    pub expires_at: UtcTimestamp,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PairingFinish {
    pub challenge: Uuid,
    pub secret: String,
    pub label: String,
    pub public_key: String,
    pub signature: String,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    pub device_id: Uuid,
    pub refresh_token: String,
    pub nonce: Uuid,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Device {
    pub id: Uuid,
    pub label: String,
    pub role: DeviceRole,
    pub camera_ids: Vec<Uuid>,
    pub scopes: Vec<DeviceScope>,
    pub paired_at: UtcTimestamp,
    pub revoked_at: Option<UtcTimestamp>,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeviceCredentials {
    pub device: Device,
    pub access_token: String,
    pub access_expires_at: UtcTimestamp,
    pub refresh_token: String,
    pub refresh_expires_at: UtcTimestamp,
}

/// Ed25519 proof over the one-time challenge, enrollment secret and device label.
pub fn pairing_message(instance: Uuid, challenge: Uuid, secret: &str, label: &str) -> Vec<u8> {
    let mut bytes = b"stillwatch/pair/v2\0".to_vec();
    bytes.extend_from_slice(instance.as_bytes());
    bytes.extend_from_slice(challenge.as_bytes());
    bytes.extend_from_slice(secret.as_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes
}

/// Ed25519 proof binds a refresh operation to the device and its fresh nonce.
pub fn refresh_message(instance: Uuid, device: Uuid, nonce: Uuid, refresh: &str) -> Vec<u8> {
    let mut bytes = b"stillwatch/refresh/v2\0".to_vec();
    bytes.extend_from_slice(instance.as_bytes());
    bytes.extend_from_slice(device.as_bytes());
    bytes.extend_from_slice(nonce.as_bytes());
    bytes.extend_from_slice(refresh.as_bytes());
    bytes
}
