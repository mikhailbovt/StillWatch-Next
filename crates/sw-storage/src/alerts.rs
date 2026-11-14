use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sw_domain::UnixMillis;
use sw_protocol::{AlertDelivery, Attention, Counter, UtcTimestamp};
use uuid::Uuid;

use crate::{
    Store, StoreError,
    cameras::camera_filter,
    db::{id, row_counter, sql_counter},
};

pub const MAX_ALERT_PAGE: usize = 128;
const AUDIT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Clone, Copy, Debug)]
pub struct AlertScan {
    pub head: Counter,
    pub attention: u8,
    pub id: Counter,
}

pub struct StoredAlertPage {
    pub alerts: Vec<AlertDelivery>,
    pub next: Option<AlertScan>,
    pub head: Counter,
}

impl Store {
    pub async fn active_alerts(
        &self,
        actor: Uuid,
        cameras: Option<Vec<Uuid>>,
        now: UnixMillis,
    ) -> Result<sw_protocol::AlertFeed, StoreError> {
        let page = self
            .active_alert_page(actor, cameras, now, None, MAX_ALERT_PAGE)
            .await?;
        Ok(sw_protocol::AlertFeed {
            alerts: page.alerts,
            next: None,
            observed_at: Some(timestamp(now)?),
            generation: Some(self.identity().generation),
            head: Some(page.head),
        })
    }

    pub async fn active_alert_page(
        &self,
        actor: Uuid,
        cameras: Option<Vec<Uuid>>,
        now: UnixMillis,
        after: Option<AlertScan>,
        limit: usize,
    ) -> Result<StoredAlertPage, StoreError> {
        if !(1..=MAX_ALERT_PAGE).contains(&limit) {
            return Err(StoreError::Invalid);
        }
        let filter = camera_filter(cameras)?;
        let instance = self.identity().instance_id;
        self.call(move |db| {
            let current = db.conn.query_row("SELECT head FROM notification_sequence WHERE singleton = 1", [], |row| row_counter(row, 0))?;
            let (head, attention, last) = match after {
                Some(after) if after.head.0 <= current && (1..=2).contains(&after.attention)
                    && (1..=after.head.0).contains(&after.id.0) => (after.head.0, after.attention, after.id.0),
                Some(_) => return Err(StoreError::CursorExpired),
                None => (current, 3, 0),
            };
            let mut query = db.conn.prepare(
                "SELECT n.id, n.incident_id, i.camera_id, i.revision, n.attention_cycle, n.attention, n.expires_at, r.client_seen_at
                 FROM notification_outbox n JOIN incidents i ON i.id = n.incident_id
                 LEFT JOIN alert_silences s ON s.incident_id = i.id AND s.device_id = ?1 AND s.attention_cycle = n.attention_cycle
                 LEFT JOIN notification_receipts r ON r.alert_id = n.id AND r.device_id = ?1
                 WHERE n.state = 'queued' AND n.expires_at > ?2 AND n.next_attempt_at <= ?2
                   AND i.resolved = 0 AND i.attention = n.attention
                   AND json_extract(i.state, '$.acknowledgement') IS NULL
                   AND coalesce(s.until_utc, 0) <= ?2
                   AND (?3 IS NULL OR lower(hex(i.camera_id)) IN (SELECT value FROM json_each(?3)))
                   AND n.id <= ?4 AND (n.attention < ?5 OR (n.attention = ?5 AND n.id > ?6))
                 ORDER BY n.attention DESC, n.id ASC LIMIT ?7",
            )?;
            let rows = query.query_map(params![id(&actor), now, filter, sql_counter(head)?, attention, sql_counter(last)?, limit as i64 + 1], |row| {
                Ok((
                    row_counter(row, 0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row_counter(row, 3)?,
                    row_counter(row, 4)?,
                    row.get::<_, u8>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })?;
            let mut alerts = Vec::new();
            for row in rows {
                let (alert, incident, camera, revision, cycle, attention, expiry, seen) = row?;
                alerts.push(AlertDelivery {
                    id: Counter(alert),
                    core_id: instance,
                    incident_id: parse_id(&incident)?,
                    camera_id: parse_id(&camera)?,
                    revision: Counter(revision),
                    attention_cycle: Counter(cycle),
                    attention: match attention {
                        1 => Attention::Review,
                        2 => Attention::Urgent,
                        _ => return Err(StoreError::Invalid),
                    },
                    expires_at: timestamp(expiry)?,
                    client_seen_at: seen.map(timestamp).transpose()?,
                });
            }
            let next = if alerts.len() > limit {
                alerts.pop();
                alerts.last().map(|alert| AlertScan {
                    head: Counter(head),
                    attention: if alert.attention == Attention::Urgent { 2 } else { 1 },
                    id: alert.id,
                })
            } else {
                None
            };
            Ok(StoredAlertPage { alerts, next, head: Counter(head) })
        }).await
    }

    pub async fn record_alert_seen(
        &self,
        actor: Uuid,
        alert: Counter,
        cycle: Counter,
        cameras: Option<Vec<Uuid>>,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        let filter = camera_filter(cameras)?;
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row_id = sql_counter(alert.0)?;
            let allowed = tx.query_row(
                "SELECT 1 FROM notification_outbox n JOIN incidents i ON i.id = n.incident_id
                 WHERE n.id = ?1 AND n.attention_cycle = ?2 AND n.expires_at > ?3
                   AND (EXISTS(SELECT 1 FROM notification_receipts WHERE alert_id = n.id AND device_id = ?5)
                        OR (n.state = 'queued' AND n.next_attempt_at <= ?3
                            AND i.resolved = 0 AND i.attention = n.attention
                            AND json_extract(i.state, '$.acknowledgement') IS NULL
                            AND NOT EXISTS(SELECT 1 FROM alert_silences s WHERE s.incident_id = i.id
                                AND s.device_id = ?5 AND s.attention_cycle = n.attention_cycle AND s.until_utc > ?3)))
                   AND (?4 IS NULL OR lower(hex(i.camera_id)) IN (SELECT value FROM json_each(?4)))",
                params![row_id, sql_counter(cycle.0)?, now, filter, id(&actor)],
                |_| Ok(()),
            ).optional()?;
            if allowed.is_none() {
                return Err(StoreError::NotFound);
            }
            tx.execute(
                "INSERT INTO notification_receipts(alert_id, device_id, client_seen_at) VALUES (?, ?, ?)
                 ON CONFLICT(alert_id, device_id) DO NOTHING",
                params![row_id, id(&actor), now],
            )?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn expire_alerts(&self, now: UnixMillis) -> Result<(), StoreError> {
        self.expire_alerts_step(now).await.map(|_| ())
    }

    pub async fn expire_alerts_step(&self, now: UnixMillis) -> Result<bool, StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let expired = tx.execute(
                "UPDATE notification_outbox SET state = 'expired' WHERE id IN (
                  SELECT id FROM notification_outbox WHERE state = 'queued' AND expires_at <= ?
                  ORDER BY expires_at, id LIMIT 512)",
                [now],
            )?;
            let cutoff = now.saturating_sub(AUDIT_RETENTION_MS);
            let receipts = tx.execute(
                "DELETE FROM notification_receipts WHERE rowid IN (
                  SELECT r.rowid FROM notification_outbox n
                  JOIN notification_receipts r ON r.alert_id = n.id
                  WHERE n.state = 'expired' AND n.expires_at <= ? LIMIT 512)",
                [cutoff],
            )?;
            let removed = tx.execute(
                "DELETE FROM notification_outbox WHERE id IN (
                  SELECT n.id FROM notification_outbox n WHERE n.state = 'expired' AND n.expires_at <= ?
                    AND NOT EXISTS (SELECT 1 FROM notification_receipts r WHERE r.alert_id = n.id)
                  ORDER BY n.expires_at, n.id LIMIT 512)",
                [cutoff],
            )?;
            let silences = tx.execute(
                "DELETE FROM alert_silences WHERE rowid IN (
                  SELECT rowid FROM alert_silences WHERE until_utc <= ? ORDER BY until_utc, rowid LIMIT 512)",
                [now],
            )?;
            tx.execute("DELETE FROM ios_push_registrations WHERE expires_at <= ?", [now])?;
            tx.commit()?;
            Ok([expired, receipts, removed, silences].contains(&512))
        }).await
    }
}

fn parse_id(data: &[u8]) -> Result<Uuid, StoreError> {
    Uuid::from_slice(data).map_err(|_| StoreError::Invalid)
}

pub(crate) fn timestamp(ms: UnixMillis) -> Result<UtcTimestamp, StoreError> {
    UtcTimestamp::from_unix_millis(ms).map_err(|_| StoreError::Invalid)
}
