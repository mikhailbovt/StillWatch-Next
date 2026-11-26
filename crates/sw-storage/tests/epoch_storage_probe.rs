//! Durable openings and the scope requirements for current observations.
use rusqlite::Connection;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
use sw_domain::{
    AnalysisOrigin, Assessment, Attention, Box2d, Category, DomainError, EvidenceState,
    NewIncident, SourceScope,
};
use sw_protocol::{Dimensions, EvidenceVisibility, LocalFacts};
use sw_storage::{
    CameraAction, CameraCommand, CameraLimits, CameraRecord, ObservationTime, OpeningDecision,
    Store, StoreError, utc_ms,
};
use uuid::Uuid;

const CRASH_OPENINGS: u32 = 34;

#[tokio::test]
async fn direct_openings_still_require_the_current_scope_on_first_write() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), Default::default()).unwrap();
    let camera = camera(&store).await;
    let (new, observed) = opening(&camera);
    let mut still_queued = new.clone();
    still_queued.id = Uuid::new_v4();
    let saved = store
        .create_observed_hazard(new.clone(), observed)
        .await
        .unwrap();
    store
        .bump_camera_epoch(camera.id, camera.revision)
        .await
        .unwrap();
    let retried = store.create_observed_hazard(new, observed).await.unwrap();
    assert_eq!(saved.id, retried.id);
    let result = store
        .create_observed_hazard(still_queued.clone(), observed)
        .await;
    assert!(matches!(
        result,
        Err(StoreError::Domain(DomainError::Stale))
    ));
    assert!(matches!(
        store.get(still_queued.id).await,
        Err(StoreError::NotFound)
    ));
}

async fn camera(store: &Store) -> CameraRecord {
    store
        .mutate_camera(
            CameraCommand {
                actor: Uuid::new_v4(),
                key: "epoch-loss-probe".parse().unwrap(),
                action: CameraAction::Create(
                    serde_json::from_value(serde_json::json!({
                        "name": "Camera", "enabled": true,
                        "source": {"type": "usb", "device_id": "sample"}
                    }))
                    .unwrap(),
                ),
            },
            Some(Dimensions {
                width: 160,
                height: 120,
            }),
            None,
            CameraLimits {
                active: 1,
                raw_bytes: 1_048_576,
            },
            utc_ms(),
        )
        .await
        .unwrap()
        .camera
}

fn opening(camera: &CameraRecord) -> (NewIncident, ObservationTime) {
    let observed = ObservationTime::new(utc_ms(), Instant::now());
    let new = NewIncident {
        id: Uuid::new_v4(),
        scope: SourceScope {
            camera_id: camera.id,
            epoch: camera.epoch,
            scene_revision: camera.scene_revision,
        },
        category: Category::Fire,
        reason: "visible_flame".into(),
        analysis_origin: Some(AnalysisOrigin {
            model_manifest_sha256: "a".repeat(64),
            policy_version: "local-policy-9".into(),
        }),
        first_source_us: 0,
        last_source_us: 500_000,
        live: true,
        observed_host_utc: Some(observed.observed_host_utc),
        assessment: Assessment::ImmediateVisibleConcern,
    };
    (new, observed)
}

fn decision(new: NewIncident) -> OpeningDecision {
    OpeningDecision::new(
        new,
        LocalFacts::Fire {
            flame_visible: true,
            smoke_visible: false,
            apparent_growth: false,
            smoke_growth: false,
            escaped_controlled_region: false,
            controlled_region_overlap: None,
            continuity_samples: 2,
            detector_confidence: 0.8,
            visibility: EvidenceVisibility::Good,
        },
        Box2d {
            x1: 4.,
            y1: 12.,
            x2: 24.,
            y2: 32.,
        },
        None,
    )
    .unwrap()
}

fn sql_count(path: &Path, query: &str) -> u32 {
    Connection::open(path.join("stillwatch.sqlite3"))
        .unwrap()
        .query_row(query, [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn an_accepted_opening_survives_a_busy_writer_and_epoch_change_once() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), Default::default()).unwrap();
    let camera = camera(&store).await;
    let (new, observed) = opening(&camera);
    let accepted = decision(new.clone());
    let blocker = Connection::open(directory.path().join("stillwatch.sqlite3")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    store.accept_openings(vec![accepted.clone()]).await.unwrap();
    assert!(
        store
            .commit_accepted_opening(new.id, observed)
            .await
            .is_err()
    );
    assert!(matches!(store.get(new.id).await, Err(StoreError::NotFound)));
    blocker.execute_batch("ROLLBACK").unwrap();
    store
        .bump_camera_epoch(camera.id, camera.revision)
        .await
        .unwrap();
    let historical = store
        .commit_accepted_opening(new.id, observed)
        .await
        .unwrap();
    assert_eq!(historical.scope, new.scope);
    assert_eq!(historical.opening_observed_host_utc, new.observed_host_utc);
    assert!(historical.opening_delayed);
    assert_eq!(historical.attention, Attention::Review);
    assert_eq!(historical.evidence_state, EvidenceState::Unavailable);

    // Replaying a surviving journal row also covers a lost acknowledgement.
    store.accept_openings(vec![accepted]).await.unwrap();
    drop(store);
    let recovered = Store::open(directory.path(), Default::default()).unwrap();
    assert_eq!(recovered.get(new.id).await.unwrap(), historical);
    assert_eq!(
        sql_count(
            directory.path(),
            "SELECT count(*) FROM change_events WHERE opened=1"
        ),
        1
    );
    assert_eq!(
        sql_count(directory.path(), "SELECT count(*) FROM notification_outbox"),
        0
    );
    assert_eq!(
        sql_count(directory.path(), "SELECT count(*) FROM hazard_observations"),
        1
    );
}

#[tokio::test]
async fn a_deleted_camera_keeps_accepted_history_without_observation_geometry() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), Default::default()).unwrap();
    let camera = camera(&store).await;
    let (new, observed) = opening(&camera);
    store
        .accept_openings(vec![decision(new.clone())])
        .await
        .unwrap();
    store
        .mutate_camera(
            CameraCommand {
                actor: Uuid::new_v4(),
                key: "delete-camera-before-opening".parse().unwrap(),
                action: CameraAction::Delete {
                    id: camera.id,
                    revision: camera.revision,
                },
            },
            None,
            None,
            CameraLimits {
                active: 1,
                raw_bytes: 1_048_576,
            },
            utc_ms(),
        )
        .await
        .unwrap();
    let historical = store
        .commit_accepted_opening(new.id, observed)
        .await
        .unwrap();
    assert!(historical.opening_delayed);
    assert_eq!(historical.evidence_state, EvidenceState::Unavailable);
    assert_eq!(
        sql_count(directory.path(), "SELECT count(*) FROM hazard_observations"),
        0
    );
    assert_eq!(
        sql_count(directory.path(), "SELECT count(*) FROM notification_outbox"),
        0
    );
}

#[tokio::test]
#[ignore = "fixture executed by the process crash test"]
async fn journal_before_crash_child() {
    let root =
        std::path::PathBuf::from(std::env::var_os("STILLWATCH_OPENING_CRASH_FIXTURE").unwrap());
    let store = Store::open(&root, Default::default()).unwrap();
    let camera = camera(&store).await;
    let openings = (0..CRASH_OPENINGS).map(|_| opening(&camera).0);
    store
        .accept_openings(openings.map(decision).collect())
        .await
        .unwrap();
    std::fs::write(root.join("accepted"), b"ready").unwrap();
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn an_abrupt_exit_recovers_every_accepted_opening_as_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "journal_before_crash_child"])
        .env("STILLWATCH_OPENING_CRASH_FIXTURE", directory.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.path().join("accepted").is_file() {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child failed to make the openings durable");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    assert_eq!(
        sql_count(directory.path(), "SELECT count(*) FROM incidents"),
        0
    );
    for _ in 0..2 {
        let store = Store::open(directory.path(), Default::default()).unwrap();
        let now = utc_ms();
        let mut passes = 0;
        while store.maintenance_step(now - 1, now - 1, now).await.unwrap() {
            passes += 1;
            assert!(passes <= 2);
        }
        let history = store
            .history(None, CRASH_OPENINGS as usize)
            .await
            .unwrap()
            .incidents;
        assert_eq!(history.len(), CRASH_OPENINGS as usize);
        assert!(
            history
                .iter()
                .all(|row| row.opening_delayed && row.evidence_state == EvidenceState::Unavailable)
        );
        assert_eq!(
            sql_count(
                directory.path(),
                "SELECT count(*) FROM change_events WHERE opened=1"
            ),
            CRASH_OPENINGS
        );
        assert_eq!(
            sql_count(directory.path(), "SELECT count(*) FROM notification_outbox"),
            0
        );
    }
}
