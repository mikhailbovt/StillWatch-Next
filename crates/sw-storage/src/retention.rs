use rusqlite::{TransactionBehavior, params};
use sw_domain::UnixMillis;

use crate::{Store, StoreError};

impl Store {
    /// Remove expired administrative records and ended health and incident history.
    /// Open incidents and unexpired evidence stay available for review.
    pub async fn retain_history(
        &self,
        cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Preserve replayable command results for the full idempotency window.
            let incident_cutoff = cutoff.min(now.saturating_sub(crate::db::IDEMPOTENCY_TTL));
            tx.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS expired_incidents(id BLOB PRIMARY KEY) WITHOUT ROWID;
                 DELETE FROM expired_incidents;",
            )?;
            let expired = tx.execute(
                "INSERT INTO expired_incidents
                 SELECT i.id FROM incidents i
                 WHERE i.resolved = 1 AND i.updated_at <= ?
                   AND NOT EXISTS (
                       SELECT 1 FROM evidence_packets p JOIN evidence_images e ON e.packet_id = p.id
                       WHERE p.incident_id = i.id AND e.digest IS NOT NULL AND e.retired = 0
                   )
                 ORDER BY i.updated_at, i.id LIMIT 128",
                [incident_cutoff],
            )?;
            tx.execute_batch(
                "DELETE FROM semantic_jobs WHERE packet_id IN (
                    SELECT id FROM evidence_packets WHERE incident_id IN (SELECT id FROM expired_incidents));
                 DELETE FROM semantic_attempts WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM alert_silences WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM notification_outbox WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM operator_actions WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM legacy_assessments WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM legacy_feedback WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM legacy_packets WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM legacy_incidents WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM evidence_images WHERE packet_id IN (
                    SELECT id FROM evidence_packets WHERE incident_id IN (SELECT id FROM expired_incidents));
                 DELETE FROM evidence_packets WHERE incident_id IN (SELECT id FROM expired_incidents);
                 UPDATE core_identity SET change_floor = max(change_floor, coalesce((
                    SELECT max(sequence) FROM change_events WHERE incident_id IN (SELECT id FROM expired_incidents)
                 ), 0)) WHERE singleton = 1;
                 DELETE FROM change_events WHERE incident_id IN (SELECT id FROM expired_incidents);
                 DELETE FROM incidents WHERE id IN (SELECT id FROM expired_incidents);
                 DELETE FROM artifacts WHERE digest IN (
                    SELECT a.digest FROM artifacts a WHERE a.available = 0
                      AND NOT EXISTS (SELECT 1 FROM evidence_images e WHERE e.digest = a.digest)
                      ORDER BY a.digest LIMIT 512);",
            )?;
            tx.execute("DELETE FROM idempotency_records WHERE expires_at <= ?", [now])?;
            tx.execute("DELETE FROM pairing_grants WHERE expires_at <= ?", [now])?;
            tx.execute("DELETE FROM retired_refresh_credentials WHERE expires_at <= ?", [now])?;
            tx.execute("DELETE FROM device_sessions WHERE refresh_expiry <= ?", [now])?;
            tx.execute("DELETE FROM health_events WHERE sequence IN (
                SELECT sequence FROM health_events WHERE ended_at <= ? ORDER BY ended_at, sequence LIMIT 512)", [cutoff])?;
            tx.execute("DELETE FROM trust_audit WHERE sequence IN (
                SELECT sequence FROM trust_audit WHERE occurred_at <= ? ORDER BY occurred_at, sequence LIMIT 512)", [cutoff])?;
            tx.execute("DELETE FROM trusted_devices WHERE id IN (
                SELECT id FROM trusted_devices WHERE revoked_at IS NOT NULL AND revoked_at <= ?
                  ORDER BY revoked_at, id LIMIT 128)", [cutoff])?;
            tx.execute("DELETE FROM camera_config_versions WHERE (camera_id, revision) IN (
                SELECT v.camera_id, v.revision FROM camera_config_versions v JOIN cameras c ON c.id = v.camera_id
                WHERE c.deleted = 1 AND v.revision = c.revision AND v.changed_at <= ?
                  AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.camera_id = c.id)
                ORDER BY v.changed_at, v.camera_id LIMIT 128)", [incident_cutoff])?;
            tx.execute("DELETE FROM camera_config_versions WHERE (camera_id, revision) IN (
                SELECT v.camera_id, v.revision FROM camera_config_versions v JOIN cameras c ON c.id = v.camera_id
                WHERE v.changed_at <= ?1 AND v.revision < c.revision
                ORDER BY v.changed_at, v.camera_id, v.revision LIMIT ?2)", params![cutoff, 512])?;
            tx.execute("DELETE FROM cameras WHERE id IN (
                SELECT c.id FROM cameras c WHERE c.deleted = 1
                  AND NOT EXISTS (SELECT 1 FROM camera_config_versions v WHERE v.camera_id = c.id)
                  AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.camera_id = c.id)
                LIMIT 128)", [])?;
            tx.commit()?;
            if expired > 0 {
                db.notify.send_replace(());
            }
            Ok(())
        }).await
    }
}
