use rusqlite::{TransactionBehavior, params};
use sw_domain::UnixMillis;

use crate::{Store, StoreError};

impl Store {
    /// One bounded pass. A full batch schedules an earlier follow-up.
    pub async fn maintenance_step(
        &self,
        evidence_cutoff: UnixMillis,
        history_cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<bool, StoreError> {
        let alerts = self.expire_alerts_step(now).await;
        let evidence = self.retire_evidence_step(evidence_cutoff, now).await;
        let history = self.retain_history_step(history_cutoff, now).await;
        let openings = self.call(|db| db.recover_openings()).await;
        Ok(alerts? | evidence? | history? | openings?)
    }

    /// Remove expired administrative records and ended health and incident history.
    /// Open incidents and unexpired evidence stay available for review.
    pub async fn retain_history(
        &self,
        cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.retain_history_step(cutoff, now).await.map(|_| ())
    }

    pub async fn retain_history_step(
        &self,
        cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<bool, StoreError> {
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
                       SELECT 1 FROM evidence_packets p
                       WHERE p.incident_id = i.id AND p.has_retirable_images = 1
                   )
                 ORDER BY i.updated_at, i.id LIMIT 128",
                [incident_cutoff],
            )?;
            let (history_debt, floor_changed) = if expired > 0 {
                purge_incident_history(&tx)?
            } else {
                (false, false)
            };
            let mut debt = expired == 128 || history_debt;
            debt |= tx.execute(
                "DELETE FROM artifacts WHERE digest IN (
                    SELECT a.digest FROM artifacts a WHERE a.available = 0
                      AND NOT EXISTS (SELECT 1 FROM evidence_images e WHERE e.digest = a.digest)
                      AND NOT EXISTS (SELECT 1 FROM retired_blob_cleanup c WHERE c.digest = a.digest)
                      ORDER BY a.digest LIMIT 512)",
                [],
            )? == 512;
            for (table, field) in [
                ("idempotency_records", "expires_at"),
                ("pairing_grants", "expires_at"),
                ("retired_refresh_credentials", "expires_at"),
                ("refresh_receipts", "expires_at"),
                ("device_sessions", "refresh_expiry"),
            ] {
                let statement = format!(
                    "DELETE FROM {table} WHERE rowid IN (
                       SELECT rowid FROM {table} WHERE {field} <= ? ORDER BY {field}, rowid LIMIT 512)"
                );
                debt |= tx.execute(&statement, [now])? == 512;
            }
            debt |= tx.execute("DELETE FROM health_events WHERE sequence IN (
                SELECT sequence FROM health_events WHERE ended_at <= ? ORDER BY ended_at, sequence LIMIT 512)", [cutoff])? == 512;
            debt |= tx.execute("DELETE FROM trust_audit WHERE sequence IN (
                SELECT sequence FROM trust_audit WHERE occurred_at <= ? ORDER BY occurred_at, sequence LIMIT 512)", [cutoff])? == 512;
            debt |= tx.execute("DELETE FROM trusted_devices WHERE id IN (
                SELECT id FROM trusted_devices WHERE revoked_at IS NOT NULL AND revoked_at <= ?
                  AND NOT EXISTS (SELECT 1 FROM device_sessions WHERE device_id = trusted_devices.id)
                  AND NOT EXISTS (SELECT 1 FROM retired_refresh_credentials WHERE device_id = trusted_devices.id)
                  ORDER BY revoked_at, id LIMIT 128)", [cutoff])? == 128;
            debt |= tx.execute("DELETE FROM camera_config_versions WHERE (camera_id, revision) IN (
                SELECT v.camera_id, v.revision FROM camera_config_versions v JOIN cameras c ON c.id = v.camera_id
                WHERE c.deleted = 1 AND v.revision = c.revision AND v.changed_at <= ?
                  AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.camera_id = c.id)
                ORDER BY v.changed_at, v.camera_id LIMIT 128)", [incident_cutoff])? == 128;
            debt |= tx.execute("DELETE FROM camera_config_versions WHERE (camera_id, revision) IN (
                SELECT v.camera_id, v.revision FROM camera_config_versions v JOIN cameras c ON c.id = v.camera_id
                WHERE v.changed_at <= ?1 AND v.revision < c.revision
                ORDER BY v.changed_at, v.camera_id, v.revision LIMIT ?2)", params![cutoff, 512])? == 512;
            debt |= tx.execute("DELETE FROM cameras WHERE id IN (
                SELECT c.id FROM cameras c WHERE c.deleted = 1
                  AND NOT EXISTS (SELECT 1 FROM camera_config_versions v WHERE v.camera_id = c.id)
                  AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.camera_id = c.id)
                LIMIT 128)", [])? == 128;
            tx.commit()?;
            if floor_changed {
                db.notify.send_replace(());
            }
            Ok(debt)
        }).await
    }
}

fn purge_incident_history(tx: &rusqlite::Transaction<'_>) -> Result<(bool, bool), StoreError> {
    let mut debt = false;
    for table in [
        "alert_silences",
        "operator_actions",
        "legacy_assessments",
        "legacy_feedback",
        "legacy_packets",
    ] {
        let statement = format!(
            "DELETE FROM {table} WHERE rowid IN (
                SELECT t.rowid FROM expired_incidents i JOIN {table} t ON t.incident_id = i.id
                LIMIT 512)"
        );
        debt |= tx.execute(&statement, [])? == 512;
    }
    debt |= tx.execute(
        "DELETE FROM notification_receipts WHERE rowid IN (
            SELECT r.rowid FROM expired_incidents i
            JOIN notification_outbox a ON a.incident_id = i.id
            JOIN notification_receipts r ON r.alert_id = a.id LIMIT 512)",
        [],
    )? == 512;
    debt |= tx.execute(
        "DELETE FROM notification_outbox WHERE id IN (
            SELECT a.id FROM expired_incidents i JOIN notification_outbox a ON a.incident_id = i.id
            WHERE NOT EXISTS (SELECT 1 FROM notification_receipts r WHERE r.alert_id = a.id)
            LIMIT 512)",
        [],
    )? == 512;
    debt |= tx.execute(
        "DELETE FROM hazard_observations WHERE (incident_id, source_us) IN (
            SELECT h.incident_id, h.source_us
            FROM expired_incidents i JOIN hazard_observations h ON h.incident_id = i.id LIMIT 512)",
        [],
    )? == 512;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS expired_packets(id BLOB PRIMARY KEY) WITHOUT ROWID;
         DELETE FROM expired_packets;",
    )?;
    let packets = tx.execute(
        "INSERT INTO expired_packets SELECT p.id
         FROM expired_incidents i JOIN evidence_packets p ON p.incident_id = i.id LIMIT 128",
        [],
    )?;
    debt |= packets == 128;
    tx.execute_batch(
        "DELETE FROM semantic_jobs WHERE packet_id IN (SELECT id FROM expired_packets);
         DELETE FROM semantic_attempts WHERE packet_id IN (SELECT id FROM expired_packets);
         DELETE FROM evidence_images WHERE packet_id IN (SELECT id FROM expired_packets);
         DELETE FROM evidence_packets WHERE id IN (SELECT id FROM expired_packets);",
    )?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS expired_changes(sequence INTEGER PRIMARY KEY);
         DELETE FROM expired_changes;",
    )?;
    let changes = tx.execute(
        "INSERT INTO expired_changes SELECT c.sequence
         FROM expired_incidents i JOIN change_events c ON c.incident_id = i.id LIMIT 512",
        [],
    )?;
    if changes > 0 {
        tx.execute_batch(
            "UPDATE core_identity SET change_floor = max(change_floor, (SELECT max(sequence) FROM expired_changes))
             WHERE singleton = 1;
             DELETE FROM change_events WHERE sequence IN (SELECT sequence FROM expired_changes);",
        )?;
    }
    debt |= changes == 512;
    if !debt {
        tx.execute_batch(
            "DELETE FROM legacy_incidents WHERE incident_id IN (SELECT id FROM expired_incidents);
             DELETE FROM incidents WHERE id IN (SELECT id FROM expired_incidents);",
        )?;
    }
    Ok((debt, changes > 0))
}
