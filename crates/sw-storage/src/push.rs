use ring::aead;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use sw_protocol::{
    AlertDelivery, ApnsDelivery, Attention, Counter, IosPushNotice, IosPushRegistration,
};
use uuid::Uuid;

use crate::{
    Store, StoreError,
    alerts::timestamp,
    db::{id, row_counter},
};

const RETRY_DELAYS_MS: [i64; 5] = [6_000, 15_000, 60_000, 300_000, 600_000];

// Both dispatch and notification activation use current server-side permissions.
const ELIGIBLE_ALERT: &str = "
    n.state = 'queued' AND n.attention = 2 AND n.expires_at > ?1 AND n.next_attempt_at <= ?1
    AND i.resolved = 0 AND i.attention = 2 AND json_extract(i.state, '$.live') = 1
    AND json_extract(i.state, '$.acknowledgement') IS NULL
    AND CAST(json_extract(i.state, '$.attention_cycle') AS INTEGER) = n.attention_cycle
    AND r.expires_at > ?1 AND r.registered_at <= n.created_at AND d.revoked_at IS NULL
    AND (json_extract(d.permissions, '$.role') = 'owner'
         OR lower(hex(i.camera_id)) IN (SELECT replace(value, '-', '')
                                          FROM json_each(d.permissions, '$.camera_ids')))
    AND NOT EXISTS (SELECT 1 FROM alert_silences s WHERE s.incident_id = i.id
                   AND s.device_id = r.device_id AND s.attention_cycle = n.attention_cycle AND s.until_utc > ?1)";

pub struct PendingApnsDelivery {
    pub envelope: ApnsDelivery,
    device: Uuid,
    token_revision: Uuid,
}

pub enum ApnsOutcome {
    Accepted,
    Unregistered,
    Retry,
}

impl Store {
    pub async fn register_ios_push(
        &self,
        device: Uuid,
        token: String,
        now: i64,
    ) -> Result<(), StoreError> {
        if !IosPushRegistration::valid_token(&token) {
            return Err(StoreError::Invalid);
        }
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let expiry: i64 = tx.query_row(
                "SELECT s.refresh_expiry FROM trusted_devices d JOIN device_sessions s ON s.device_id = d.id
                   WHERE d.id = ? AND d.revoked_at IS NULL AND s.access_expiry > ? AND s.refresh_expiry > ?",
                params![id(&device), now, now], |row| row.get(0),
            ).optional()?.ok_or(StoreError::Credential)?;
            let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            let unchanged = tx.execute(
                "UPDATE ios_push_registrations SET expires_at = ?
                 WHERE device_id = ? AND token_digest = ? AND expires_at > ?",
                params![expiry, id(&device), &digest[..], now],
            )?;
            if unchanged == 0 {
                let revision = Uuid::new_v4();
                let encrypted = seal(&db.refresh_key, device, revision, token.into_bytes())?;
                tx.execute(
                    "INSERT INTO ios_push_registrations VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(device_id) DO UPDATE SET token_revision = excluded.token_revision,
                        token_digest = excluded.token_digest, encrypted_token = excluded.encrypted_token,
                        registered_at = CASE WHEN ios_push_registrations.expires_at > ?5
                            THEN ios_push_registrations.registered_at ELSE ?5 END,
                        expires_at = excluded.expires_at",
                    params![id(&device), id(&revision), &digest[..], encrypted, now, expiry],
                )?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn unregister_ios_push(&self, device: Uuid) -> Result<(), StoreError> {
        self.call(move |db| {
            db.conn.execute(
                "DELETE FROM ios_push_registrations WHERE device_id = ?",
                [id(&device)],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn take_apns_delivery(
        &self,
        now: i64,
    ) -> Result<Option<PendingApnsDelivery>, StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = tx.query_row(&format!("
                SELECT r.device_id, r.token_revision, r.encrypted_token, n.id, n.expires_at,
                       p.notice, coalesce(p.attempts, 0)
                FROM notification_outbox n JOIN incidents i ON i.id = n.incident_id
                CROSS JOIN ios_push_registrations r JOIN trusted_devices d ON d.id = r.device_id
                LEFT JOIN ios_push_deliveries p ON p.device_id = r.device_id AND p.alert_id = n.id
                WHERE {ELIGIBLE_ALERT} AND p.provider_accepted_at IS NULL
                  AND (p.notice IS NULL OR p.token_revision != r.token_revision
                        OR (p.attempts < 5 AND p.next_attempt_at <= ?1))
                  AND NOT EXISTS (SELECT 1 FROM notification_receipts seen
                      WHERE seen.alert_id = n.id AND seen.device_id = r.device_id)
                ORDER BY coalesce(p.next_attempt_at, n.created_at), n.id, r.device_id LIMIT 1"), [now], |row| {
                Ok((read_uuid(row, 0)?, read_uuid(row, 1)?, row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?, row.get::<_, i64>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?, row.get::<_, u8>(6)?))
            }).optional()?;
            let Some((device, revision, encrypted, alert, expiry, previous_notice, attempts)) = row else {
                return Ok(None);
            };
            let notice = match previous_notice {
                Some(value) => Uuid::from_slice(&value).map_err(|_| StoreError::Invalid)?,
                None => Uuid::new_v4(),
            };
            let token = unseal(&db.refresh_key, device, revision, encrypted)?;
            tx.execute(
                "INSERT INTO ios_push_deliveries (notice, device_id, token_revision, alert_id, attempts, next_attempt_at)
                  VALUES (?1, ?2, ?3, ?4, 1, ?5)
                  ON CONFLICT(device_id, alert_id) DO UPDATE SET token_revision = excluded.token_revision,
                    attempts = CASE WHEN ios_push_deliveries.token_revision = ?3 THEN ios_push_deliveries.attempts + 1 ELSE 1 END,
                    next_attempt_at = CASE WHEN ios_push_deliveries.token_revision = ?3 THEN ?6 ELSE ?5 END",
                params![id(&notice), id(&device), id(&revision), alert,
                        now.saturating_add(RETRY_DELAYS_MS[0]),
                        now.saturating_add(RETRY_DELAYS_MS[usize::from(attempts.min(4))])],
            )?;
            tx.commit()?;
            Ok(Some(PendingApnsDelivery {
                envelope: ApnsDelivery { token, notice, expires_at: timestamp(expiry)? }, device, token_revision: revision,
            }))
        }).await
    }

    pub async fn finish_apns_delivery(
        &self,
        delivery: PendingApnsDelivery,
        now: i64,
        outcome: ApnsOutcome,
    ) -> Result<(), StoreError> {
        let device = delivery.device;
        let revision = delivery.token_revision;
        let notice = delivery.envelope.notice;
        drop(delivery);
        self.call(move |db| {
            match outcome {
                ApnsOutcome::Accepted => db.conn.execute(
                    "UPDATE ios_push_deliveries SET provider_accepted_at = ?1
                     WHERE notice = ?2 AND token_revision = ?3 AND provider_accepted_at IS NULL
                       AND EXISTS (SELECT 1 FROM ios_push_registrations r
                           WHERE r.device_id = ios_push_deliveries.device_id AND r.token_revision = ?3)",
                    params![now, id(&notice), id(&revision)],
                )?,
                ApnsOutcome::Unregistered => db.conn.execute(
                    "DELETE FROM ios_push_registrations WHERE device_id = ? AND token_revision = ?",
                    params![id(&device), id(&revision)],
                )?,
                ApnsOutcome::Retry => 0,
            };
            Ok(())
        }).await
    }

    pub async fn ios_push_notice(
        &self,
        device: Uuid,
        notice: Uuid,
        now: i64,
    ) -> Result<IosPushNotice, StoreError> {
        let identity = self.identity();
        self.call(move |db| {
            let row = db.conn.query_row(&format!("
                SELECT n.id, n.incident_id, i.camera_id, i.revision, n.attention_cycle, n.expires_at, seen.client_seen_at
                FROM ios_push_deliveries p
                JOIN ios_push_registrations r ON p.device_id = r.device_id
                JOIN trusted_devices d ON d.id = r.device_id
                JOIN notification_outbox n ON n.id = p.alert_id JOIN incidents i ON i.id = n.incident_id
                LEFT JOIN notification_receipts seen ON seen.alert_id = n.id AND seen.device_id = r.device_id
                WHERE p.notice = ?2 AND r.device_id = ?3 AND {ELIGIBLE_ALERT}"),
                params![now, id(&notice), id(&device)], |row| {
                    Ok((row_counter(row, 0)?, read_uuid(row, 1)?, read_uuid(row, 2)?, row_counter(row, 3)?,
                        row_counter(row, 4)?, row.get::<_, i64>(5)?, row.get::<_, Option<i64>>(6)?))
                }).optional()?.ok_or(StoreError::NotFound)?;
            Ok(IosPushNotice { generation: identity.generation, alert: AlertDelivery {
                id: Counter(row.0), core_id: identity.instance_id, incident_id: row.1, camera_id: row.2,
                revision: Counter(row.3), attention_cycle: Counter(row.4), attention: Attention::Urgent,
                expires_at: timestamp(row.5)?, client_seen_at: row.6.map(timestamp).transpose()?,
            }})
        }).await
    }
}

fn read_uuid(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Uuid> {
    let bytes: Vec<u8> = row.get(column)?;
    Uuid::from_slice(&bytes).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn aad(device: Uuid, revision: Uuid) -> Vec<u8> {
    let mut data = b"stillwatch/apns-token/v2\0".to_vec();
    data.extend_from_slice(device.as_bytes());
    data.extend_from_slice(revision.as_bytes());
    data
}

fn seal(
    key: &aead::LessSafeKey,
    device: Uuid,
    revision: Uuid,
    mut token: Vec<u8>,
) -> Result<Vec<u8>, StoreError> {
    let mut nonce = [0_u8; aead::NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| StoreError::Unavailable)?;
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(aad(device, revision)),
        &mut token,
    )
    .map_err(|_| StoreError::Unavailable)?;
    let mut envelope = nonce.to_vec();
    envelope.extend_from_slice(&token);
    Ok(envelope)
}

fn unseal(
    key: &aead::LessSafeKey,
    device: Uuid,
    revision: Uuid,
    mut envelope: Vec<u8>,
) -> Result<String, StoreError> {
    if !(30..=540).contains(&envelope.len()) {
        return Err(StoreError::Invalid);
    }
    let (nonce, encrypted) = envelope.split_at_mut(aead::NONCE_LEN);
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| StoreError::Invalid)?;
    let bytes = key
        .open_in_place(nonce, aead::Aad::from(aad(device, revision)), encrypted)
        .map_err(|_| StoreError::Invalid)?;
    let result = std::str::from_utf8(bytes)
        .ok()
        .filter(|value| IosPushRegistration::valid_token(value))
        .map(str::to_owned);
    bytes.fill(0);
    result.ok_or(StoreError::Invalid)
}
