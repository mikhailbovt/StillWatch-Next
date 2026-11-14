use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Attention, Counter, UtcTimestamp};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AlertDelivery {
    pub id: Counter,
    pub core_id: Uuid,
    pub incident_id: Uuid,
    pub camera_id: Uuid,
    pub revision: Counter,
    pub attention_cycle: Counter,
    pub attention: Attention,
    pub expires_at: UtcTimestamp,
    pub client_seen_at: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AlertFeed {
    pub alerts: Vec<AlertDelivery>,
    /// Device-scoped continuation for the current feed scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Server time at which this page was evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<UtcTimestamp>,
    /// Changes when Core invalidates all delivery state during restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<Uuid>,
    /// Highest notification ID included in this scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Counter>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AlertReceipt {
    pub attention_cycle: Counter,
}
