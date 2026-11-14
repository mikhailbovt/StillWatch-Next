use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sw_domain::{
    Assessment, Clearance, DomainError, EvidenceStamp, EvidenceState, Incident, UnixMillis,
};
use sw_protocol::{EvidencePacket, SemanticReport, SemanticRoute};
use uuid::Uuid;

use crate::{
    Store, StoreError,
    db::{get_incident, id, update_incident, verify_camera_scope},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticOutcome {
    Unavailable,
    Invalid,
    Deadline,
    Stale,
}

impl SemanticOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Invalid => "invalid",
            Self::Deadline => "deadline",
            Self::Stale => "stale",
        }
    }
}

pub struct SemanticWork {
    pub packet: EvidencePacket,
    pub stamp: EvidenceStamp,
    pub route: SemanticRoute,
}

fn route_name(route: SemanticRoute) -> &'static str {
    match route {
        SemanticRoute::CloudOnly => "cloud_only",
        SemanticRoute::LocalOnly => "local_only",
        SemanticRoute::PreferLocal => "prefer_local",
    }
}

impl Store {
    pub async fn semantic_usage(&self, day_start: UnixMillis) -> Result<u32, StoreError> {
        self.call(move |db| {
            Ok(db.conn.query_row(
                "SELECT count(*) FROM semantic_attempts WHERE started_at >= ?",
                [day_start],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn claim_semantic(
        &self,
        route: SemanticRoute,
        local_ready: bool,
        now: UnixMillis,
        daily_limit: u32,
        minimum_interval_ms: i64,
        include_replay: bool,
    ) -> Result<Option<SemanticWork>, StoreError> {
        if route == SemanticRoute::LocalOnly && !local_ready {
            return Ok(None);
        }
        let day_start = now.div_euclid(86_400_000) * 86_400_000;
        self.call(move |db| {
            let now = now.max(crate::utc_ms());
            let day_start = day_start.max(now.div_euclid(86_400_000) * 86_400_000);
            let monotonic = Instant::now();
            db.evidence_monotonic.retain(|_, time| time.evidence_fresh(now, monotonic));
            if db.evidence_monotonic.is_empty() {
                return Ok(None);
            }
            let active = serde_json::to_string(
                &db.evidence_monotonic.keys().map(|packet| packet.simple().to_string()).collect::<Vec<_>>()
            )?;
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let used: u32 = tx.query_row("SELECT count(*) FROM semantic_attempts WHERE started_at >= ?", [day_start], |row| row.get(0))?;
            if used >= daily_limit {
                return Ok(None);
            }
            let candidate = tx.query_row(
                "SELECT p.id, p.incident_id, json_type(c.config, '$.cloud_consent') IS 'true'
                 FROM json_each(?6) active
                 JOIN evidence_packets p ON p.id = unhex(active.value)
                 JOIN incidents i ON i.id = p.incident_id
                 JOIN cameras c ON i.camera_id = c.id
                 WHERE p.context IS NOT NULL AND p.expires_at > ?1 AND p.semantic_eligible = 1
                   AND i.resolved = 0
                   AND i.camera_hazard = 1 AND c.deleted = 0 AND c.enabled = 1
                   AND c.source_epoch = json_extract(i.state, '$.scope.epoch')
                   AND c.scene_revision = json_extract(i.state, '$.scope.scene_revision')
                   AND p.version = json_extract(i.state, '$.evidence.version')
                   AND json_extract(i.state, '$.evidence_state') = 'persisted'
                   AND coalesce(json_extract(i.state, '$.assessed_evidence_version'), 0) < p.version
                   AND (?2 OR json_extract(i.state, '$.live') = 1)
                   AND (?4 = 'local_only' OR json_type(c.config, '$.cloud_consent') IS 'true'
                        OR (?4 = 'prefer_local' AND ?5))
                   AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.packet_id = p.id)
                   AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.camera_id = c.id AND a.started_at > ?3)
                   AND (SELECT count(*) FROM semantic_attempts a WHERE a.incident_id = i.id) < 3
                 ORDER BY p.captured_at, p.id LIMIT 1",
                params![now, include_replay, now.saturating_sub(minimum_interval_ms), route_name(route), local_ready, active],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, bool>(2)?)),
            ).optional()?;
            let Some((packet_id, incident_id, consent)) = candidate else { return Ok(None); };
            let route = if consent { route } else { SemanticRoute::LocalOnly };
            let packet_id = Uuid::from_slice(&packet_id).map_err(|_| StoreError::Schema)?;
            let incident_id = Uuid::from_slice(&incident_id).map_err(|_| StoreError::Schema)?;
            let incident = get_incident(&tx, incident_id)?;
            let stamp = incident.evidence.filter(|stamp| stamp.packet_id == packet_id).ok_or(StoreError::Schema)?;
            tx.execute("INSERT INTO semantic_attempts(packet_id, route, incident_id, camera_id, started_at) VALUES (?, ?, ?, ?, ?)",
                params![id(&packet_id), route_name(route), id(&incident_id), id(&incident.scope.camera_id), now])?;
            tx.commit()?;
            let packet = db.evidence_packet(incident_id, packet_id)?;
            Ok(Some(SemanticWork { packet, stamp, route }))
        }).await
    }

    pub async fn semantic_current(
        &self,
        work: &SemanticWork,
        now: UnixMillis,
    ) -> Result<bool, StoreError> {
        let incident = work.packet.incident_id;
        let stamp = work.stamp;
        let route = work.route;
        self.call(move |db| {
            let now = now.max(crate::utc_ms());
            if !db
                .evidence_monotonic
                .get(&stamp.packet_id)
                .is_some_and(|time| time.evidence_fresh(now, Instant::now()))
            {
                return Ok(false);
            }
            Ok(current(&db.conn, incident, stamp, route, now)?.is_some())
        })
        .await
    }

    pub async fn semantic_failed(
        &self,
        stamp: EvidenceStamp,
        route: SemanticRoute,
        outcome: SemanticOutcome,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.call(move |db| {
            db.conn.execute(
                "UPDATE semantic_attempts SET finished_at = ?, outcome = ? WHERE packet_id = ? AND route = ? AND finished_at IS NULL",
                params![now, outcome.as_str(), id(&stamp.packet_id), route_name(route)],
            )?;
            Ok(())
        }).await
    }

    pub async fn semantic_completed(
        &self,
        work: SemanticWork,
        report: SemanticReport,
        assessment: Assessment,
        now: UnixMillis,
    ) -> Result<bool, StoreError> {
        let stamp = work.stamp;
        let incident_id = work.packet.incident_id;
        let route = work.route;
        self.call(move |db| {
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = now.max(crate::utc_ms());
            let fresh = db
                .evidence_monotonic
                .get(&stamp.packet_id)
                .is_some_and(|time| time.evidence_fresh(now, Instant::now()));
            let current = if fresh {
                current(&tx, incident_id, stamp, route, now)?
            } else {
                None
            };
            let transition = current
                .map(|incident| {
                    incident.record_semantic(stamp, assessment, Clearance::Unsubstantiated, now)
                })
                .transpose()?;
            let changed = tx.execute(
                "UPDATE semantic_attempts SET finished_at = ?, outcome = ?, report = ?
                 WHERE packet_id = ? AND route = ? AND incident_id = ? AND finished_at IS NULL",
                params![
                    now,
                    if transition.is_some() {
                        "applied"
                    } else {
                        "stale"
                    },
                    transition
                        .as_ref()
                        .map(|_| serde_json::to_string(&report))
                        .transpose()?,
                    id(&stamp.packet_id),
                    route_name(route),
                    id(&incident_id)
                ],
            )?;
            if changed != 1 {
                return Ok(false);
            }
            let applied = transition.is_some();
            if let Some(transition) = transition {
                update_incident(&tx, &transition)?;
            }
            tx.commit()?;
            if applied {
                db.notify.send_replace(());
            }
            Ok(applied)
        })
        .await
    }
}

fn current(
    conn: &Connection,
    incident_id: Uuid,
    stamp: EvidenceStamp,
    route: SemanticRoute,
    now: UnixMillis,
) -> Result<Option<Incident>, StoreError> {
    if stamp.expires_at <= now {
        return Ok(None);
    }
    if !conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM evidence_packets WHERE id = ? AND semantic_eligible = 1)",
        [id(&stamp.packet_id)],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(None);
    }
    match verify_camera_scope(conn, stamp.scope) {
        Ok(()) => {}
        Err(StoreError::Domain(DomainError::Stale)) => return Ok(None),
        Err(error) => return Err(error),
    }
    if route != SemanticRoute::LocalOnly
        && !crate::cameras::get(conn, stamp.scope.camera_id)?
            .config
            .cloud_consent
    {
        return Ok(None);
    }
    let incident = get_incident(conn, incident_id)?;
    let eligible = incident.evidence == Some(stamp)
        && incident.evidence_state == EvidenceState::Persisted
        && incident.assessed_evidence_version.unwrap_or(0) < stamp.version
        && incident.resolution.is_none();
    Ok(eligible.then_some(incident))
}
