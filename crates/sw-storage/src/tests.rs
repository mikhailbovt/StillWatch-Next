use std::{fs, path::Path};

use rusqlite::{Connection, ErrorCode};
use sw_domain::{
    Assessment, Category, EvidenceStamp, EvidenceState, NewIncident, OperatorAction, SourceScope,
};
use sw_protocol::{CameraConfig, CameraSource, Counter, Dimensions};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    CameraAction, CameraCommand, CameraLimits, ChangeCursor, EvidenceFailure, EvidenceImage,
    EvidenceOutcome, EvidenceWrite, HealthIssue, HealthObservation, OperatorCommand, Store,
    StoreConfig, StoreError,
    db::{DATABASE_FILE, Database},
    restore_backup,
};

const BLACK: &[u8] = include_bytes!("../../../tests/fixtures/evidence/black.jpg");
const WHITE: &[u8] = include_bytes!("../../../tests/fixtures/evidence/white.jpg");

#[tokio::test]
async fn health_intervals_survive_restart_and_respect_camera_revision_and_scope() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let actor = Uuid::new_v4();
    let limits = CameraLimits {
        active: 2,
        raw_bytes: 8_000_000,
    };
    let camera = store
        .mutate_camera(
            CameraCommand {
                actor,
                key: "health-create".parse().unwrap(),
                action: CameraAction::Create(CameraConfig {
                    name: "Camera".into(),
                    source: CameraSource::Replay {
                        file: "reference.mp4".into(),
                        pace: true,
                        repeat: true,
                    },
                    enabled: true,
                    cloud_consent: false,
                    privacy_masks: Vec::new(),
                    controlled_fire_regions: Vec::new(),
                }),
            },
            Some(Dimensions {
                width: 640,
                height: 360,
            }),
            None,
            limits,
            900,
        )
        .await
        .unwrap()
        .camera;
    let frozen = HealthObservation {
        camera: Some((camera.id, camera.revision)),
        issue: HealthIssue::CameraFrozen,
        active: true,
    };
    let disk = HealthObservation::global(HealthIssue::DiskSpaceLow, true);
    store
        .record_health(vec![frozen, disk], 1_000)
        .await
        .unwrap();
    store
        .record_health(vec![frozen, disk], 2_000)
        .await
        .unwrap();
    let newest = store.health_events(None, 1, false, None).await.unwrap();
    assert_eq!(newest.next, Some(Counter(2)));
    assert_eq!(newest.events[0].camera_id, None);
    let scope = Some(vec![camera.id]);
    let older = store
        .health_events(newest.next, 1, false, scope.clone())
        .await
        .unwrap();
    assert_eq!(older.events[0].opened_at.unix_millis(), 1_000);
    assert_eq!(older.events[0].camera_id, Some(camera.id));
    assert_eq!(older.next, None);
    let hidden = store
        .health_events(None, 5, true, Some(vec![Uuid::new_v4()]))
        .await
        .unwrap();
    assert_eq!(hidden.events, newest.events);

    drop(store);
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    assert_eq!(
        store
            .health_events(None, 5, false, scope.clone())
            .await
            .unwrap()
            .events
            .len(),
        2
    );
    store.record_health(vec![frozen], 3_000).await.unwrap();
    store
        .mutate_camera(
            CameraCommand {
                actor,
                key: "health-delete".parse().unwrap(),
                action: CameraAction::Delete {
                    id: camera.id,
                    revision: camera.revision,
                },
            },
            None,
            None,
            limits,
            4_000,
        )
        .await
        .unwrap();
    store.record_health(vec![frozen], 5_000).await.unwrap();
    let active = store
        .health_events(None, 5, true, scope.clone())
        .await
        .unwrap();
    assert_eq!(active.events, newest.events);
    let history = store
        .health_events(None, 5, false, scope.clone())
        .await
        .unwrap();
    assert_eq!(history.events.len(), 2);
    assert_eq!(history.events[1].ended_at.unwrap().unix_millis(), 4_000);

    store.retain_history(4_000, 5_001).await.unwrap();
    let retained = store.health_events(None, 5, false, scope).await.unwrap();
    assert_eq!(retained.events, newest.events);
}

fn incident() -> NewIncident {
    NewIncident {
        id: Uuid::new_v4(),
        scope: SourceScope {
            camera_id: Uuid::new_v4(),
            epoch: 1,
            scene_revision: 1,
        },
        category: Category::Medical,
        reason: "observed posture change".into(),
        analysis_origin: None,
        first_source_us: 0,
        last_source_us: 50_000,
        live: true,
        assessment: Assessment::Concern,
    }
}

fn database(root: &Path, quota: u64) -> Database {
    Database::open(root, quota, watch::channel(()).0).unwrap()
}

fn counts(conn: &Connection) -> (u64, u64, u64) {
    conn.query_row("SELECT (SELECT count(*) FROM incidents), (SELECT count(*) FROM change_events), (SELECT count(*) FROM notification_outbox)", [], |row| {
        Ok((super::db::row_counter(row, 0)?, super::db::row_counter(row, 1)?, super::db::row_counter(row, 2)?))
    }).unwrap()
}

#[tokio::test]
async fn history_retention_preserves_open_incidents_and_advances_the_sse_floor() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let open = store.create(incident(), 1_000).await.unwrap();
    let cursor_before = store.snapshot().await.unwrap().cursor;
    let resolved = store.create(incident(), 2_000).await.unwrap();
    let actor = Uuid::new_v4();
    let resolve = || OperatorCommand {
        incident_id: resolved.id,
        actor_id: actor,
        idempotency_key: "resolve-before-retention".into(),
        expected_revision: resolved.revision,
        action: OperatorAction::Resolve {
            reason: "reviewed".into(),
        },
    };
    let closed = store.apply(resolve(), 3_000).await.unwrap().incident;
    let latest = store.snapshot().await.unwrap().cursor;
    store.retain_history(4_000, 5_000).await.unwrap();
    assert!(store.apply(resolve(), 5_001).await.unwrap().replayed);
    assert_eq!(
        store.get(resolved.id).await.unwrap().revision,
        closed.revision
    );

    let now = 8 * 86_400_000;
    store.retain_history(4_000, now).await.unwrap();
    assert!(matches!(
        store.get(resolved.id).await,
        Err(StoreError::NotFound)
    ));
    assert!(store.get(open.id).await.unwrap().resolution.is_none());
    assert!(matches!(
        store.changes(cursor_before, 100).await,
        Err(StoreError::CursorExpired)
    ));
    assert_eq!(store.snapshot().await.unwrap().cursor, latest);
    assert!(store.changes(latest, 100).await.unwrap().is_empty());
    let next = store.create(incident(), now + 1).await.unwrap();
    let resumed = store.changes(latest, 100).await.unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].incident.id, next.id);
}

#[tokio::test]
async fn semantic_consent_controls_claim_and_invalidates_inflight_evidence() {
    use sw_protocol::{
        CameraConfig, CameraPatch, CameraSource, Dimensions, EvidenceContext, EvidenceCoverage,
        EvidenceInterval, LocalFacts, SemanticRoute,
    };

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let actor = Uuid::new_v4();
    let limits = crate::CameraLimits {
        active: 2,
        raw_bytes: 1024 * 1024,
    };
    let dimensions = Some(Dimensions {
        width: 160,
        height: 120,
    });
    let config: CameraConfig = serde_json::from_value(serde_json::json!({
        "name": "Sample", "enabled": true,
        "source": {"type": "usb", "device_id": "sample"}
    }))
    .unwrap();
    assert!(!config.cloud_consent);
    let camera = store
        .mutate_camera(
            crate::CameraCommand {
                actor,
                key: "create".parse().unwrap(),
                action: crate::CameraAction::Create(config),
            },
            dimensions,
            None,
            limits,
            1_000,
        )
        .await
        .unwrap()
        .camera;
    assert!(matches!(camera.config.source, CameraSource::Usb { .. }));

    let evidence = async |epoch, at| {
        let mut new = incident();
        new.scope.camera_id = camera.id;
        new.scope.epoch = epoch;
        let saved = store.create_hazard(new, at).await.unwrap();
        let stamp = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: 1,
            scope: saved.scope,
            expires_at: 50_000,
        };
        store
            .persist_evidence(
                EvidenceWrite {
                    incident_id: saved.id,
                    expected_revision: saved.revision,
                    stamp,
                    context: Some(EvidenceContext {
                        observed_interval: EvidenceInterval {
                            start_source_s: 0.0,
                            end_source_s: 0.05,
                        },
                        coverage: EvidenceCoverage {
                            expected_samples: 1,
                            received_samples: 1,
                            max_gap_ms: 0,
                        },
                        local_facts: LocalFacts::Medical {
                            track_id: sw_protocol::Counter(1),
                            episode_id: saved.id,
                            previous_episode_id: None,
                            onset_observed: true,
                            identity_uncertain: false,
                            recovery_seconds: 0.0,
                            clear_eligible: false,
                        },
                        limitations: vec![],
                        policy_version: "qa".into(),
                        prompt_version: "qa".into(),
                    }),
                    images: vec![EvidenceImage {
                        source_us: 50_000,
                        kind: sw_protocol::EvidenceImageKind::Overview,
                        jpeg: BLACK.to_vec(),
                    }],
                },
                at + 1,
            )
            .await
            .unwrap();
        stamp
    };
    let patch = async |revision, consent, key: &'static str, at| {
        store
            .mutate_camera(
                crate::CameraCommand {
                    actor,
                    key: key.parse().unwrap(),
                    action: crate::CameraAction::Patch {
                        id: camera.id,
                        revision,
                        patch: CameraPatch {
                            cloud_consent: Some(consent),
                            ..CameraPatch::default()
                        },
                    },
                },
                dimensions,
                None,
                limits,
                at,
            )
            .await
            .unwrap()
            .camera
    };
    let claim = async |route, ready, at| {
        store
            .claim_semantic(route, ready, at, 5, 100, false)
            .await
            .unwrap()
    };

    let default_epoch = store.bump_camera_epoch(camera.id, 1).await.unwrap().epoch;
    let local = evidence(default_epoch, 2_000).await;
    assert!(claim(SemanticRoute::CloudOnly, true, 3_000).await.is_none());
    assert!(
        claim(SemanticRoute::PreferLocal, false, 3_000)
            .await
            .is_none()
    );
    assert!(
        claim(SemanticRoute::LocalOnly, false, 3_000)
            .await
            .is_none()
    );
    let local_work = claim(SemanticRoute::PreferLocal, true, 3_000)
        .await
        .unwrap();
    assert_eq!(local_work.route, SemanticRoute::LocalOnly);
    assert_eq!(local_work.stamp, local);

    let opted_in = patch(1, true, "allow-cloud", 4_000).await;
    assert!(!store.semantic_current(&local_work, 4_000).await.unwrap());
    assert!(
        claim(SemanticRoute::CloudOnly, false, 4_000)
            .await
            .is_none()
    );
    let cloud = evidence(opted_in.epoch, 5_000).await;
    let cloud_work = claim(SemanticRoute::CloudOnly, false, 6_000).await.unwrap();
    assert_eq!(cloud_work.stamp, cloud);
    assert_eq!(cloud_work.route, SemanticRoute::CloudOnly);
    assert!(store.semantic_current(&cloud_work, 6_000).await.unwrap());

    let opted_out = patch(2, false, "revoke-cloud", 7_000).await;
    assert!(opted_out.epoch > opted_in.epoch);
    assert!(!store.semantic_current(&cloud_work, 7_000).await.unwrap());
    patch(3, true, "allow-cloud-again", 8_000).await;
    assert!(!store.semantic_current(&cloud_work, 8_000).await.unwrap());
}

#[test]
fn evidence_retention_tracks_references_across_restarts() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(root.path(), 1024 * 1024);
    let mut references = Vec::new();
    for captured in [1_000, 2_000] {
        let saved = db.create(incident(), captured).unwrap();
        let stamp = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: 1,
            scope: saved.scope,
            expires_at: 5_000,
        };
        db.persist_evidence(
            EvidenceWrite {
                incident_id: saved.id,
                expected_revision: saved.revision,
                stamp,
                context: None,
                images: vec![EvidenceImage {
                    source_us: 50_000,
                    kind: sw_protocol::EvidenceImageKind::Overview,
                    jpeg: BLACK.to_vec(),
                }],
            },
            captured,
        )
        .unwrap();
        references.push((saved.id, stamp.packet_id));
    }
    let (first, first_packet) = references[0];
    let (second, second_packet) = references[1];
    let path = root
        .path()
        .join("blobs")
        .join(format!("{}.jpg", crate::evidence::hex_digest(BLACK)));
    db.retire_evidence(1_500, 2_500).unwrap();
    assert!(path.exists());
    assert_eq!(
        db.get(first).unwrap().evidence_state,
        EvidenceState::Unavailable
    );
    assert!(db.read_evidence(first, first_packet, 0).unwrap().is_none());
    assert_eq!(
        db.read_evidence(second, second_packet, 0).unwrap().unwrap(),
        BLACK
    );
    assert_eq!(
        db.evidence_packet(first, first_packet).unwrap().images[0].sha256,
        Some(crate::evidence::hex_digest(BLACK))
    );
    drop(db);
    let mut db = database(root.path(), 1024 * 1024);
    assert_eq!(
        db.get(first).unwrap().evidence_state,
        EvidenceState::Unavailable
    );
    assert_eq!(
        db.get(second).unwrap().evidence_state,
        EvidenceState::Persisted
    );
    db.retire_evidence(2_500, 3_000).unwrap();
    assert!(!path.exists());
    assert!(
        db.read_evidence(second, second_packet, 0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn incident_change_and_outbox_rollback_together() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(root.path(), 0);
    db.conn.execute_batch("CREATE TEMP TRIGGER fail_outbox BEFORE INSERT ON notification_outbox BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();
    let new = incident();
    assert!(matches!(
        db.create(new.clone(), 1_000),
        Err(StoreError::Sql(_))
    ));
    assert_eq!(counts(&db.conn), (0, 0, 0));
    db.conn.execute_batch("DROP TRIGGER fail_outbox;").unwrap();
    let saved = db.create(new.clone(), 1_000).unwrap();
    assert_eq!(db.create(new, 2_000).unwrap(), saved);
    assert_eq!(counts(&db.conn), (1, 1, 1));
}

#[tokio::test]
async fn scoped_alert_receipts_do_not_acknowledge_and_expired_alerts_do_not_replay() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let item = store.create(incident(), 1_000).await.unwrap();
    let mut replay = incident();
    replay.live = false;
    store.create(replay, 1_100).await.unwrap();
    let actor = Uuid::new_v4();
    let scope = Some(vec![item.scope.camera_id]);
    let scoped = store
        .active_alerts(actor, scope.clone(), 1_500)
        .await
        .unwrap();
    assert_eq!(scoped.alerts.len(), 1);
    let alert = &scoped.alerts[0];
    assert_eq!(alert.incident_id, item.id);
    assert!(
        store
            .active_alerts(actor, Some(vec![]), 1_500)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
    assert!(matches!(
        store
            .record_alert_seen(actor, alert.id, alert.attention_cycle, Some(vec![]), 1_800)
            .await,
        Err(StoreError::NotFound)
    ));
    for now in [2_000, 3_000] {
        store
            .record_alert_seen(actor, alert.id, alert.attention_cycle, scope.clone(), now)
            .await
            .unwrap();
    }
    let delivery = store
        .active_alerts(actor, scope.clone(), 4_000)
        .await
        .unwrap()
        .alerts
        .remove(0);
    assert_eq!(delivery.client_seen_at.unwrap().unix_millis(), 2_000);
    assert!(store.get(item.id).await.unwrap().acknowledgement.is_none());
    let muted = store
        .apply(
            OperatorCommand {
                incident_id: item.id,
                actor_id: actor,
                idempotency_key: "silence".into(),
                expected_revision: item.revision,
                action: OperatorAction::Silence {
                    duration_seconds: 5,
                },
            },
            4_000,
        )
        .await
        .unwrap();
    assert!(
        store
            .active_alerts(actor, scope.clone(), 5_000)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
    assert_eq!(
        store
            .active_alerts(actor, scope.clone(), 9_001)
            .await
            .unwrap()
            .alerts
            .len(),
        1
    );
    store
        .apply(
            OperatorCommand {
                incident_id: item.id,
                actor_id: actor,
                idempotency_key: "ack".into(),
                expected_revision: muted.incident.revision,
                action: OperatorAction::Acknowledge,
            },
            10_000,
        )
        .await
        .unwrap();
    assert!(
        store
            .active_alerts(actor, scope, 10_001)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
    store.expire_alerts(3_601_001).await.unwrap();
    assert!(
        store
            .active_alerts(actor, None, 3_601_001)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
}

#[test]
fn sqlite_full_and_read_only_keep_last_committed_state() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(root.path(), 0);
    db.conn.pragma_update(None, "query_only", true).unwrap();
    assert!(
        matches!(db.create(incident(), 1_000), Err(StoreError::Sql(error)) if error.sqlite_error_code() == Some(ErrorCode::ReadOnly))
    );
    assert_eq!(counts(&db.conn), (0, 0, 0));
    db.conn.pragma_update(None, "query_only", false).unwrap();
    let pages: i64 = db
        .conn
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    db.conn
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    for committed in 0..1000 {
        match db.create(incident(), 1_001 + committed) {
            Ok(_) => {}
            Err(StoreError::Sql(error))
                if error.sqlite_error_code() == Some(ErrorCode::DiskFull) =>
            {
                let n = committed as u64;
                assert_eq!(counts(&db.conn), (n, n, n));
                assert_eq!(db.snapshot(None).unwrap().cursor.sequence, n);
                return;
            }
            Err(error) => panic!("unexpected store error: {error:?}"),
        }
    }
    panic!("test did not reach SQLite page limit");
}

#[tokio::test]
async fn concurrent_retries_are_deduplicated_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    assert!(matches!(
        Store::open(root.path(), StoreConfig::default()),
        Err(StoreError::InUse)
    ));
    let incident = store.create(incident(), 1_000).await.unwrap();
    let actor = Uuid::new_v4();
    let command = || OperatorCommand {
        incident_id: incident.id,
        actor_id: actor,
        idempotency_key: "ack-2".into(),
        expected_revision: 1,
        action: OperatorAction::Acknowledge,
    };
    let (first, second) =
        tokio::join!(store.apply(command(), 2_000), store.apply(command(), 2_000));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.incident, second.incident);
    assert_ne!(first.replayed, second.replayed);
    let identity = store.identity();
    drop(store);
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    assert_eq!(store.identity(), identity);
    let replay = store.apply(command(), 3_000).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.incident, first.incident);
    assert!(matches!(
        store
            .apply(
                OperatorCommand {
                    expected_revision: 2,
                    ..command()
                },
                3_000
            )
            .await,
        Err(StoreError::Idempotency)
    ));
    assert!(matches!(
        store
            .apply(
                OperatorCommand {
                    idempotency_key: "another".into(),
                    ..command()
                },
                3_000
            )
            .await,
        Err(StoreError::Revision { current: 2 })
    ));
    assert_eq!(
        store
            .changes(
                ChangeCursor {
                    generation: identity.generation,
                    sequence: 0
                },
                10
            )
            .await
            .unwrap()
            .len(),
        2
    );
}

fn evidence_write(
    incident: &sw_domain::Incident,
    stamp: EvidenceStamp,
    images: &[&[u8]],
) -> EvidenceWrite {
    EvidenceWrite {
        incident_id: incident.id,
        expected_revision: incident.revision,
        stamp,
        context: None,
        images: images
            .iter()
            .enumerate()
            .map(|(i, jpeg)| EvidenceImage {
                source_us: i as i64,
                kind: sw_protocol::EvidenceImageKind::Overview,
                jpeg: jpeg.to_vec(),
            })
            .collect(),
    }
}

#[test]
fn evidence_commit_failure_leaves_a_reconcilable_orphan() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(root.path(), 1024);
    let incident = db.create(incident(), 1_000).unwrap();
    let stamp = EvidenceStamp {
        packet_id: Uuid::new_v4(),
        version: 1,
        scope: incident.scope,
        expires_at: 9_000,
    };
    db.conn.execute_batch("CREATE TEMP TRIGGER fail_incident BEFORE UPDATE ON incidents BEGIN SELECT RAISE(ABORT, 'injected metadata failure'); END;").unwrap();
    assert!(
        db.persist_evidence(evidence_write(&incident, stamp, &[BLACK]), 2_000)
            .is_err()
    );
    assert_eq!(
        db.get(incident.id).unwrap().evidence_state,
        EvidenceState::Pending
    );
    assert!(
        db.read_evidence(incident.id, stamp.packet_id, 0)
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_dir(&db.blobs).unwrap().count(), 1);
    drop(db);
    let mut reopened = database(root.path(), 1024);
    assert_eq!(fs::read_dir(&reopened.blobs).unwrap().count(), 0);
    assert!(matches!(
        reopened
            .persist_evidence(evidence_write(&incident, stamp, &[BLACK]), 2_000)
            .unwrap(),
        EvidenceOutcome::Persisted(_)
    ));
}

#[test]
fn quota_partial_packet_and_missing_blob_are_visible_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(root.path(), BLACK.len() as u64);
    let incident = db.create(incident(), 1_000).unwrap();
    let stamp = EvidenceStamp {
        packet_id: Uuid::new_v4(),
        version: 1,
        scope: incident.scope,
        expires_at: 9_000,
    };
    let outcome = db
        .persist_evidence(evidence_write(&incident, stamp, &[BLACK, WHITE]), 2_000)
        .unwrap();
    let EvidenceOutcome::Partial(result) = &outcome else {
        panic!("expected partial evidence")
    };
    assert_eq!(result.available, 1);
    assert_eq!(result.failures, vec![(1, EvidenceFailure::Quota)]);
    assert_eq!(
        db.persist_evidence(evidence_write(&incident, stamp, &[BLACK, WHITE]), 2_001)
            .unwrap(),
        outcome
    );
    assert_eq!(
        db.read_evidence(incident.id, stamp.packet_id, 0)
            .unwrap()
            .as_deref(),
        Some(BLACK)
    );
    assert!(
        db.read_evidence(incident.id, stamp.packet_id, 1)
            .unwrap()
            .is_none()
    );
    let file = fs::read_dir(&db.blobs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(file).unwrap();
    drop(db);
    let reopened = database(root.path(), BLACK.len() as u64);
    let changed = reopened.get(incident.id).unwrap();
    assert_eq!(changed.evidence_state, EvidenceState::Unavailable);
    assert_eq!(changed.revision, 3);
}

#[tokio::test]
async fn restore_checks_blob_integrity_and_rotates_stream_generation() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("live"), StoreConfig::default()).unwrap();
    let incident = store.create(incident(), 1_000).await.unwrap();
    let stamp = EvidenceStamp {
        packet_id: Uuid::new_v4(),
        version: 1,
        scope: incident.scope,
        expires_at: 9_000,
    };
    store
        .persist_evidence(evidence_write(&incident, stamp, &[BLACK]), 2_000)
        .await
        .unwrap();
    let cursor = store.snapshot().await.unwrap().cursor;
    let backup = root.path().join("backup");
    store.backup(backup.clone()).await.unwrap();
    let target = root.path().join("restored");
    let identity = restore_backup(&backup, &target).unwrap();
    assert_eq!(identity.instance_id, store.identity().instance_id);
    assert_ne!(identity.generation, cursor.generation);
    let restored = Store::open(&target, StoreConfig::default()).unwrap();
    assert!(matches!(
        restored.changes(cursor, 10).await,
        Err(StoreError::CursorExpired)
    ));
    assert_eq!(
        restored
            .read_evidence(incident.id, stamp.packet_id, 0)
            .await
            .unwrap()
            .as_deref(),
        Some(BLACK)
    );
    drop(restored);
    let conn = Connection::open(target.join(DATABASE_FILE)).unwrap();
    let queued: i64 = conn
        .query_row(
            "SELECT count(*) FROM notification_outbox WHERE state = 'queued'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0);
    let saved_blob = fs::read_dir(backup.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(saved_blob, WHITE).unwrap();
    let rejected = root.path().join("rejected");
    assert!(restore_backup(&backup, &rejected).is_err());
    assert!(!rejected.exists());
}
