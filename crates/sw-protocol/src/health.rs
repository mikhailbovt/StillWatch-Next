use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Counter, UtcTimestamp};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HealthEvent {
    pub sequence: Counter,
    pub camera_id: Option<Uuid>,
    /// Stable event code. Clients display unfamiliar codes as generic technical events.
    pub code: String,
    pub opened_at: UtcTimestamp,
    pub ended_at: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HealthEventPage {
    pub events: Vec<HealthEvent>,
    pub next: Option<Counter>,
}
