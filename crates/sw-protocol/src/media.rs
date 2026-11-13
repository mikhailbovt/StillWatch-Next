use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Counter, UtcTimestamp};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StreamProfile {
    Preview,
    Detail,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StreamRequest {
    pub camera_id: Uuid,
    pub profile: StreamProfile,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StreamGrant {
    pub id: Uuid,
    pub camera_id: Uuid,
    pub profile: StreamProfile,
    pub source_epoch: Counter,
    pub scene_revision: Counter,
    pub whep_path: String,
    pub expires_at: UtcTimestamp,
    pub redacted: bool,
    pub codec: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IceServer {
    pub url: String,
    pub username: String,
    pub credential: String,
}
