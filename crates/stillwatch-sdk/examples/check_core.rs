use std::{path::PathBuf, sync::Arc, time::Duration};

use futures_util::StreamExt;
use serde::Deserialize;
use stillwatch_sdk::{
    ChangeStream, Client, DeviceKey, DeviceSession, Error, PairedEndpoint, StreamMessage, enroll,
    protocol::{Change, ChangeKind, Cursor, PairingOffer, ProblemCode},
};
use uuid::Uuid;

#[derive(Deserialize)]
struct Input {
    offer: PairingOffer,
    camera: Uuid,
    reconnect_markers: Option<PathBuf>,
}

async fn next_change(changes: &mut ChangeStream) -> anyhow::Result<Box<Change>> {
    while let Some(message) = changes.next().await {
        match message {
            Ok(StreamMessage::Change(change)) => return Ok(change),
            Err(
                Error::Transport
                | Error::Deadline
                | Error::Http {
                    status: 502..=504, ..
                },
            ) => {}
            other => anyhow::bail!("expected a scoped change, received {other:?}"),
        }
    }
    anyhow::bail!("change stream ended")
}

fn incident(change: &Change) -> &stillwatch_sdk::protocol::Incident {
    match &change.kind {
        ChangeKind::IncidentOpened(item) | ChangeKind::IncidentUpdated(item) => item,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let [fixture, input] = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected fixture and enrollment file"))?;
    let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture)?)?;
    for valid in fixture["counter_valid"].as_array().unwrap() {
        let parsed: stillwatch_sdk::protocol::Counter = serde_json::from_value(valid.clone())?;
        assert_eq!(serde_json::to_value(parsed)?, *valid);
    }
    for invalid in fixture["counter_invalid"].as_array().unwrap() {
        assert!(
            serde_json::from_value::<stillwatch_sdk::protocol::Counter>(invalid.clone()).is_err()
        );
    }
    let input: Input = serde_json::from_slice(&std::fs::read(input)?)?;
    let wrong_identity = PairedEndpoint::new(
        &input.offer.endpoint,
        Uuid::new_v4(),
        &input.offer.tls_certificate_sha256,
        &input.offer.tls_certificate_der,
    )?;
    let key = DeviceKey::generate()?;
    let (endpoint, credentials) = enroll(input.offer, &key, "SDK QA Rust".into()).await?;
    #[cfg(feature = "blocking")]
    {
        let endpoint = endpoint.clone();
        let mismatched = wrong_identity.clone();
        let token = credentials.access_token.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let wrong = stillwatch_sdk::blocking::Client::pinned(&mismatched, token.clone())?;
            assert!(matches!(wrong.capabilities(), Err(Error::Identity)));
            let client = stillwatch_sdk::blocking::Client::pinned(&endpoint, token)?;
            assert_eq!(client.capabilities()?.instance_id, endpoint.instance());
            let active = client.health_events(None, 1, true)?;
            assert_eq!(active.events.len(), 1);
            assert_eq!(active.events[0].camera_id, None);
            Ok(())
        })
        .await??;
    }
    let session = Arc::new(DeviceSession::new(&endpoint, key, credentials, None)?);
    let wrong = Client::pinned(&wrong_identity, session.clone())?;
    assert!(matches!(wrong.capabilities().await, Err(Error::Identity)));
    let client = Client::pinned(&endpoint, session)?;
    assert_eq!(
        client.capabilities().await?.instance_id,
        endpoint.instance()
    );
    let cameras = client.cameras(None, 100).await?;
    assert_eq!(cameras.cameras.len(), 1);
    assert_eq!(cameras.cameras[0].id, input.camera);
    let health = client.health_events(None, 1, false).await?;
    assert_eq!(health.events[0].camera_id, Some(input.camera));
    let older = client.health_events(health.next, 1, false).await?;
    assert_eq!(older.events[0].camera_id, None);
    assert!(health.next.is_some() && older.next.is_none());
    let mut changes = client.changes(client.snapshot().await?.cursor);
    if let Some(directory) = &input.reconnect_markers {
        std::fs::File::create_new(directory.join("ready-rust"))?;
    }
    let first = tokio::time::timeout(Duration::from_secs(30), next_change(&mut changes)).await??;
    assert_eq!(incident(&first).camera_id, input.camera);
    if let Some(directory) = &input.reconnect_markers {
        assert_eq!(incident(&first).detection_reason, "fixture 0");
        std::fs::File::create_new(directory.join("after-first-rust"))?;
        let next =
            tokio::time::timeout(Duration::from_secs(45), next_change(&mut changes)).await??;
        assert_eq!(incident(&next).camera_id, input.camera);
        assert_eq!(incident(&next).detection_reason, "fixture 1000");
        assert_eq!(next.stream_generation, first.stream_generation);
        assert!(next.sequence > first.sequence);
        drop(changes);

        let mut expired = client.changes(Cursor {
            generation: Uuid::new_v4(),
            sequence: next.sequence,
        });
        let Some(Err(error @ Error::Http { status: 409, .. })) =
            tokio::time::timeout(Duration::from_secs(5), expired.next()).await?
        else {
            anyhow::bail!("expected a cursor expiration problem");
        };
        assert_eq!(
            error.problem().map(|value| value.code),
            Some(ProblemCode::CursorExpired)
        );
        let fresh = client.snapshot().await?;
        assert_eq!(fresh.cursor, next.cursor());
        assert_eq!(fresh.incidents.len(), 2);
        assert!(
            fresh
                .incidents
                .iter()
                .all(|item| item.camera_id == input.camera)
        );
    }
    println!("rust pairing and scoped SSE verified");
    Ok(())
}
