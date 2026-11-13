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
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AlertReceipt {
    pub attention_cycle: Counter,
}
