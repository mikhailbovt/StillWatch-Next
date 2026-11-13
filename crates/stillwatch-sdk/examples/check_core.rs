use std::{path::PathBuf, sync::Arc, time::Duration};

use futures_util::StreamExt;
use serde::Deserialize;
use stillwatch_sdk::{
    Client, DeviceKey, DeviceSession, Error, PairedEndpoint, StreamMessage, enroll,
    protocol::PairingOffer,
};
use uuid::Uuid;

#[derive(Deserialize)]
struct Input {
    offer: PairingOffer,
    camera: Uuid,
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
    let mut changes = client.changes(client.snapshot().await?.cursor);
    let result = tokio::time::timeout(Duration::from_secs(30), changes.next()).await?;
    let Some(Ok(StreamMessage::Change(change))) = result else {
        anyhow::bail!("expected a scoped change");
    };
    let item = match change.kind {
        stillwatch_sdk::protocol::ChangeKind::IncidentOpened(item)
        | stillwatch_sdk::protocol::ChangeKind::IncidentUpdated(item) => item,
    };
    assert_eq!(item.camera_id, input.camera);
    println!("rust pairing and scoped SSE verified");
    Ok(())
}
