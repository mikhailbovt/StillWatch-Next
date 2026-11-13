use ed25519_dalek::{Signature, VerifyingKey};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use sw_domain::UnixMillis;
use sw_protocol::{
    Device, DeviceCredentials, DeviceRole, DeviceScope, PairingFinish, PairingRequest,
    RefreshRequest, UtcTimestamp, pairing_message, refresh_message,
};
use uuid::Uuid;

use crate::{Store, StoreError, db::id};

const PAIR_TTL_MS: i64 = 5 * 60 * 1000;
const ACCESS_TTL_MS: i64 = 10 * 60 * 1000;
const REFRESH_TTL_MS: i64 = 14 * 24 * 60 * 60 * 1000;
const MAX_GRANTS: u32 = 16;
const MAX_DEVICES: u32 = 128;

pub struct PairingGrant {
    pub challenge: Uuid,
    pub secret: String,
    pub expires_at: UtcTimestamp,
}

#[derive(Clone)]
pub struct DeviceAccess {
    pub device: Device,
    pub expires_at: i64,
}

impl DeviceAccess {
    pub fn owner(&self) -> bool {
        self.device.role == DeviceRole::Owner
    }

    pub fn allows(&self, scope: DeviceScope) -> bool {
        self.owner() || self.device.scopes.contains(&scope)
    }

    pub fn camera(&self, id: Uuid) -> bool {
        self.owner() || self.device.camera_ids.contains(&id)
    }
}

impl Store {
    pub async fn issue_pairing(
        &self,
        actor: Uuid,
        request: PairingRequest,
        now: UnixMillis,
    ) -> Result<PairingGrant, StoreError> {
        if !request.valid() {
            return Err(StoreError::Invalid);
        }
        let challenge = Uuid::new_v4();
        let secret = random_token()?;
        let secret_hash = hash(&secret);
        let expiry = now.saturating_add(PAIR_TTL_MS);
        self.call(move |db| {
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM pairing_grants WHERE expires_at <= ?", [now])?;
            let pending: u32 =
                tx.query_row("SELECT count(*) FROM pairing_grants", [], |r| r.get(0))?;
            if pending >= MAX_GRANTS {
                return Err(StoreError::DeviceCapacity);
            }
            for camera in &request.camera_ids {
                if !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cameras WHERE id = ? AND deleted = 0)",
                    [id(camera)],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Err(StoreError::Invalid);
                }
            }
            tx.execute(
                "INSERT INTO pairing_grants VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    id(&challenge),
                    &secret_hash[..],
                    serde_json::to_string(&request)?,
                    id(&actor),
                    now,
                    expiry
                ],
            )?;
            audit(&tx, actor, None, "pairing_grant", now)?;
            tx.commit()?;
            Ok(PairingGrant {
                challenge,
                secret,
                expires_at: timestamp(expiry)?,
            })
        })
        .await
    }

    pub async fn finish_pairing(
        &self,
        finish: PairingFinish,
        now: UnixMillis,
    ) -> Result<DeviceCredentials, StoreError> {
        if !token_format(&finish.secret)
            || finish.challenge.is_nil()
            || finish.label.trim().is_empty()
            || finish.label.chars().count() > 80
            || finish.label.chars().any(char::is_control)
        {
            return Err(StoreError::Credential);
        }
        let public = decode_hex::<32>(&finish.public_key)?;
        let signature = Signature::from_bytes(&decode_hex::<64>(&finish.signature)?);
        let device = Uuid::new_v4();
        let tokens = Tokens::new(now)?;
        let instance = self.identity().instance_id;
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (stored_hash, permissions, issued_by) = tx.query_row(
                "SELECT secret_hash, permissions, issued_by FROM pairing_grants WHERE challenge = ? AND expires_at > ?",
                params![id(&finish.challenge), now],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?)),
            ).optional()?.ok_or(StoreError::Credential)?;
            if !bool::from(stored_hash.ct_eq(&hash(&finish.secret))) {
                return Err(StoreError::Credential);
            }
            verify(&public, &pairing_message(instance, finish.challenge, &finish.secret, &finish.label), &signature)?;
            let count: u32 = tx.query_row("SELECT count(*) FROM trusted_devices WHERE revoked_at IS NULL", [], |r| r.get(0))?;
            if count >= MAX_DEVICES { return Err(StoreError::DeviceCapacity); }
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM trusted_devices WHERE public_key = ? AND revoked_at IS NULL)",
                [&public[..]], |r| r.get::<_, bool>(0),
            )? {
                return Err(StoreError::Credential);
            }
            tx.execute("DELETE FROM pairing_grants WHERE challenge = ?", [id(&finish.challenge)])?;
            tx.execute("INSERT INTO trusted_devices VALUES (?, ?, ?, ?, ?, NULL)",
                params![id(&device), &public[..], finish.label, permissions, now])?;
            tokens.store(&tx, device)?;
            audit(&tx, parse_id(&issued_by)?, Some(device), "enrolled", now)?;
            let record = read_device(&tx, device)?.ok_or(StoreError::Schema)?;
            tx.commit()?;
            tokens.response(record)
        }).await
    }

    pub async fn refresh_device(
        &self,
        request: RefreshRequest,
        now: UnixMillis,
    ) -> Result<DeviceCredentials, StoreError> {
        if request.nonce.is_nil() || !token_format(&request.refresh_token) {
            return Err(StoreError::Credential);
        }
        let signature = Signature::from_bytes(&decode_hex::<64>(&request.signature)?);
        let old = hash(&request.refresh_token);
        let next = Tokens::new(now)?;
        let instance = self.identity().instance_id;
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let record = read_device(&tx, request.device_id)?.filter(|device| device.revoked_at.is_none())
                .ok_or(StoreError::Credential)?;
            let (public, digest, expires) = tx.query_row(
                "SELECT d.public_key, s.refresh_hash, s.refresh_expiry FROM trusted_devices d
                 JOIN device_sessions s ON d.id = s.device_id WHERE d.id = ?",
                [id(&record.id)], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, i64>(2)?)),
            ).optional()?.ok_or(StoreError::Credential)?;
            verify(&public.try_into().map_err(|_| StoreError::Schema)?,
                &refresh_message(instance, request.device_id, request.nonce, &request.refresh_token), &signature)?;
            if !bool::from(digest.ct_eq(&old)) {
                let reused = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM retired_refresh_credentials WHERE digest = ? AND device_id = ? AND expires_at > ?)",
                    params![&old[..], id(&record.id), now], |r| r.get::<_, bool>(0),
                )?;
                if reused {
                    revoke(&tx, record.id, now)?;
                    audit(&tx, record.id, Some(record.id), "refresh_reuse", now)?;
                    tx.commit()?;
                    db.notify.send_replace(());
                }
                return Err(StoreError::Credential);
            }
            if expires <= now { return Err(StoreError::Credential); }
            tx.execute("DELETE FROM retired_refresh_credentials WHERE expires_at <= ?", [now])?;
            tx.execute("INSERT INTO retired_refresh_credentials VALUES (?, ?, ?)",
                params![&old[..], id(&record.id), expires])?;
            next.store(&tx, record.id)?;
            tx.commit()?;
            db.notify.send_replace(());
            next.response(record)
        }).await
    }

    pub async fn device_access(
        &self,
        token: &str,
        now: UnixMillis,
    ) -> Result<DeviceAccess, StoreError> {
        if !token_format(token) {
            return Err(StoreError::Credential);
        }
        let digest = hash(token);
        self.call(move |db| {
            let (id, expires_at) = db.conn.query_row(
                "SELECT device_id, access_expiry FROM device_sessions WHERE access_hash = ? AND access_expiry > ?",
                params![&digest[..], now], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)),
            ).optional()?.ok_or(StoreError::Credential)?;
            let device = read_device(&db.conn, parse_id(&id)?)?.filter(|d| d.revoked_at.is_none())
                .ok_or(StoreError::Credential)?;
            Ok(DeviceAccess { device, expires_at })
        }).await
    }

    pub async fn devices(&self) -> Result<Vec<Device>, StoreError> {
        self.call(move |db| {
            let mut statement = db.conn.prepare(
                "SELECT id, label, permissions, paired_at, revoked_at FROM trusted_devices
                 ORDER BY (revoked_at IS NOT NULL), paired_at DESC, id DESC LIMIT 256",
            )?;
            let rows = statement.query_map([], device_row)?;
            rows.map(|r| convert(r?)).collect()
        })
        .await
    }

    pub async fn revoked_devices(&self, devices: Vec<Uuid>) -> Result<Vec<Uuid>, StoreError> {
        if devices.len() > MAX_DEVICES as usize {
            return Err(StoreError::Invalid);
        }
        self.call(move |db| {
            let mut query = db.conn.prepare(
                "SELECT EXISTS(SELECT 1 FROM trusted_devices WHERE id = ? AND revoked_at IS NULL)",
            )?;
            let mut revoked = Vec::new();
            for device in devices {
                if !query.query_row([id(&device)], |row| row.get::<_, bool>(0))? {
                    revoked.push(device);
                }
            }
            Ok(revoked)
        })
        .await
    }

    pub async fn revoke_device(
        &self,
        actor: Uuid,
        device: Uuid,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.call(move |db| {
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if revoke(&tx, device, now)? {
                audit(&tx, actor, Some(device), "revoked", now)?;
            }
            tx.commit()?;
            db.notify.send_replace(());
            Ok(())
        })
        .await
    }
}

fn revoke(tx: &Transaction<'_>, device: Uuid, now: UnixMillis) -> Result<bool, StoreError> {
    let changed = tx.execute(
        "UPDATE trusted_devices SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
        params![now, id(&device)],
    )?;
    tx.execute(
        "DELETE FROM device_sessions WHERE device_id = ?",
        [id(&device)],
    )?;
    tx.execute(
        "DELETE FROM retired_refresh_credentials WHERE device_id = ?",
        [id(&device)],
    )?;
    tx.execute(
        "DELETE FROM pairing_grants WHERE issued_by = ?",
        [id(&device)],
    )?;
    Ok(changed != 0)
}

fn audit(
    tx: &Transaction<'_>,
    actor: Uuid,
    device: Option<Uuid>,
    operation: &str,
    now: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO trust_audit(actor_id, device_id, operation, occurred_at) VALUES (?, ?, ?, ?)",
        params![id(&actor), device.as_ref().map(id), operation, now],
    )?;
    Ok(())
}

type DeviceRow = (Vec<u8>, String, String, i64, Option<i64>);

fn device_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn convert((id, label, permissions, paired, revoked): DeviceRow) -> Result<Device, StoreError> {
    let permissions: PairingRequest = serde_json::from_str(&permissions)?;
    if !permissions.valid() {
        return Err(StoreError::Schema);
    }
    Ok(Device {
        id: parse_id(&id)?,
        label,
        role: permissions.role,
        camera_ids: permissions.camera_ids,
        scopes: permissions.scopes,
        paired_at: timestamp(paired)?,
        revoked_at: revoked.map(timestamp).transpose()?,
    })
}

fn read_device(db: &Connection, device: Uuid) -> Result<Option<Device>, StoreError> {
    db.query_row(
        "SELECT id, label, permissions, paired_at, revoked_at FROM trusted_devices WHERE id = ?",
        [id(&device)],
        device_row,
    )
    .optional()?
    .map(convert)
    .transpose()
}

fn timestamp(ms: i64) -> Result<UtcTimestamp, StoreError> {
    UtcTimestamp::from_unix_millis(ms).map_err(|_| StoreError::Schema)
}

fn parse_id(bytes: &[u8]) -> Result<Uuid, StoreError> {
    Uuid::from_slice(bytes).map_err(|_| StoreError::Schema)
}

fn verify(public: &[u8; 32], bytes: &[u8], signature: &Signature) -> Result<(), StoreError> {
    let public = VerifyingKey::from_bytes(public).map_err(|_| StoreError::Credential)?;
    public
        .verify_strict(bytes, signature)
        .map_err(|_| StoreError::Credential)
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn token_format(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn random_token() -> Result<String, StoreError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| StoreError::Unavailable)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn decode_hex<const N: usize>(text: &str) -> Result<[u8; N], StoreError> {
    if text.len() != N * 2 || !text.is_ascii() {
        return Err(StoreError::Credential);
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| StoreError::Credential)?;
    }
    Ok(bytes)
}

struct Tokens {
    access: String,
    refresh: String,
    access_expiry: i64,
    refresh_expiry: i64,
}

impl Tokens {
    fn new(now: i64) -> Result<Self, StoreError> {
        Ok(Self {
            access: random_token()?,
            refresh: random_token()?,
            access_expiry: now.saturating_add(ACCESS_TTL_MS),
            refresh_expiry: now.saturating_add(REFRESH_TTL_MS),
        })
    }

    fn store(&self, tx: &Transaction<'_>, device: Uuid) -> Result<(), StoreError> {
        tx.execute(
            "INSERT INTO device_sessions VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(device_id) DO UPDATE SET access_hash = excluded.access_hash,
             access_expiry = excluded.access_expiry, refresh_hash = excluded.refresh_hash,
             refresh_expiry = excluded.refresh_expiry",
            params![
                id(&device),
                &hash(&self.access)[..],
                self.access_expiry,
                &hash(&self.refresh)[..],
                self.refresh_expiry
            ],
        )?;
        Ok(())
    }

    fn response(self, device: Device) -> Result<DeviceCredentials, StoreError> {
        Ok(DeviceCredentials {
            device,
            access_token: self.access,
            access_expires_at: timestamp(self.access_expiry)?,
            refresh_token: self.refresh,
            refresh_expires_at: timestamp(self.refresh_expiry)?,
        })
    }
}
