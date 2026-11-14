#![forbid(unsafe_code)]

use std::{fs, path::Path};

use anyhow::{Context, Result, bail, ensure};
use schemars::{JsonSchema, generate::SchemaSettings};
use sw_protocol::{
    AcknowledgeRequest, AlertFeed, AlertReceipt, AnalysisMetrics, AnalysisStatus, Bootstrap,
    Camera, CameraConfig, CameraPage, CameraPatch, CaptureDevice, Change, CoreStatus, Counter,
    Cursor, Device, DeviceCredentials, EvidencePacket, FeedbackRequest, Health, HealthEventPage,
    Incident, IncidentPage, IosPushNotice, IosPushRegistration, OnvifDevice, OnvifProfile,
    OnvifProfilesRequest, OnvifStream, OnvifStreamRequest, PairingFinish, PairingOffer,
    PairingRequest, Problem, RefreshRequest, ReplayUpload, ResolveRequest, SemanticStatus,
    SilenceRequest, Snapshot, SourceProbe, SourceProbeRequest, StreamGrant, StreamRequest,
    worker::{CaptureRequest, FireProposalResult, InferenceConfig, InferenceResult},
};

fn main() -> Result<()> {
    match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [command, mode] if command == "contracts" && mode == "--write" => contracts(false),
        [command, mode] if command == "contracts" && mode == "--check" => contracts(true),
        _ => bail!("usage: cargo xtask contracts --write | --check"),
    }
}

fn contracts(check: bool) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/contracts/schemas");
    if !check {
        fs::create_dir_all(&root)?;
    }
    write_schema::<Counter>(&root, "Counter", check)?;
    write_schema::<Cursor>(&root, "Cursor", check)?;
    write_schema::<Health>(&root, "Health", check)?;
    write_schema::<HealthEventPage>(&root, "HealthEventPage", check)?;
    write_schema::<CoreStatus>(&root, "CoreStatus", check)?;
    write_schema::<Bootstrap>(&root, "Bootstrap", check)?;
    write_schema::<Camera>(&root, "Camera", check)?;
    write_schema::<CameraConfig>(&root, "CameraConfig", check)?;
    write_schema::<CameraPage>(&root, "CameraPage", check)?;
    write_schema::<CameraPatch>(&root, "CameraPatch", check)?;
    write_schema::<Vec<CaptureDevice>>(&root, "CaptureDevices", check)?;
    write_schema::<Vec<OnvifDevice>>(&root, "OnvifDevices", check)?;
    write_schema::<Vec<OnvifProfile>>(&root, "OnvifProfiles", check)?;
    write_schema::<OnvifProfilesRequest>(&root, "OnvifProfilesRequest", check)?;
    write_schema::<OnvifStream>(&root, "OnvifStream", check)?;
    write_schema::<OnvifStreamRequest>(&root, "OnvifStreamRequest", check)?;
    write_schema::<SourceProbe>(&root, "SourceProbe", check)?;
    write_schema::<SourceProbeRequest>(&root, "SourceProbeRequest", check)?;
    write_schema::<ReplayUpload>(&root, "ReplayUpload", check)?;
    write_schema::<Incident>(&root, "Incident", check)?;
    write_schema::<IncidentPage>(&root, "IncidentPage", check)?;
    write_schema::<EvidencePacket>(&root, "EvidencePacket", check)?;
    write_schema::<PairingRequest>(&root, "PairingRequest", check)?;
    write_schema::<PairingOffer>(&root, "PairingOffer", check)?;
    write_schema::<PairingFinish>(&root, "PairingFinish", check)?;
    write_schema::<RefreshRequest>(&root, "RefreshRequest", check)?;
    write_schema::<Vec<Device>>(&root, "Devices", check)?;
    write_schema::<DeviceCredentials>(&root, "DeviceCredentials", check)?;
    write_schema::<StreamGrant>(&root, "StreamGrant", check)?;
    write_schema::<StreamRequest>(&root, "StreamRequest", check)?;
    write_schema::<Snapshot>(&root, "Snapshot", check)?;
    write_schema::<Change>(&root, "Change", check)?;
    write_schema::<AcknowledgeRequest>(&root, "AcknowledgeRequest", check)?;
    write_schema::<AnalysisStatus>(&root, "AnalysisStatus", check)?;
    write_schema::<AnalysisMetrics>(&root, "AnalysisMetrics", check)?;
    write_schema::<SemanticStatus>(&root, "SemanticStatus", check)?;
    write_schema::<ResolveRequest>(&root, "ResolveRequest", check)?;
    write_schema::<FeedbackRequest>(&root, "FeedbackRequest", check)?;
    write_schema::<SilenceRequest>(&root, "SilenceRequest", check)?;
    write_schema::<AlertFeed>(&root, "AlertFeed", check)?;
    write_schema::<AlertReceipt>(&root, "AlertReceipt", check)?;
    write_schema::<IosPushRegistration>(&root, "IosPushRegistration", check)?;
    write_schema::<IosPushNotice>(&root, "IosPushNotice", check)?;
    write_schema::<Problem>(&root, "Problem", check)?;
    let workers = root.join("../workers/v1");
    if !check {
        fs::create_dir_all(&workers)?;
    }
    write_schema::<CaptureRequest>(&workers, "CaptureRequest", check)?;
    write_schema::<InferenceConfig>(&workers, "InferenceConfig", check)?;
    write_schema::<InferenceResult>(&workers, "InferenceResult", check)?;
    let proposals = root.join("../workers/v2-proposals");
    if !check {
        fs::create_dir_all(&proposals)?;
    }
    write_schema::<FireProposalResult>(&proposals, "FireProposalResult", check)?;
    Ok(())
}

fn write_schema<T: JsonSchema>(root: &Path, name: &str, check: bool) -> Result<()> {
    let path = root.join(format!("{name}.json"));
    let generator = SchemaSettings::draft2020_12().into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let text = serde_json::to_string_pretty(&schema)? + "\n";
    if check {
        let current =
            fs::read_to_string(&path).with_context(|| format!("missing schema {name}"))?;
        ensure!(
            current == text,
            "schema {name} drifted, regenerate with cargo xtask contracts --write"
        );
    } else {
        fs::write(path, text)?;
    }
    Ok(())
}
