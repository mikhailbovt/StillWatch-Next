use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sw_domain::{
    Assessment, Attention, Category, Clearance, DomainError, EvidenceStamp, EvidenceState,
    Incident, UnixMillis,
};
use sw_protocol::{EvidencePacket, SemanticReport, SemanticRoute};
use uuid::Uuid;

use crate::{
    Store, StoreError,
    db::{Database, get_incident, id, update_incident, verify_camera_scope},
    utc_ms,
};

const LEASE_MS: i64 = 3_000;
const MIN_DISPATCH_MS: i64 = 400;
const DISPATCH_BUDGET_MS: i64 = 15_000;
const UNFINISHED_EXPIRATION_BATCH: u32 = 128;
const OPENING_REVIEW_MS: i64 = 10_000;
const OPENING_OFFER_TTL: Duration = Duration::from_secs(7);

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
    owner: Uuid,
    token: Uuid,
    pub request_id: Uuid,
    pub observed: crate::ObservationTime,
    pub deadline: UnixMillis,
}

pub struct SemanticClaim {
    packet: Uuid,
    incident: Uuid,
    request_id: Uuid,
    owner: Uuid,
    token: Uuid,
    pub route: SemanticRoute,
}

#[derive(Clone, Copy)]
pub struct SemanticSchedule {
    pub owner: Uuid,
    pub route: SemanticRoute,
    pub local_ready: bool,
    pub daily_limit: u32,
    pub minimum_interval_ms: i64,
    pub include_replay: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct OpeningReviewOffer {
    schedule: SemanticSchedule,
    expires: Instant,
}

pub(crate) fn opening_review_deadline(
    conn: &Connection,
    offer: Option<OpeningReviewOffer>,
    incident: &Incident,
    observed: crate::ObservationTime,
    now: UnixMillis,
) -> Result<Option<UnixMillis>, StoreError> {
    let Some(offer) = offer.filter(|offer| offer.expires > Instant::now()) else {
        return Ok(None);
    };
    let schedule = offer.schedule;
    let deadline = observed
        .expires_at()
        .min(now.saturating_add(OPENING_REVIEW_MS));
    if (!incident.live && !schedule.include_replay)
        || incident.attention == Attention::Info
        || incident.category == Category::Overview
        || deadline <= now.saturating_add(MIN_DISPATCH_MS)
        || !crate::cameras::get(conn, incident.scope.camera_id)?
            .config
            .cloud_consent
        || usage(conn, day_start(now))? >= schedule.daily_limit
    {
        return Ok(None);
    }
    let cooling_down: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM semantic_attempts WHERE camera_id = ? AND started_at > ?)",
        params![
            id(&incident.scope.camera_id),
            now.saturating_sub(schedule.minimum_interval_ms)
        ],
        |row| row.get(0),
    )?;
    Ok((!cooling_down).then_some(deadline))
}

pub(crate) fn finish_opening_review(
    conn: &Connection,
    incident: Uuid,
    packet: Option<Uuid>,
) -> Result<bool, StoreError> {
    let changed = conn.execute(
        "DELETE FROM opening_reviews WHERE incident_id = ?1 AND (?2 IS NULL OR packet_id = ?2)",
        params![id(&incident), packet.as_ref().map(id)],
    )?;
    if changed > 0 {
        conn.execute(
            "UPDATE notification_outbox SET next_attempt_at = created_at
             WHERE incident_id = ? AND state = 'queued' AND next_attempt_at > created_at",
            [id(&incident)],
        )?;
    }
    Ok(changed > 0)
}

fn release_opening_reviews(conn: &Connection, deadline: UnixMillis) -> Result<bool, StoreError> {
    conn.execute(
        "UPDATE notification_outbox SET next_attempt_at = created_at
         WHERE incident_id IN (SELECT incident_id FROM opening_reviews WHERE deadline <= ?)
           AND state = 'queued' AND next_attempt_at > created_at",
        [deadline],
    )?;
    Ok(conn.execute(
        "DELETE FROM opening_reviews WHERE deadline <= ?",
        [deadline],
    )? > 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticCompleted {
    Applied {
        source: sw_protocol::InferenceSource,
    },
    Stale,
}

fn route_name(route: SemanticRoute) -> &'static str {
    match route {
        SemanticRoute::CloudOnly => "cloud_only",
        SemanticRoute::LocalOnly => "local_only",
        SemanticRoute::PreferLocal => "prefer_local",
    }
}

fn assessment_name(assessment: Assessment) -> &'static str {
    match assessment {
        Assessment::Unknown => "unknown",
        Assessment::NoVisibleConcern => "no_visible_concern",
        Assessment::Concern => "concern",
        Assessment::ImmediateVisibleConcern => "immediate_visible_concern",
    }
}

impl Database {
    pub(crate) fn recover_semantic_jobs(&mut self) -> Result<(), StoreError> {
        // Opening the database owns the exclusive Core instance lock. This
        // process cannot verify the previous process's monotonic frame time.
        let now = utc_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        release_opening_reviews(&tx, i64::MAX)?;
        tx.execute(
            "UPDATE semantic_attempts SET finished_at = ?, outcome = 'deadline'
             WHERE finished_at IS NULL AND report IS NULL
               AND EXISTS(SELECT 1 FROM semantic_jobs j WHERE j.packet_id = semantic_attempts.packet_id
                 AND j.state = 'dispatched')",
            [now],
        )?;
        tx.execute_batch(
            "UPDATE semantic_jobs SET state = 'expired', lease_until = NULL
             WHERE state IN ('queued', 'claimed');
             UPDATE semantic_jobs SET state = 'unknown_outcome', dispatch_until = NULL
             WHERE state = 'dispatched';",
        )?;
        if !self.semantic_jobs_enabled {
            tx.execute(
                "UPDATE semantic_attempts SET finished_at = ?, outcome = 'stale'
                 WHERE finished_at IS NULL AND packet_id IN (
                    SELECT packet_id FROM semantic_jobs WHERE state = 'response_ready'
                 )",
                [now],
            )?;
            tx.execute(
                "UPDATE semantic_jobs SET state = 'completed' WHERE state = 'response_ready'",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn expire_semantic_jobs(&mut self, now: UnixMillis) -> Result<(), StoreError> {
        let expired: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM semantic_jobs j JOIN evidence_packets p ON p.id = j.packet_id
               WHERE j.state IN ('queued', 'claimed') AND p.expires_at <= ?1)
             OR EXISTS(SELECT 1 FROM semantic_jobs j WHERE j.state = 'dispatched' AND j.dispatch_until <= ?1)
             OR EXISTS(SELECT 1 FROM opening_reviews WHERE deadline <= ?1)",
            [now],
            |r| r.get(0),
        )?;
        if !expired {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let released = release_opening_reviews(&tx, now)?;
        tx.execute(
            "UPDATE semantic_attempts SET finished_at = ?1, outcome = 'deadline' WHERE packet_id IN (
                SELECT packet_id FROM semantic_jobs WHERE state = 'dispatched' AND dispatch_until <= ?1
                ORDER BY dispatch_until, packet_id LIMIT ?2)
             AND finished_at IS NULL",
            params![now, UNFINISHED_EXPIRATION_BATCH],
        )?;
        tx.execute(
            "UPDATE semantic_jobs SET state = 'unknown_outcome', dispatch_until = NULL WHERE packet_id IN (
                SELECT packet_id FROM semantic_jobs WHERE state = 'dispatched' AND dispatch_until <= ?1
                ORDER BY dispatch_until, packet_id LIMIT ?2)",
            params![now, UNFINISHED_EXPIRATION_BATCH],
        )?;
        tx.execute(
            "UPDATE semantic_jobs SET state = 'expired', lease_until = NULL WHERE packet_id IN (
                SELECT j.packet_id FROM semantic_jobs j JOIN evidence_packets p ON p.id = j.packet_id
                WHERE j.state IN ('queued', 'claimed') AND p.expires_at <= ?1
                ORDER BY p.expires_at, j.packet_id LIMIT ?2)",
            params![now, UNFINISHED_EXPIRATION_BATCH],
        )?;
        tx.commit()?;
        if released {
            self.notify.send_replace(());
        }
        Ok(())
    }
}

impl Store {
    /// Offer an opening check while the worker has a ready Firebase route.
    /// A lost supervisor stops reserving new checks when its offer expires.
    pub async fn set_opening_review_schedule(
        &self,
        schedule: Option<SemanticSchedule>,
    ) -> Result<(), StoreError> {
        self.call(move |db| {
            db.opening_review_offer = schedule
                .filter(|schedule| {
                    db.semantic_jobs_enabled && schedule.route != SemanticRoute::LocalOnly
                })
                .map(|schedule| OpeningReviewOffer {
                    schedule,
                    expires: Instant::now() + OPENING_OFFER_TTL,
                });
            if db.opening_review_offer.is_none() {
                let tx = db
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let released = release_opening_reviews(&tx, i64::MAX)?;
                tx.commit()?;
                if released {
                    db.notify.send_replace(());
                }
            }
            Ok(())
        })
        .await
    }

    pub async fn semantic_usage(&self, day_start: UnixMillis) -> Result<u32, StoreError> {
        self.call(move |db| {
            db.expire_semantic_jobs(utc_ms())?;
            usage(&db.conn, day_start)
        })
        .await
    }

    pub async fn claim_semantic(
        &self,
        schedule: SemanticSchedule,
    ) -> Result<Option<SemanticClaim>, StoreError> {
        if schedule.route == SemanticRoute::LocalOnly && !schedule.local_ready {
            return Ok(None);
        }
        let owner = schedule.owner;
        let token = Uuid::new_v4();
        self.call(move |db| {
            let now = utc_ms();
            let monotonic = Instant::now();
            db.evidence_monotonic.retain(|_, time| time.evidence_fresh(now, monotonic));
            if db.evidence_monotonic.is_empty() {
                return Ok(None);
            }
            let active = serde_json::to_string(
                &db.evidence_monotonic.keys().map(|packet| packet.simple().to_string()).collect::<Vec<_>>()
            )?;
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = utc_ms();
            if usage(&tx, day_start(now))? >= schedule.daily_limit {
                return Ok(None);
            }
            let candidate = tx.query_row(
                "SELECT p.id, p.incident_id, j.request_id,
                        CASE WHEN review.deadline IS NULL THEN p.expires_at ELSE min(review.deadline, p.expires_at) END,
                        review.deadline IS NOT NULL
                 FROM json_each(?6) active
                 JOIN evidence_packets p ON p.id = unhex(active.value)
                 JOIN semantic_jobs j ON j.packet_id = p.id
                 JOIN incidents i ON i.id = p.incident_id
                 JOIN cameras c ON i.camera_id = c.id
                 LEFT JOIN opening_reviews review ON review.packet_id = p.id AND ?4 != 'local_only'
                   AND review.deadline > ?1 + 400 AND json_type(c.config, '$.cloud_consent') IS 'true'
                 WHERE p.context IS NOT NULL AND p.expires_at >= ?1 + 400 AND p.semantic_eligible = 1
                   AND (j.state = 'queued' OR (j.state = 'claimed' AND j.lease_until <= ?1))
                   AND i.resolved = 0
                   AND i.camera_hazard = 1 AND c.deleted = 0 AND c.enabled = 1
                   AND c.source_epoch = json_extract(i.state, '$.scope.epoch')
                   AND c.scene_revision = json_extract(i.state, '$.scope.scene_revision')
                   AND p.version = json_extract(i.state, '$.evidence.version')
                   AND json_extract(i.state, '$.evidence_state') = 'persisted'
                   AND EXISTS (SELECT 1 FROM evidence_images img WHERE img.packet_id = p.id
                     AND img.kind = 'overview' AND NOT img.retired AND img.digest IS NOT NULL
                     AND img.source_us >= json_extract(i.state, '$.last_source_us'))
                   AND coalesce(json_extract(i.state, '$.assessed_evidence_version'), 0) < p.version
                   AND (?2 OR json_extract(i.state, '$.live') = 1)
                   AND (review.deadline IS NOT NULL OR (?4 != 'cloud_only' AND ?5))
                   AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.packet_id = p.id)
                   AND NOT EXISTS (SELECT 1 FROM semantic_attempts a WHERE a.camera_id = c.id AND a.started_at > ?3)
                   AND (SELECT count(*) FROM semantic_attempts a WHERE a.incident_id = i.id) < 3
                 ORDER BY review.deadline IS NULL, p.captured_at, p.id LIMIT 1",
                params![now, schedule.include_replay, now.saturating_sub(schedule.minimum_interval_ms),
                    route_name(schedule.route), schedule.local_ready, active],
                |r| Ok((read_uuid(r, 0)?, read_uuid(r, 1)?, read_uuid(r, 2)?, r.get::<_, i64>(3)?, r.get::<_, bool>(4)?)),
            ).optional()?;
            let Some((packet, incident, request_id, expires_at, opening_review)) = candidate else {
                return Ok(None);
            };
            let route = if opening_review { schedule.route } else { SemanticRoute::LocalOnly };
            tx.execute(
                "UPDATE semantic_jobs SET state = 'claimed', lease_owner = ?, lease_token = ?, lease_until = ?, route = ?
                 WHERE packet_id = ?",
                params![id(&owner), id(&token), expires_at.min(now.saturating_add(LEASE_MS)), route_name(route), id(&packet)],
            )?;
            tx.commit()?;
            Ok(Some(SemanticClaim { packet, incident, request_id, owner, token, route }))
        }).await
    }

    pub async fn load_semantic(
        &self,
        claim: &SemanticClaim,
    ) -> Result<Option<SemanticWork>, StoreError> {
        let (packet, incident, owner, token, route, request_id) = (
            claim.packet,
            claim.incident,
            claim.owner,
            claim.token,
            claim.route,
            claim.request_id,
        );
        self.call(move |db| {
            let now = utc_ms();
            let Some(observed) = db
                .evidence_monotonic
                .get(&packet)
                .copied()
                .filter(|time| time.evidence_fresh(now, Instant::now()))
            else {
                return Ok(None);
            };
            if !owns_lease(&db.conn, packet, owner, token, now)? {
                return Ok(None);
            }
            let item = get_incident(&db.conn, incident)?;
            let Some(stamp) = item.evidence.filter(|stamp| stamp.packet_id == packet) else {
                return Ok(None);
            };
            if current(&db.conn, incident, stamp, route, now)?.is_none() {
                return Ok(None);
            }
            let packet = db.evidence_packet(incident, packet)?;
            let deadline = semantic_deadline(&db.conn, stamp, route)?;
            let Some(deadline) = deadline else {
                return Ok(None);
            };
            Ok(Some(SemanticWork {
                packet,
                stamp,
                route,
                owner,
                token,
                request_id,
                observed,
                deadline,
            }))
        })
        .await
    }

    pub async fn semantic_current(&self, work: &SemanticWork) -> Result<bool, StoreError> {
        let (incident, stamp, route, owner, token) = (
            work.packet.incident_id,
            work.stamp,
            work.route,
            work.owner,
            work.token,
        );
        self.call(move |db| {
            let now = utc_ms();
            let dispatched: bool = db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM semantic_jobs WHERE packet_id = ?
                 AND state = 'dispatched' AND lease_owner = ? AND lease_token = ? AND dispatch_until > ?)",
                params![id(&stamp.packet_id), id(&owner), id(&token), now],
                |r| r.get(0),
            )?;
            Ok(dispatched && fresh(db, stamp.packet_id, now)
                && current(&db.conn, incident, stamp, route, now)?.is_some())
        }).await
    }

    pub async fn release_semantic(
        &self,
        claim: &SemanticClaim,
        expire: bool,
    ) -> Result<(), StoreError> {
        let (packet, incident, owner, token) =
            (claim.packet, claim.incident, claim.owner, claim.token);
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "UPDATE semantic_jobs SET state = ?, lease_owner = NULL, lease_token = NULL, lease_until = NULL, route = NULL
                 WHERE packet_id = ? AND state = 'claimed' AND lease_owner = ? AND lease_token = ?",
                params![if expire { "expired" } else { "queued" }, id(&packet), id(&owner), id(&token)],
            )?;
            let released = expire && changed > 0 && finish_opening_review(&tx, incident, Some(packet))?;
            tx.commit()?;
            if released {
                db.notify.send_replace(());
            }
            Ok(())
        }).await
    }

    /// The only transition that charges budget. After it commits, the request
    /// can be sent once. A lost or ambiguous response is never re-sent.
    pub async fn dispatch_semantic(
        &self,
        work: &SemanticWork,
        schedule: SemanticSchedule,
    ) -> Result<Option<u32>, StoreError> {
        let (incident, stamp, route, owner, token, request, deadline) = (
            work.packet.incident_id,
            work.stamp,
            work.route,
            work.owner,
            work.token,
            work.request_id,
            work.deadline,
        );
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = utc_ms();
            if now.saturating_add(MIN_DISPATCH_MS) >= deadline
                || !db.evidence_monotonic.get(&stamp.packet_id)
                    .is_some_and(|time| time.evidence_fresh(now, Instant::now()))
                || owner != schedule.owner
                || !owns_lease(&tx, stamp.packet_id, owner, token, now)?
                || current(&tx, incident, stamp, route, now)?.is_none()
            {
                return Ok(None);
            }
            let used = usage(&tx, day_start(now))?;
            if used >= schedule.daily_limit {
                return Ok(None);
            }
            let available: bool = tx.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM semantic_attempts WHERE packet_id = ?1 OR (camera_id = ?2 AND started_at > ?3))
                    AND (SELECT count(*) FROM semantic_attempts WHERE incident_id = ?4) < 3",
                params![id(&stamp.packet_id), id(&stamp.scope.camera_id), now.saturating_sub(schedule.minimum_interval_ms), id(&incident)],
                |r| r.get(0),
            )?;
            if !available {
                return Ok(None);
            }
            tx.execute(
                "UPDATE semantic_jobs SET state = 'dispatched', lease_until = NULL, dispatch_until = ? WHERE request_id = ?",
                params![deadline.min(now.saturating_add(DISPATCH_BUDGET_MS)), id(&request)],
            )?;
            tx.execute(
                "INSERT INTO semantic_attempts(packet_id, route, incident_id, camera_id, started_at) VALUES (?, ?, ?, ?, ?)",
                params![id(&stamp.packet_id), route_name(route), id(&incident), id(&stamp.scope.camera_id), now],
            )?;
            tx.commit()?;
            Ok(Some(used + 1))
        }).await
    }

    pub async fn semantic_failed(
        &self,
        work: &SemanticWork,
        outcome: SemanticOutcome,
    ) -> Result<(), StoreError> {
        let (packet, incident, owner, token) = (
            work.stamp.packet_id,
            work.packet.incident_id,
            work.owner,
            work.token,
        );
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let state = match outcome {
                SemanticOutcome::Unavailable | SemanticOutcome::Deadline => "unknown_outcome",
                SemanticOutcome::Invalid => "completed",
                SemanticOutcome::Stale => "expired",
            };
            let changed = tx.execute(
                "UPDATE semantic_jobs SET state = ?, dispatch_until = NULL WHERE packet_id = ? AND state = 'dispatched'
                 AND lease_owner = ? AND lease_token = ?",
                params![state, id(&packet), id(&owner), id(&token)],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE semantic_attempts SET finished_at = ?, outcome = ?
                     WHERE packet_id = ? AND finished_at IS NULL",
                    params![utc_ms(), outcome.as_str(), id(&packet)],
                )?;
            }
            let released = changed == 1 && finish_opening_review(&tx, incident, Some(packet))?;
            tx.commit()?;
            if released {
                db.notify.send_replace(());
            }
            Ok(())
        }).await
    }

    pub async fn save_semantic_response(
        &self,
        work: &SemanticWork,
        report: &SemanticReport,
        assessment: Assessment,
    ) -> Result<(), StoreError> {
        let (packet, owner, token) = (work.stamp.packet_id, work.owner, work.token);
        let report = serde_json::to_string(report)?;
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "UPDATE semantic_jobs SET state = 'response_ready', dispatch_until = NULL, projected_assessment = ?
                 WHERE packet_id = ? AND state IN ('dispatched', 'unknown_outcome') AND lease_owner = ? AND lease_token = ?",
                params![assessment_name(assessment), id(&packet), id(&owner), id(&token)],
            )?;
            if changed == 1 {
                if tx.execute(
                    "UPDATE semantic_attempts SET report = ?, finished_at = NULL, outcome = NULL
                     WHERE packet_id = ? AND report IS NULL",
                    params![&report, id(&packet)],
                )? != 1 {
                    return Err(StoreError::Schema);
                }
            } else {
                let saved: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM semantic_jobs j JOIN semantic_attempts a ON a.packet_id = j.packet_id
                     WHERE j.packet_id = ? AND j.lease_owner = ? AND j.lease_token = ? AND j.projected_assessment = ?
                     AND j.state IN ('response_ready', 'completed') AND a.report = ?)",
                    params![id(&packet), id(&owner), id(&token), assessment_name(assessment), &report],
                    |r| r.get(0),
                )?;
                if !saved {
                    return Err(StoreError::Idempotency);
                }
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn finish_semantic_response(&self) -> Result<Option<SemanticCompleted>, StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate = tx.query_row(
                "SELECT j.packet_id, a.incident_id, a.route, a.report, j.projected_assessment
                 FROM semantic_jobs j JOIN semantic_attempts a ON a.packet_id = j.packet_id
                 WHERE j.state = 'response_ready' ORDER BY j.packet_id LIMIT 1",
                [],
                |r| Ok((read_uuid(r, 0)?, read_uuid(r, 1)?, r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?, r.get::<_, String>(4)?)),
            ).optional()?;
            let Some((packet, incident, route, report, assessment)) = candidate else { return Ok(None); };
            let now = utc_ms();
            let eligible = db.evidence_monotonic.get(&packet)
                .is_some_and(|time| time.evidence_fresh(now, Instant::now()));
            let transition = if eligible {
                let route = match route.as_str() {
                    "cloud_only" => SemanticRoute::CloudOnly,
                    "local_only" => SemanticRoute::LocalOnly,
                    "prefer_local" => SemanticRoute::PreferLocal,
                    _ => return Err(StoreError::Schema),
                };
                let item = get_incident(&tx, incident)?;
                if let Some(stamp) = item.evidence.filter(|stamp| stamp.packet_id == packet) {
                    current(&tx, incident, stamp, route, now)?.map(|incident| {
                        let assessment = serde_json::from_value(serde_json::Value::String(assessment))?;
                        let clearance = if assessment == Assessment::NoVisibleConcern
                            && route != SemanticRoute::LocalOnly
                        {
                            Clearance::ReviewedFireOpening
                        } else {
                            Clearance::Unsubstantiated
                        };
                        let assessment = if assessment == Assessment::NoVisibleConcern
                            && clearance == Clearance::Unsubstantiated
                        {
                            Assessment::Unknown
                        } else {
                            assessment
                        };
                        Ok::<_, StoreError>(incident.record_semantic(stamp, assessment, clearance, now)?)
                    }).transpose()?
                } else {
                    None
                }
            } else {
                None
            };
            let result = if let Some(transition) = transition {
                let report: SemanticReport = serde_json::from_str(&report)?;
                update_incident(&tx, &transition)?;
                if transition.incident.attention == Attention::Info {
                    tx.execute(
                        "UPDATE notification_outbox SET state = 'expired'
                         WHERE incident_id = ? AND attention = 'review' AND state = 'queued'",
                        [id(&incident)],
                    )?;
                }
                SemanticCompleted::Applied { source: report.inference_source }
            } else {
                SemanticCompleted::Stale
            };
            tx.execute("UPDATE semantic_jobs SET state = 'completed' WHERE packet_id = ?", [id(&packet)])?;
            let outcome = if matches!(result, SemanticCompleted::Applied { .. }) { "applied" } else { "stale" };
            tx.execute(
                "UPDATE semantic_attempts SET outcome = ?, finished_at = ? WHERE packet_id = ? AND finished_at IS NULL",
                params![outcome, now, id(&packet)],
            )?;
            let released = finish_opening_review(&tx, incident, Some(packet))?;
            tx.commit()?;
            if released || matches!(result, SemanticCompleted::Applied { .. }) {
                db.notify.send_replace(());
            }
            Ok(Some(result))
        })
        .await
    }
}

fn read_uuid(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Uuid> {
    let value: [u8; 16] = row.get(column)?;
    Ok(Uuid::from_bytes(value))
}

fn day_start(now: UnixMillis) -> UnixMillis {
    now.div_euclid(86_400_000) * 86_400_000
}

fn usage(conn: &Connection, start: UnixMillis) -> Result<u32, StoreError> {
    Ok(conn.query_row(
        "SELECT count(*) FROM semantic_attempts WHERE started_at >= ?",
        [start],
        |r| r.get(0),
    )?)
}

fn fresh(db: &Database, packet: Uuid, now: UnixMillis) -> bool {
    db.evidence_monotonic
        .get(&packet)
        .is_some_and(|time| time.evidence_fresh(now, Instant::now()))
}

fn owns_lease(
    conn: &Connection,
    packet: Uuid,
    owner: Uuid,
    token: Uuid,
    now: UnixMillis,
) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM semantic_jobs WHERE packet_id = ? AND state = 'claimed'
          AND lease_owner = ? AND lease_token = ? AND lease_until > ?)",
        params![id(&packet), id(&owner), id(&token), now],
        |r| r.get(0),
    )?)
}

fn current(
    conn: &Connection,
    incident_id: Uuid,
    stamp: EvidenceStamp,
    route: SemanticRoute,
    now: UnixMillis,
) -> Result<Option<Incident>, StoreError> {
    if !semantic_deadline(conn, stamp, route)?.is_some_and(|deadline| deadline > now) {
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
    let evidence_source = conn.query_row(
        "SELECT max(source_us) FROM evidence_images
         WHERE packet_id = ? AND kind = 'overview' AND NOT retired AND digest IS NOT NULL",
        [id(&stamp.packet_id)],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let eligible = incident.evidence == Some(stamp)
        && incident.evidence_state == EvidenceState::Persisted
        && evidence_source.is_some_and(|source| source >= incident.last_source_us)
        && incident.assessed_evidence_version.unwrap_or(0) < stamp.version
        && incident.resolution.is_none();
    Ok(eligible.then_some(incident))
}

fn semantic_deadline(
    conn: &Connection,
    stamp: EvidenceStamp,
    route: SemanticRoute,
) -> Result<Option<UnixMillis>, StoreError> {
    if route == SemanticRoute::LocalOnly {
        return Ok(Some(stamp.expires_at));
    }
    Ok(conn
        .query_row(
            "SELECT min(deadline, ?) FROM opening_reviews WHERE packet_id = ?",
            params![stamp.expires_at, id(&stamp.packet_id)],
            |row| row.get(0),
        )
        .optional()?)
}
