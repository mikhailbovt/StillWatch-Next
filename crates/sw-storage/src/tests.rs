use std::{fs, path::Path};

use rusqlite::{Connection, ErrorCode};
use sw_domain::{
    Assessment, Category, EvidenceStamp, EvidenceState, NewIncident, OperatorAction, SourceScope,
};
use sw_protocol::{CameraConfig, CameraSource, Counter, Dimensions};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    BackupLimits, CameraAction, CameraCommand, CameraLimits, ChangeCursor, EvidenceFailure,
    EvidenceImage, EvidenceOutcome, EvidenceWrite, HealthIssue, HealthObservation, OperatorCommand,
    Store, StoreConfig, StoreError,
    db::{DATABASE_FILE, Database},
    restore_backup, restore_backup_with_limits,
};

const BLACK: &[u8] = include_bytes!("../../../tests/fixtures/evidence/black.jpg");
const WHITE: &[u8] = include_bytes!("../../../tests/fixtures/evidence/white.jpg");

#[tokio::test]
async fn compute_admission_reserves_once_inside_camera_transaction() {
    use crate::{AnalysisAdmission, CameraAdmission, CameraSourceMeasurements};
    use std::sync::Arc;
    use sw_protocol::{
        AnalysisWorkload, CameraPatch, CapacityState, MediaWorkloadLimits, SourceKind,
        SourceMeasurement, SourceRate, SourceRateQuality, SourceWorkload,
    };

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let dimensions = Dimensions {
        width: 640,
        height: 360,
    };
    let config = CameraConfig {
        name: "Replay".into(),
        source: CameraSource::Replay {
            file: "reference.mp4".into(),
            pace: true,
            repeat: true,
        },
        enabled: true,
        sampling: Default::default(),
        priority: Default::default(),
        cloud_consent: false,
        privacy_masks: Vec::new(),
        controlled_fire_regions: Vec::new(),
    };
    let profile = Arc::new(AnalysisAdmission {
        id: "admission-test".into(),
        sha256: "a".repeat(64),
        workload: AnalysisWorkload {
            source: SourceWorkload {
                kind: SourceKind::PacedReplay,
                codec: "h264".into(),
                dimensions,
                min_millifps: 14_900,
                max_millifps: 15_100,
            },
            sampling: config.sampling,
            cameras: 1,
            min_pose_per_wall_minute: 285,
            min_fire_per_wall_minute: 114,
            detail_source: None,
            media: MediaWorkloadLimits::DEFAULT_LIMITS,
        },
    });
    let measured = CameraSourceMeasurements {
        analysis: SourceMeasurement {
            dimensions,
            codec: Some("h264".into()),
            rate: SourceRate {
                advertised_millifps: Some(15_000),
                observed_millifps: Some(15_000),
                quality: SourceRateQuality::Observed,
            },
        },
        detail: None,
    };
    let evaluated = |observed: bool| CameraAdmission {
        dimensions: Some(dimensions),
        analysis: Some(profile.clone()),
        source_measurements: observed.then(|| measured.clone()),
        reviewed_scene: None,
    };
    let actor = Uuid::new_v4();
    let create = |key: &str| CameraCommand {
        actor,
        key: key.parse().unwrap(),
        action: CameraAction::Create(config.clone()),
    };
    let limits = CameraLimits {
        active: 4,
        raw_bytes: 16_000_000,
    };
    let apply = |command, inspected| store.mutate_camera_admitted(command, inspected, limits, 900);
    assert_eq!(
        store
            .analysis_capacity(
                config.source.clone(),
                config.sampling,
                measured.clone(),
                None,
                profile.clone()
            )
            .await
            .unwrap()
            .state,
        CapacityState::Verified
    );
    assert!(matches!(
        apply(create("unknown"), evaluated(false)).await,
        Err(StoreError::AnalysisCapacity { .. })
    ));
    let (first, second) = tokio::join!(
        apply(create("first"), evaluated(true)),
        apply(create("second"), evaluated(true)),
    );
    let ((key, saved), rejected) = match first {
        Ok(saved) => (("first", saved), second),
        Err(error) => (("second", second.unwrap()), Err(error)),
    };
    assert!(matches!(rejected, Err(StoreError::AnalysisCapacity { .. })));
    let first = saved.camera;
    let retry = apply(create(key), evaluated(false)).await.unwrap();
    assert_eq!(retry.camera.id, first.id);
    let patch = |key: &str, value| CameraCommand {
        actor,
        key: key.parse().unwrap(),
        action: CameraAction::Patch {
            id: first.id,
            revision: first.revision,
            patch: value,
        },
    };
    let changed = sw_protocol::CameraSampling {
        fire_fps: 1,
        ..config.sampling
    };
    assert!(matches!(
        apply(
            patch(
                "sampling",
                CameraPatch {
                    sampling: Some(changed),
                    ..Default::default()
                }
            ),
            evaluated(false)
        )
        .await,
        Err(StoreError::AnalysisCapacity { .. })
    ));
    apply(
        patch(
            "disable",
            CameraPatch {
                enabled: Some(false),
                ..Default::default()
            },
        ),
        CameraAdmission {
            analysis: Some(profile.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let replacement = apply(create("replacement"), evaluated(true))
        .await
        .unwrap()
        .camera;
    drop(store);
    let restored = Store::open(root.path(), StoreConfig::default()).unwrap();
    let active = restored.enabled_cameras().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, replacement.id);
    assert_eq!(active[0].source_measurements, Some(measured));
}

#[tokio::test]
async fn a_cancelled_queued_command_does_not_mutate_and_retry_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(
        root.path(),
        StoreConfig {
            queue_capacity: 1,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    let held = store.clone();
    let (started, active) = tokio::sync::oneshot::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let running = tokio::spawn(async move {
        held.call(move |_| {
            let _ = started.send(());
            finish.recv().map_err(|_| StoreError::Unavailable)
        })
        .await
        .unwrap();
    });
    active.await.unwrap();
    let saved = incident();
    let id = saved.id;
    let queued = store.clone();
    let requested = saved.clone();
    let cancelled = tokio::spawn(async move { queued.create(requested, 1000).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store.writer_status().unwrap().queued == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        store.create(incident(), 1000).await,
        Err(StoreError::Busy)
    ));
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    running.await.unwrap();
    loop {
        match store.healthy().await {
            Ok(()) => break,
            Err(StoreError::Busy) => tokio::task::yield_now().await,
            result => panic!("unexpected health result: {result:?}"),
        }
    }
    let first = store.create(saved.clone(), 1001).await.unwrap();
    let second = store.create(saved, 1002).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(second.created_at, 1001);
    assert_eq!(store.writer_status().unwrap().cancelled_queued, Counter(1));
    let (stored, events, alerts) = store.call(|db| Ok(counts(&db.conn))).await.unwrap();
    assert_eq!((stored, events, alerts), (1, 1, 1));
    assert_eq!(store.get(id).await.unwrap(), second);
}

#[tokio::test]
async fn a_running_command_can_commit_after_a_completion_timeout() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let command = incident();
    let requested = command.clone();
    let (started, active) = tokio::sync::oneshot::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let writer = store.clone();
    let call = tokio::spawn(async move {
        writer
            .call_with_budget(std::time::Duration::from_millis(250), move |db| {
                let _ = started.send(());
                finish.recv().map_err(|_| StoreError::Unavailable)?;
                db.create(requested, 2_000)
            })
            .await
    });
    active.await.unwrap();
    let result = call.await.unwrap();
    assert!(matches!(result, Err(StoreError::OutcomeUnknown)));
    assert!(store.writer_status().unwrap().running_ms.unwrap().0 >= 200);
    release.send(()).unwrap();
    let retried = store.create(command, 3_000).await.unwrap();
    assert_eq!(retried.created_at, 2_000);
    assert_eq!(
        store.call(|db| Ok(counts(&db.conn))).await.unwrap(),
        (1, 1, 1)
    );
}

#[tokio::test]
async fn maintenance_drains_preaged_rows_in_bounded_batches() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let active = store.create(incident(), 10_000).await.unwrap();
    let active_id = active.id;
    let old = store.create(incident(), 1_000).await.unwrap();
    let old = store
        .apply(
            OperatorCommand {
                incident_id: old.id,
                actor_id: Uuid::new_v4(),
                idempotency_key: "preaged-history".into(),
                expected_revision: old.revision,
                action: OperatorAction::Resolve {
                    reason: "Reviewed".into(),
                },
            },
            1_100,
        )
        .await
        .unwrap()
        .incident;
    let old_id = old.id;
    let now = 7 * 86_400_000;
    let now = now + 2_000;
    store.call(move |db| {
        db.conn.execute_batch(
            "WITH RECURSIVE rows(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM rows WHERE i<1537)
             INSERT INTO idempotency_records
             SELECT zeroblob(16), 'test', CAST(i AS TEXT), zeroblob(32), '{}', 500 FROM rows",
        )?;
        db.conn.execute(
            "INSERT INTO idempotency_records VALUES(zeroblob(16),'test','active',zeroblob(32),'{}',?)",
            [now + 1],
        )?;
        db.conn.execute(
            "WITH RECURSIVE rows(i) AS (VALUES(2) UNION ALL SELECT i+1 FROM rows WHERE i<1026)
             INSERT INTO notification_outbox(incident_id, attention_cycle, attention, state, created_at, expires_at, next_attempt_at)
             SELECT ?1, i, 1, 'queued', 1, 500, 1 FROM rows",
             [active_id.as_bytes()],
        )?;
        db.conn.execute(
            "WITH RECURSIVE rows(i) AS (VALUES(3) UNION ALL SELECT i+1 FROM rows WHERE i<1025)
             INSERT INTO change_events(incident_id, revision, occurred_at, opened, incident)
             SELECT ?1, i, 1000, 0, ?2 FROM rows",
             rusqlite::params![old_id.as_bytes(), serde_json::to_string(&old)?],
        )?;
        Ok(())
    }).await.unwrap();
    let snapshot = store.snapshot().await.unwrap();
    for (index, expected) in [1026, 514, 2, 1].into_iter().enumerate() {
        assert_eq!(
            store.maintenance_step(2_000, 2_000, now).await.unwrap(),
            index < 3
        );
        let count = store
            .call(|db| {
                db.conn
                    .query_row(
                        "SELECT count(*) FROM idempotency_records WHERE operation = 'test'",
                        [],
                        |row| row.get::<_, u32>(0),
                    )
                    .map_err(Into::into)
            })
            .await
            .unwrap();
        assert_eq!(count, expected);
        let remaining = store
            .call(move |db| {
                db.conn
                    .query_row(
                        "SELECT count(*) FROM change_events WHERE incident_id = ?",
                        [old_id.as_bytes()],
                        |row| row.get::<_, u32>(0),
                    )
                    .map_err(Into::into)
            })
            .await
            .unwrap();
        assert_eq!(remaining, [513, 1, 0, 0][index]);
    }
    assert!(matches!(store.get(old_id).await, Err(StoreError::NotFound)));
    assert_eq!(store.snapshot().await.unwrap().cursor, snapshot.cursor);
    assert!(store.get(active_id).await.unwrap().resolution.is_none());
    let foreign_errors = store
        .call(|db| {
            db.conn
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get::<_, u32>(0)
                })
                .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(foreign_errors, 0);
}

#[tokio::test]
async fn material_observations_are_durable_and_bounded_without_image_packets() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let camera = store
        .mutate_camera(
            CameraCommand {
                actor: Uuid::new_v4(),
                key: "create-observation-camera".parse().unwrap(),
                action: CameraAction::Create(serde_json::from_value(serde_json::json!({
                    "name": "Camera", "enabled": true, "source": {"type": "usb", "device_id": "sample"}
                })).unwrap()),
            },
            Some(Dimensions { width: 160, height: 120 }),
            None,
            CameraLimits { active: 1, raw_bytes: 1_048_576 },
            1_000,
        ).await.unwrap().camera;
    let mut observed = incident();
    observed.category = Category::Fire;
    observed.scope.camera_id = camera.id;
    observed.scope.epoch = camera.epoch;
    observed.scope.scene_revision = camera.scene_revision;
    for index in 0..70 {
        let time = crate::ObservationTime::new(crate::utc_ms(), std::time::Instant::now());
        observed.last_source_us = index * 500_000;
        observed.observed_host_utc = Some(time.observed_host_utc);
        let facts = sw_protocol::LocalFacts::Fire {
            flame_visible: index >= 20,
            smoke_visible: true,
            apparent_growth: index >= 30,
            smoke_growth: index >= 10,
            escaped_controlled_region: index >= 40,
            controlled_region_overlap: None,
            continuity_samples: 3,
            detector_confidence: 0.65,
            visibility: sw_protocol::EvidenceVisibility::Good,
        };
        let region = sw_domain::Box2d {
            x1: 20.0,
            y1: 20.0,
            x2: 80.0,
            y2: 80.0,
        };
        let saved = store
            .record_hazard_observation(observed.clone(), time, facts, region, None, index == 0)
            .await
            .unwrap();
        assert_eq!(saved.last_source_us, observed.last_source_us);
        assert_eq!(saved.evidence_version(), 0);
    }
    let (count, oldest, latest) = store.call(move |db| {
        db.conn.query_row(
            "SELECT count(*), min(source_us), max(source_us) FROM hazard_observations WHERE incident_id = ?",
            [observed.id.as_bytes()],
            |r| Ok((r.get::<_, u32>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!((count, oldest, latest), (64, 3_000_000, 34_500_000));
}

#[tokio::test]
async fn new_source_epoch_can_link_but_never_inherits_prior_evidence_and_alerts() {
    use std::time::Instant;
    use sw_domain::SceneSignature;
    use sw_protocol::{EvidenceVisibility, LocalFacts};

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let camera = store.mutate_camera(
        CameraCommand {
            actor: Uuid::new_v4(),
            key: "scene-lineage-camera".parse().unwrap(),
            action: CameraAction::Create(serde_json::from_value(serde_json::json!({
                "name": "Camera", "enabled": true, "source": {"type": "usb", "device_id": "sample"}
            })).unwrap()),
        },
        Some(Dimensions { width: 160, height: 120 }),
        None,
        CameraLimits { active: 1, raw_bytes: 1_048_576 },
        crate::utc_ms(),
    ).await.unwrap().camera;
    let initial_scope = SourceScope {
        camera_id: camera.id,
        epoch: camera.epoch,
        scene_revision: camera.scene_revision,
    };
    let signature = SceneSignature([12; 48]);
    let mut different = signature;
    for (i, pixel) in different.0[8..].iter_mut().enumerate() {
        *pixel = if i % 2 == 0 { 100 } else { 0 };
    }
    let open = async |store: &Store, scope, signature| {
        let observed = crate::ObservationTime::new(crate::utc_ms(), Instant::now());
        store
            .record_hazard_observation(
                NewIncident {
                    scope,
                    category: Category::Fire,
                    observed_host_utc: Some(observed.observed_host_utc),
                    ..incident()
                },
                observed,
                LocalFacts::Fire {
                    flame_visible: true,
                    smoke_visible: false,
                    apparent_growth: false,
                    smoke_growth: false,
                    escaped_controlled_region: false,
                    controlled_region_overlap: None,
                    continuity_samples: 1,
                    detector_confidence: 0.9,
                    visibility: EvidenceVisibility::Good,
                },
                sw_domain::Box2d {
                    x1: 20.0,
                    y1: 20.0,
                    x2: 80.0,
                    y2: 80.0,
                },
                Some(signature),
                true,
            )
            .await
            .unwrap()
    };
    let first = open(&store, initial_scope, signature).await;
    let same_view = store
        .bump_camera_epoch(camera.id, camera.revision)
        .await
        .unwrap();
    let next = SourceScope {
        epoch: same_view.epoch,
        ..initial_scope
    };
    drop(store);
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let reobserved = open(&store, next, signature).await;
    assert_eq!(
        reobserved.possible_continuation_of,
        Some(sw_domain::UncertainIncidentLink {
            incident_id: first.id,
            source_epoch: first.scope.epoch,
        })
    );
    assert_ne!(reobserved.id, first.id);
    assert_eq!(reobserved.evidence_version(), 0);
    assert!(store.get(first.id).await.unwrap().resolution.is_none());
    let different_view = store
        .bump_camera_epoch(camera.id, camera.revision)
        .await
        .unwrap();
    let other = open(
        &store,
        SourceScope {
            epoch: different_view.epoch,
            ..initial_scope
        },
        different,
    )
    .await;
    assert!(other.possible_continuation_of.is_none());
    let last = store
        .bump_camera_epoch(camera.id, camera.revision)
        .await
        .unwrap();
    let ambiguous = open(
        &store,
        SourceScope {
            epoch: last.epoch,
            ..initial_scope
        },
        signature,
    )
    .await;
    assert!(ambiguous.possible_continuation_of.is_none());
    assert_eq!(
        store.call(|db| Ok(counts(&db.conn))).await.unwrap(),
        (4, 5, 4)
    );
}

#[tokio::test]
async fn medical_recovery_retries_after_partial_evidence_and_checks_the_current_course() {
    use std::time::{Duration, Instant};

    use sw_domain::{Attention, RecoveryCourse};
    use sw_protocol::{EvidenceContext, EvidenceCoverage, EvidenceInterval, LocalFacts};

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(
        root.path(),
        StoreConfig {
            evidence_quota_bytes: BLACK.len() as u64,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    let camera = store.mutate_camera(
        CameraCommand {
            actor: Uuid::new_v4(),
            key: "recovery-camera".parse().unwrap(),
            action: CameraAction::Create(serde_json::from_value(serde_json::json!({
                "name": "Camera", "enabled": true, "source": {"type": "usb", "device_id": "sample"}
            })).unwrap()),
        },
        Some(Dimensions { width: 160, height: 120 }),
        None,
        CameraLimits { active: 1, raw_bytes: 1_048_576 },
        crate::utc_ms(),
    ).await.unwrap().camera;
    let scope = SourceScope {
        camera_id: camera.id,
        epoch: camera.epoch,
        scene_revision: camera.scene_revision,
    };
    let course = RecoveryCourse::ControlledActivity;
    let open = |assessment| NewIncident {
        scope,
        category: Category::Medical,
        assessment,
        ..incident()
    };
    let save = async |new: &NewIncident,
                      saved: &sw_domain::Incident,
                      source_us,
                      images: &[&[u8]],
                      onset_observed,
                      identity_uncertain| {
        let time = crate::ObservationTime::new(crate::utc_ms(), Instant::now());
        let intent = crate::RecoveryIntent {
            incident_id: saved.id,
            scope,
            episode_id: Uuid::new_v4(),
            course,
            source_us,
        };
        let facts = LocalFacts::Medical {
            track_id: Counter(1),
            episode_id: intent.episode_id,
            previous_episode_id: None,
            onset_observed,
            identity_uncertain,
            recovery_seconds: 3.0,
            clear_eligible: true,
        };
        let saved = store
            .record_hazard_observation(
                NewIncident {
                    last_source_us: source_us,
                    observed_host_utc: Some(time.observed_host_utc),
                    ..new.clone()
                },
                time,
                facts,
                sw_domain::Box2d {
                    x1: 10.0,
                    y1: 10.0,
                    x2: 60.0,
                    y2: 40.0,
                },
                None,
                false,
            )
            .await
            .unwrap();
        let mut write = evidence_write(
            &saved,
            EvidenceStamp {
                packet_id: Uuid::new_v4(),
                version: saved.evidence_version() + 1,
                scope,
                expires_at: time.expires_at(),
            },
            images,
        );
        write.observation_time = Some(time);
        write.context = Some(EvidenceContext {
            observed_interval: EvidenceInterval {
                start_source_s: 0.0,
                end_source_s: source_us as f64 / 1_000_000.0,
            },
            coverage: EvidenceCoverage {
                expected_samples: 6,
                received_samples: 6,
                max_gap_ms: 500,
            },
            sampling: None,
            local_facts: facts,
            lineage: None,
            recovery: Some(intent.observation()),
            limitations: vec![],
            policy_version: "qa".into(),
            prompt_version: "qa".into(),
        });
        for image in &mut write.images {
            image.source_us = source_us;
        }
        let outcome = store
            .persist_evidence(write, crate::utc_ms())
            .await
            .unwrap();
        (intent, time, outcome)
    };
    let new = open(Assessment::Concern);
    let saved = store
        .create_hazard(new.clone(), crate::utc_ms())
        .await
        .unwrap();
    let (intent, time, evidence) =
        save(&new, &saved, 1_000_000, &[BLACK, WHITE], true, false).await;
    assert!(matches!(evidence, EvidenceOutcome::Partial(_)));
    assert_eq!(
        store.apply_recovery(intent, time).await.unwrap(),
        crate::RecoveryOutcome::NeedsEvidence
    );
    assert!(store.get(saved.id).await.unwrap().resolution.is_none());

    let (intent, time, evidence) = save(&new, &saved, 2_000_000, &[BLACK], true, false).await;
    assert!(matches!(evidence, EvidenceOutcome::Persisted(_)));
    assert_eq!(
        store
            .apply_recovery(
                crate::RecoveryIntent {
                    scope: SourceScope {
                        epoch: scope.epoch + 1,
                        ..scope
                    },
                    ..intent
                },
                time
            )
            .await
            .unwrap(),
        crate::RecoveryOutcome::Superseded
    );
    let stale = crate::ObservationTime::new(
        time.observed_host_utc,
        Instant::now() - Duration::from_secs(6),
    );
    assert_eq!(
        store.apply_recovery(intent, stale).await.unwrap(),
        crate::RecoveryOutcome::Superseded
    );
    assert_eq!(
        store.apply_recovery(intent, time).await.unwrap(),
        crate::RecoveryOutcome::Applied
    );
    let resolved = store.get(saved.id).await.unwrap();
    assert_eq!(
        resolved.resolution.as_ref().unwrap().reason,
        course.resolution_reason()
    );
    assert_eq!(
        store.apply_recovery(intent, time).await.unwrap(),
        crate::RecoveryOutcome::Applied
    );
    assert_eq!(store.get(saved.id).await.unwrap(), resolved);

    for (assessment, acknowledge, onset, uncertain, expected) in [
        (
            Assessment::Concern,
            true,
            true,
            false,
            crate::RecoveryOutcome::Inapplicable,
        ),
        (
            Assessment::ImmediateVisibleConcern,
            false,
            true,
            false,
            crate::RecoveryOutcome::Inapplicable,
        ),
        (
            Assessment::Concern,
            false,
            false,
            false,
            crate::RecoveryOutcome::Superseded,
        ),
        (
            Assessment::Concern,
            false,
            true,
            true,
            crate::RecoveryOutcome::Superseded,
        ),
    ] {
        let new = open(assessment);
        let saved = store
            .create_hazard(new.clone(), crate::utc_ms())
            .await
            .unwrap();
        let (intent, time, _) = save(&new, &saved, 2_500_000, &[BLACK], onset, uncertain).await;
        let before = if acknowledge {
            let current = store.get(saved.id).await.unwrap();
            store
                .apply(
                    OperatorCommand {
                        incident_id: saved.id,
                        actor_id: Uuid::new_v4(),
                        idempotency_key: "ack-before-recovery".into(),
                        expected_revision: current.revision,
                        action: OperatorAction::Acknowledge,
                    },
                    crate::utc_ms(),
                )
                .await
                .unwrap()
                .incident
        } else {
            store.get(saved.id).await.unwrap()
        };
        assert_eq!(store.apply_recovery(intent, time).await.unwrap(), expected);
        let after = store.get(saved.id).await.unwrap();
        assert_eq!(after, before);
        assert_eq!(
            after.attention,
            if assessment == Assessment::Concern {
                Attention::Review
            } else {
                Attention::Urgent
            }
        );
        assert!(after.resolution.is_none());
    }
}

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
                    sampling: Default::default(),
                    priority: Default::default(),
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
        observed_host_utc: None,
        live: true,
        assessment: Assessment::Concern,
    }
}

fn database(root: &Path, quota: u64) -> Database {
    Database::open(
        root,
        &StoreConfig {
            evidence_quota_bytes: quota,
            ..StoreConfig::default()
        },
        watch::channel(()).0,
    )
    .unwrap()
}

fn counts(conn: &Connection) -> (u64, u64, u64) {
    conn.query_row("SELECT (SELECT count(*) FROM incidents), (SELECT count(*) FROM change_events), (SELECT count(*) FROM notification_outbox)", [], |row| {
        Ok((super::db::row_counter(row, 0)?, super::db::row_counter(row, 1)?, super::db::row_counter(row, 2)?))
    }).unwrap()
}

#[test]
fn snapshot_remains_available_when_open_incidents_exceed_its_page() {
    let root = tempfile::tempdir().unwrap();
    let mut db = database(root.path(), 0);
    let over_limit_camera = Uuid::new_v4();
    let other_camera = Uuid::new_v4();
    let mut first = Uuid::nil();
    for index in 0..=super::MAX_SNAPSHOT {
        let mut incident = incident();
        incident.scope.camera_id = over_limit_camera;
        if index == 0 {
            first = incident.id;
        }
        db.create(incident, 1_000 + index as i64).unwrap();
    }
    let mut other = incident();
    other.scope.camera_id = other_camera;
    db.create(other, 5_000).unwrap();
    let page = db.snapshot(Some(vec![over_limit_camera])).unwrap();
    assert!(page.has_older_open_incidents);
    assert_eq!(page.incidents.len(), super::MAX_SNAPSHOT);
    assert!(page.incidents.iter().all(|incident| incident.id != first));
    assert_eq!(db.get(first).unwrap().scope.camera_id, over_limit_camera);
    assert_eq!(db.change_head().unwrap(), page.cursor);
    let other_page = db.snapshot(Some(vec![other_camera])).unwrap();
    assert!(!other_page.has_older_open_incidents);
    assert_eq!(other_page.incidents.len(), 1);
    assert_eq!(other_page.cursor, page.cursor);
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
    use std::time::{Duration, Instant};

    use sw_protocol::{
        CameraConfig, CameraPatch, CameraSource, Classification, Dimensions, EvidenceContext,
        EvidenceCoverage, EvidenceInterval, EvidenceVisibility, InferenceSource, LocalFacts,
        MedicalObservation, Observable, ObservationReport, SemanticObservation, SemanticReport,
        SemanticRoute,
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

    let evidence = async |epoch, at, stale: bool| {
        let mut new = incident();
        new.scope.camera_id = camera.id;
        new.scope.epoch = epoch;
        let monotonic = Instant::now();
        let utc = crate::utc_ms();
        // Keep UTC within the apparent TTL while the real frame is a minute old.
        let captured_at = if stale {
            monotonic - Duration::from_secs(60)
        } else {
            monotonic
        };
        let saved = if stale {
            let time = crate::ObservationTime::new(utc, captured_at);
            new.observed_host_utc = Some(utc);
            new.assessment = Assessment::ImmediateVisibleConcern;
            store.create_observed_hazard(new, time).await.unwrap()
        } else {
            store.create_hazard(new, at).await.unwrap()
        };
        let observation_time = crate::ObservationTime::new(utc, captured_at);
        let stamp = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: 1,
            scope: saved.scope,
            expires_at: observation_time.expires_at(),
        };
        store
            .persist_evidence(
                EvidenceWrite {
                    incident_id: saved.id,
                    expected_revision: saved.revision,
                    stamp,
                    observation_time: Some(observation_time),
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
                        sampling: None,
                        local_facts: LocalFacts::Medical {
                            track_id: sw_protocol::Counter(1),
                            episode_id: saved.id,
                            previous_episode_id: None,
                            onset_observed: true,
                            identity_uncertain: false,
                            recovery_seconds: 0.0,
                            clear_eligible: false,
                        },
                        lineage: None,
                        recovery: None,
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
                crate::utc_ms(),
            )
            .await
            .unwrap();
        let packet = store
            .evidence_packet(saved.id, stamp.packet_id)
            .await
            .unwrap();
        assert_eq!(packet.fresh_when_persisted, Some(!stale));
        assert_eq!(packet.observed_host_utc.unwrap().unix_millis(), utc);
        assert!(packet.persisted_at.is_some());
        assert_eq!(
            packet.expires_at.unix_millis(),
            observation_time.expires_at()
        );
        if stale {
            assert!(saved.opening_delayed);
            assert_eq!(saved.attention, sw_domain::Attention::Review);
            assert!(saved.live);
            assert_eq!(store.get(saved.id).await.unwrap().evidence, Some(stamp));
            assert!(matches!(
                store
                    .record_fresh_recovery(
                        saved.id,
                        stamp.scope,
                        saved.last_source_us + 1,
                        observation_time
                    )
                    .await,
                Err(StoreError::Domain(sw_domain::DomainError::Stale))
            ));
        }
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
    let owner = Uuid::new_v4();
    let schedule = |route, ready| crate::SemanticSchedule {
        owner,
        route,
        local_ready: ready,
        daily_limit: 5,
        minimum_interval_ms: 0,
        include_replay: false,
    };
    let claim = async |route, ready| store.claim_semantic(schedule(route, ready)).await.unwrap();

    let default_epoch = store.bump_camera_epoch(camera.id, 1).await.unwrap().epoch;
    evidence(default_epoch, 1_500, true).await;
    assert!(claim(SemanticRoute::LocalOnly, true).await.is_none());
    let observed = Instant::now();
    let before_step = crate::ObservationTime::new(10_000, observed);
    assert!(before_step.evidence_fresh(10_500, observed + Duration::from_millis(500)));
    assert!(!before_step.evidence_fresh(10_100, observed + Duration::from_secs(3)));
    assert!(!before_step.evidence_fresh(15_000, observed + Duration::from_millis(500)));
    let local = evidence(default_epoch, 2_000, false).await;
    assert!(claim(SemanticRoute::CloudOnly, true).await.is_none());
    assert!(claim(SemanticRoute::PreferLocal, false).await.is_none());
    assert!(claim(SemanticRoute::LocalOnly, false).await.is_none());
    let local_claim = claim(SemanticRoute::PreferLocal, true).await.unwrap();
    let initial_work = store.load_semantic(&local_claim).await.unwrap().unwrap();
    assert_eq!(store.semantic_usage(0).await.unwrap(), 0);
    store
        .call(move |db| {
            db.conn.execute(
                "UPDATE semantic_jobs SET lease_until = 0 WHERE packet_id = ?",
                [local.packet_id.as_bytes()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let successor = crate::SemanticSchedule {
        owner: Uuid::new_v4(),
        ..schedule(SemanticRoute::PreferLocal, true)
    };
    let local_claim = store.claim_semantic(successor).await.unwrap().unwrap();
    let local_work = store.load_semantic(&local_claim).await.unwrap().unwrap();
    assert_eq!(local_work.request_id, initial_work.request_id);
    assert_eq!(
        store
            .dispatch_semantic(&initial_work, schedule(SemanticRoute::PreferLocal, true))
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.semantic_usage(0).await.unwrap(), 0);
    assert_eq!(local_work.route, SemanticRoute::LocalOnly);
    assert_eq!(local_work.stamp, local);

    let opted_in = patch(1, true, "allow-cloud", 4_000).await;
    assert!(store.load_semantic(&local_claim).await.unwrap().is_none());
    assert_eq!(
        store
            .dispatch_semantic(&local_work, schedule(SemanticRoute::PreferLocal, true))
            .await
            .unwrap(),
        None
    );
    store.release_semantic(&local_claim, true).await.unwrap();
    assert!(claim(SemanticRoute::CloudOnly, false).await.is_none());
    let cloud = evidence(opted_in.epoch, 5_000, false).await;
    let cloud_claim = claim(SemanticRoute::CloudOnly, false).await.unwrap();
    let cloud_work = store.load_semantic(&cloud_claim).await.unwrap().unwrap();
    assert_eq!(cloud_work.stamp, cloud);
    assert_eq!(cloud_work.route, SemanticRoute::CloudOnly);
    assert_eq!(
        store
            .dispatch_semantic(&cloud_work, schedule(SemanticRoute::CloudOnly, false))
            .await
            .unwrap(),
        Some(1)
    );
    assert!(store.semantic_current(&cloud_work).await.unwrap());

    let opted_out = patch(2, false, "revoke-cloud", 7_000).await;
    assert!(opted_out.epoch > opted_in.epoch);
    assert!(!store.semantic_current(&cloud_work).await.unwrap());
    patch(3, true, "allow-cloud-again", 8_000).await;
    assert!(!store.semantic_current(&cloud_work).await.unwrap());

    let current = store.camera(camera.id).await.unwrap();
    let fresh_before_restart = evidence(current.epoch, 9_000, false).await;
    assert_ne!(fresh_before_restart.packet_id, cloud_work.stamp.packet_id);
    let cloud_schedule = schedule(SemanticRoute::CloudOnly, false);
    let report = SemanticReport {
        sdk_version: "13.3.0".into(),
        inference_source: InferenceSource::InCloud,
        model: "gemini-3.5-flash-lite".into(),
        usage_total_tokens: Some(10),
        observation: ObservationReport {
            classification: Classification::PossibleVisibleMedicalConcern,
            observations: SemanticObservation::Medical(MedicalObservation {
                person_visible: Observable::Yes,
                transition_visible: Observable::Unknown,
                persistent_low_posture: Observable::Yes,
                same_subject_recovery_visible: Observable::Unknown,
                visibility: EvidenceVisibility::Good,
            }),
            supporting_image_ids: vec!["f0".into()],
            contradictions: vec![],
            limitations: vec![],
            brief_description: "A person is low in the frame.".into(),
        },
    };
    let fresh_claim = store.claim_semantic(cloud_schedule).await.unwrap().unwrap();
    let fresh_work = store.load_semantic(&fresh_claim).await.unwrap().unwrap();
    assert_eq!(
        store
            .dispatch_semantic(&fresh_work, cloud_schedule)
            .await
            .unwrap(),
        Some(2)
    );
    assert!(
        store
            .claim_semantic(crate::SemanticSchedule {
                daily_limit: 2,
                ..cloud_schedule
            })
            .await
            .unwrap()
            .is_none()
    );
    store
        .call(|db| {
            db.conn.execute_batch(
                "CREATE TEMP TRIGGER reject_semantic BEFORE UPDATE ON semantic_attempts
            WHEN NEW.report IS NOT NULL BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        store
            .save_semantic_response(&fresh_work, &report, Assessment::Concern)
            .await
            .is_err()
    );
    assert!(store.semantic_current(&fresh_work).await.unwrap());
    store
        .call(|db| {
            db.conn.execute_batch("DROP TRIGGER reject_semantic;")?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .save_semantic_response(&fresh_work, &report, Assessment::Concern)
        .await
        .unwrap();
    store
        .save_semantic_response(&fresh_work, &report, Assessment::Concern)
        .await
        .unwrap();
    assert_eq!(
        store.finish_semantic_response().await.unwrap(),
        Some(crate::SemanticCompleted::Applied {
            source: InferenceSource::InCloud
        })
    );
    assert_eq!(store.finish_semantic_response().await.unwrap(), None);
    assert_eq!(store.semantic_usage(0).await.unwrap(), 2);

    // An accepted provider response survives a Core restart as evidence history.
    // Its previous monotonic clock cannot authorize escalation in this process.
    let after_restart = evidence(current.epoch, 10_000, false).await;
    let claim_before_restart = store.claim_semantic(cloud_schedule).await.unwrap().unwrap();
    let work_before_restart = store
        .load_semantic(&claim_before_restart)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(work_before_restart.stamp, after_restart);
    assert_eq!(
        store
            .dispatch_semantic(&work_before_restart, cloud_schedule)
            .await
            .unwrap(),
        Some(3)
    );
    store
        .save_semantic_response(
            &work_before_restart,
            &report,
            Assessment::ImmediateVisibleConcern,
        )
        .await
        .unwrap();
    let undispatched = evidence(current.epoch, 11_000, false).await;
    drop(store);
    let restarted = Store::open(root.path(), StoreConfig::default()).unwrap();
    assert!(
        restarted
            .claim_semantic(schedule(SemanticRoute::CloudOnly, false))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        restarted.finish_semantic_response().await.unwrap(),
        Some(crate::SemanticCompleted::Stale)
    );
    let states = restarted
        .call(move |db| {
            [
                cloud.packet_id,
                work_before_restart.stamp.packet_id,
                undispatched.packet_id,
            ]
            .map(|packet| {
                db.conn.query_row(
                    "SELECT state FROM semantic_jobs WHERE packet_id = ?",
                    [packet.as_bytes()],
                    |r| r.get::<_, String>(0),
                )
            })
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(states, ["unknown_outcome", "completed", "expired"]);
    assert_eq!(restarted.semantic_usage(0).await.unwrap(), 3);
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
                observation_time: None,
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

#[tokio::test]
async fn alert_scan_keeps_old_cycles_reachable_while_receipts_and_new_intents_arrive() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let actor = Uuid::new_v4();
    let camera = Uuid::new_v4();
    let scope = Some(vec![camera]);
    let sample = |assessment| NewIncident {
        scope: SourceScope {
            camera_id: camera,
            epoch: 1,
            scene_revision: 1,
        },
        assessment,
        ..incident()
    };
    let oldest = store
        .create(sample(Assessment::Concern), 1_000)
        .await
        .unwrap();
    let mut urgent = Vec::new();
    for _ in 0..130 {
        urgent.push(
            store
                .create(sample(Assessment::ImmediateVisibleConcern), 1_200)
                .await
                .unwrap(),
        );
    }
    let page = store
        .active_alert_page(actor, scope.clone(), 2_000, None, 128)
        .await
        .unwrap();
    assert_eq!(page.alerts.len(), 128);
    assert_eq!(page.alerts[0].incident_id, urgent[0].id);
    let receipt = &page.alerts[0];
    for now in [2_001, 2_002] {
        store
            .record_alert_seen(
                actor,
                receipt.id,
                receipt.attention_cycle,
                scope.clone(),
                now,
            )
            .await
            .unwrap();
    }
    let new = store
        .create(sample(Assessment::ImmediateVisibleConcern), 2_500)
        .await
        .unwrap();
    let deadline = 2_501;
    store
        .apply(
            OperatorCommand {
                incident_id: urgent[128].id,
                actor_id: actor,
                idempotency_key: "test-mute-one".into(),
                expected_revision: urgent[128].revision,
                action: OperatorAction::Silence {
                    duration_seconds: 4,
                },
            },
            deadline,
        )
        .await
        .unwrap();
    let next = store
        .active_alert_page(actor, scope.clone(), deadline, page.next, 128)
        .await
        .unwrap();
    let ids: Vec<_> = next.alerts.iter().map(|alert| alert.incident_id).collect();
    assert_eq!(ids, [urgent[129].id, oldest.id]);
    assert!(next.next.is_none());
    let renewed = store
        .active_alert_page(actor, scope.clone(), 7_000, None, 128)
        .await
        .unwrap();
    let tail = store
        .active_alert_page(actor, scope.clone(), 7_000, renewed.next, 128)
        .await
        .unwrap();
    assert!(tail.alerts.iter().any(|alert| alert.incident_id == new.id));
    assert!(
        tail.alerts
            .iter()
            .any(|alert| alert.incident_id == urgent[128].id)
    );
    assert_eq!(
        renewed.alerts[0].client_seen_at.unwrap().unix_millis(),
        2_001
    );
    assert!(
        store
            .active_alert_page(actor, Some(vec![]), 7_000, page.next, 128)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
    assert!(
        store
            .active_alert_page(actor, scope, 3_603_000, page.next, 128)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
}

#[tokio::test]
async fn refresh_replays_one_signed_rotation_across_reopen_and_revokes_new_proof_reuse() {
    use ed25519_dalek::{Signer, SigningKey};
    use sw_protocol::{
        DeviceRole, PairingFinish, PairingRequest, RefreshRequest, pairing_message, refresh_message,
    };

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let instance = store.identity().instance_id;
    let signer = SigningKey::from_bytes(&[43; 32]);
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let grant = store
        .issue_pairing(
            instance,
            PairingRequest {
                role: DeviceRole::Owner,
                camera_ids: vec![],
                scopes: vec![],
            },
            100,
        )
        .await
        .unwrap();
    let label = "Refresh receipt regression";
    let message = pairing_message(instance, grant.challenge, &grant.secret, label);
    let initial = store
        .finish_pairing(
            PairingFinish {
                challenge: grant.challenge,
                secret: grant.secret,
                label: label.into(),
                public_key: hex(&signer.verifying_key().to_bytes()),
                signature: hex(&signer.sign(&message).to_bytes()),
            },
            200,
        )
        .await
        .unwrap();
    let device = initial.device.id;
    let request = |token: &str, nonce| RefreshRequest {
        device_id: device,
        nonce,
        refresh_token: token.into(),
        signature: hex(&signer
            .sign(&refresh_message(instance, device, nonce, token))
            .to_bytes()),
    };
    let first = request(&initial.refresh_token, Uuid::new_v4());
    let (committed, duplicate) = tokio::join!(
        store.refresh_device(first.clone(), 1_000),
        store.refresh_device(first.clone(), 1_000)
    );
    let (committed, duplicate) = (committed.unwrap(), duplicate.unwrap());
    assert_eq!(committed.access_token, duplicate.access_token);
    assert_eq!(committed.refresh_token, duplicate.refresh_token);
    drop(store);
    let db = Connection::open(root.path().join(DATABASE_FILE)).unwrap();
    let encrypted: Vec<u8> = db
        .query_row("SELECT envelope FROM refresh_receipts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        !encrypted
            .windows(64)
            .any(|part| part == committed.refresh_token.as_bytes()
                || part == committed.access_token.as_bytes())
    );
    drop(db);
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let replay = store.refresh_device(first.clone(), 120_999).await.unwrap();
    assert_eq!(replay.access_token, committed.access_token);
    assert_eq!(replay.refresh_token, committed.refresh_token);
    assert_eq!(replay.access_expires_at, committed.access_expires_at);
    assert!(matches!(
        store.refresh_device(first.clone(), 121_000).await,
        Err(StoreError::Credential)
    ));
    assert!(
        store
            .device_access(&committed.access_token, 121_000)
            .await
            .is_ok()
    );

    let current = request(&committed.refresh_token, Uuid::new_v4());
    let replacement = store
        .refresh_device(current.clone(), 122_000)
        .await
        .unwrap();
    assert!(matches!(
        store.refresh_device(first, 122_001).await,
        Err(StoreError::Credential)
    ));
    assert!(
        store
            .device_access(&replacement.access_token, 122_001)
            .await
            .is_ok()
    );
    let different = request(&committed.refresh_token, Uuid::new_v4());
    assert!(matches!(
        store.refresh_device(different, 122_002).await,
        Err(StoreError::Credential)
    ));
    assert!(matches!(
        store.refresh_device(current, 122_003).await,
        Err(StoreError::Credential)
    ));
    assert!(matches!(
        store
            .device_access(&replacement.access_token, 122_003)
            .await,
        Err(StoreError::Credential)
    ));
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
async fn metadata_quota_reports_pressure_and_reuses_pages_after_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let quota = 4 * 1024 * 1024;
    let store = Store::open(
        root.path(),
        StoreConfig {
            database_quota_bytes: quota,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.ready().await.unwrap();
    let saved = store.create(incident(), 1_000).await.unwrap();
    let inserted = store
        .call(|db| {
            db.conn
                .execute("CREATE TABLE quota_fixture(payload BLOB)", [])?;
            for i in 0..128 {
                match db
                    .conn
                    .execute("INSERT INTO quota_fixture VALUES(zeroblob(65536))", [])
                {
                    Ok(_) => {}
                    Err(error) if error.sqlite_error_code() == Some(ErrorCode::DiskFull) => {
                        let count: i64 =
                            db.conn
                                .query_row("SELECT count(*) FROM quota_fixture", [], |row| {
                                    row.get(0)
                                })?;
                        assert_eq!(count, i);
                        return Ok(i);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            panic!("fixture did not exhaust the configured page quota");
        })
        .await
        .unwrap();
    assert!(inserted > 0);
    assert!(matches!(
        store.ready().await,
        Err(StoreError::DatabaseCapacity)
    ));
    assert!(store.database_budget_low().await.unwrap());
    assert!(matches!(
        store
            .call(|db| {
                db.conn
                    .execute("INSERT INTO quota_fixture VALUES(zeroblob(65536))", [])?;
                Ok(())
            })
            .await,
        Err(StoreError::DatabaseCapacity)
    ));
    assert!(fs::metadata(root.path().join(DATABASE_FILE)).unwrap().len() <= quota);
    assert_eq!(store.get(saved.id).await.unwrap(), saved);
    store
        .call(|db| {
            db.conn.execute("DROP TABLE quota_fixture", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!store.database_budget_low().await.unwrap());
    store.ready().await.unwrap();
    store.create(incident(), 2_000).await.unwrap();
}

#[tokio::test]
async fn startup_recovers_pending_evidence_when_metadata_space_returns() {
    let root = tempfile::tempdir().unwrap();
    let config = StoreConfig {
        database_quota_bytes: 4 * 1024 * 1024,
        ..StoreConfig::default()
    };
    let mut db = Database::open(root.path(), &config, watch::channel(()).0).unwrap();
    let mut saved = Vec::new();
    for _ in 0..64 {
        let hazard = db.create(incident(), 1_000).unwrap();
        saved.push(hazard.id);
    }
    db.conn.execute_batch(
        "UPDATE incidents SET camera_hazard = 1, state = json_set(state, '$.evidence_state', 'pending');
         CREATE TABLE quota_fixture(payload BLOB);",
    ).unwrap();
    loop {
        match db
            .conn
            .execute("INSERT INTO quota_fixture VALUES(zeroblob(4096))", [])
        {
            Ok(_) => {}
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::DiskFull) => break,
            Err(error) => panic!("unexpected fixture result: {error:?}"),
        }
    }
    drop(db);
    let store = Store::open(root.path(), config).unwrap();
    assert!(matches!(
        store.ready().await,
        Err(StoreError::DatabaseCapacity)
    ));
    assert_eq!(
        store.get(saved[0]).await.unwrap().evidence_state,
        EvidenceState::Pending
    );
    store
        .call(|db| {
            db.conn.execute("DROP TABLE quota_fixture", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!store.database_budget_low().await.unwrap());
    store.ready().await.unwrap();
    for id in saved {
        assert_eq!(
            store.get(id).await.unwrap().evidence_state,
            EvidenceState::Unavailable
        );
    }
}

#[tokio::test]
async fn a_pinned_wal_reader_cannot_grow_the_log_indefinitely() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let saved = store.create(incident(), 1_000).await.unwrap();
    store
        .call(|db| {
            db.conn.execute_batch(
                "CREATE TABLE wal_fixture(payload BLOB);
                 INSERT INTO wal_fixture VALUES(zeroblob(1048576));",
            )?;
            let busy: i64 = db
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
            assert_eq!(busy, 0);
            Ok(())
        })
        .await
        .unwrap();
    let reader = Connection::open(root.path().join(DATABASE_FILE)).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let count: i64 = reader
        .query_row("SELECT count(*) FROM wal_fixture", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let mut suspended = false;
    for iteration in 0..32 {
        match store
            .call(move |db| {
                db.conn.execute(
                    "UPDATE wal_fixture
                     SET payload = substr(hex(zeroblob(524288)), 1, 1048576 - ?)",
                    [iteration],
                )?;
                Ok(())
            })
            .await
        {
            Ok(()) => {}
            Err(StoreError::DatabaseCapacity) => {
                suspended = true;
                break;
            }
            Err(error) => panic!("unexpected checkpoint result {error:?}"),
        }
    }
    assert!(suspended);
    let wal = root.path().join(format!("{DATABASE_FILE}-wal"));
    let held_bytes = fs::metadata(&wal).unwrap().len();
    assert!((16 * 1024 * 1024..18 * 1024 * 1024).contains(&held_bytes));
    assert!(matches!(
        store.ready().await,
        Err(StoreError::DatabaseCapacity)
    ));
    store.healthy().await.unwrap();
    assert_eq!(store.get(saved.id).await.unwrap(), saved);
    assert!(matches!(
        store.create(incident(), 2_000).await,
        Err(StoreError::DatabaseCapacity)
    ));
    assert_eq!(fs::metadata(&wal).unwrap().len(), held_bytes);
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    store.ready().await.unwrap();
    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    store.create(incident(), 3_000).await.unwrap();
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
        observation_time: None,
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
    let quota = BLACK.len().max(WHITE.len()) as u64;
    let mut db = database(root.path(), quota);
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
    db.conn.execute_batch("DROP TRIGGER fail_incident").unwrap();
    let blocked = db
        .persist_evidence(evidence_write(&incident, stamp, &[WHITE]), 2_001)
        .unwrap();
    assert!(matches!(blocked, EvidenceOutcome::Failed(result)
        if result.failures == vec![(0, EvidenceFailure::Quota)]));
    drop(db);
    let mut reopened = database(root.path(), quota);
    assert_eq!(fs::read_dir(&reopened.blobs).unwrap().count(), 0);
    let incident = reopened.get(incident.id).unwrap();
    let stamp = EvidenceStamp {
        packet_id: Uuid::new_v4(),
        version: 2,
        ..stamp
    };
    assert!(matches!(
        reopened
            .persist_evidence(evidence_write(&incident, stamp, &[BLACK]), 2_002)
            .unwrap(),
        EvidenceOutcome::Persisted(_)
    ));
}

#[test]
fn uncertain_blob_bytes_recount_in_maintenance_slices() {
    let root = tempfile::tempdir().unwrap();
    let byte_count = 513;
    let mut db = database(root.path(), byte_count + BLACK.len() as u64);
    for i in 0..byte_count {
        fs::write(db.blobs.join(format!("quota-{i}")), [0_u8]).unwrap();
    }
    let mut saved = db.create(incident(), 1_000).unwrap();
    db.blob_bytes = None;
    for i in 0..3 {
        let stamp = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            scope: saved.scope,
            version: i + 1,
            expires_at: 9_000,
        };
        let outcome = db
            .persist_evidence(evidence_write(&saved, stamp, &[BLACK]), 2_000 + i as i64)
            .unwrap();
        saved = db.get(saved.id).unwrap();
        if i < 2 {
            assert!(matches!(outcome, EvidenceOutcome::Failed(result)
                if result.failures == vec![(0, EvidenceFailure::Io)]));
            assert!(db.retire_evidence(500, 3_000).unwrap());
            assert_eq!(db.blob_bytes, None);
            continue;
        }
        assert!(matches!(outcome, EvidenceOutcome::Failed(_)));
        assert!(!db.retire_evidence(500, 3_000).unwrap());
        assert_eq!(db.blob_bytes, Some(byte_count));
    }
    let stamp = EvidenceStamp {
        packet_id: Uuid::new_v4(),
        scope: saved.scope,
        version: 4,
        expires_at: 9_000,
    };
    assert!(matches!(
        db.persist_evidence(evidence_write(&saved, stamp, &[BLACK]), 4_000)
            .unwrap(),
        EvidenceOutcome::Persisted(_)
    ));
    assert_eq!(db.blob_bytes, Some(byte_count + BLACK.len() as u64));
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
    let rejected_budget = BackupLimits { max_bytes: 1 };
    assert!(matches!(
        store
            .backup_with_limits(backup.clone(), rejected_budget)
            .await,
        Err(StoreError::BackupCapacity)
    ));
    assert!(!backup.exists());
    let budget = BackupLimits {
        max_bytes: 4 * 1024 * 1024,
    };
    store
        .backup_with_limits(backup.clone(), budget)
        .await
        .unwrap();
    let target = root.path().join("restored");
    assert!(matches!(
        restore_backup_with_limits(&backup, &target, rejected_budget),
        Err(StoreError::BackupCapacity)
    ));
    assert!(!target.exists());
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
