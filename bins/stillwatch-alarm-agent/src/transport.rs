use std::fmt;

use stillwatch_sdk::protocol::AlertDelivery;

/// Presents an already-authorized, current alert through a bundled output.
/// Success allows the agent to record a device receipt. The transport owns no
/// Core credentials and cannot acknowledge or resolve an incident.
pub trait NotificationTransport {
    async fn present(&self, alert: &AlertDelivery) -> Result<(), OutputUnavailable>;

    async fn repeat(&self, _: &AlertDelivery) -> Result<(), OutputUnavailable> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct OutputUnavailable(pub &'static str);

impl fmt::Display for OutputUnavailable {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{} output unavailable", self.0)
    }
}

impl std::error::Error for OutputUnavailable {}

pub struct LogOutput;

impl NotificationTransport for LogOutput {
    async fn present(&self, alert: &AlertDelivery) -> Result<(), OutputUnavailable> {
        eprintln!("StillWatch urgent incident {}", alert.incident_id);
        Ok(())
    }
}
