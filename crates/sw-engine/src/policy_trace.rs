use std::io::{self, BufRead, BufReader, Read, Write};

use serde::Serialize;
use sw_domain::{
    Box2d, FireDecision, FireFacts, FirePolicy, FireSample, FireSettings, Observation,
    ObservationLabel, PoseTracker, SourceScope,
};
use sw_protocol::worker::{
    FireProposalResult, FrameHeader, HEADER_LEN, InferenceResult, PtsQuality, RESULT_MAX_BYTES,
};
use uuid::Uuid;

use crate::scene;

#[derive(Serialize)]
struct RegionDecision {
    region: Box2d,
    decision: &'static str,
    facts: FireFacts,
}

/// Replay the default fire policy on the exact masked frames and observations
/// supplied by the local detector diagnostic. There is no storage or delivery.
pub fn trace_fire_policy(input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut input = BufReader::new(input);
    let mut policy = FirePolicy::new(FireSettings::default()).expect("valid default fire policy");
    let mut scope = SourceScope {
        camera_id: Uuid::nil(),
        epoch: 0,
        scene_revision: 0,
    };
    let mut last = None;
    let mut rgb = Vec::new();
    let mut body = Vec::new();
    output.write_all(b"SWP1")?;
    output.flush()?;
    loop {
        let mut length = [0; 4];
        if input.read(&mut length[..1])? == 0 {
            return Ok(());
        }
        input.read_exact(&mut length[1..])?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 {
            scope.epoch = scope.epoch.checked_add(1).ok_or_else(invalid_frame)?;
            last = None;
            continue;
        }
        if length > RESULT_MAX_BYTES || scope.epoch == 0 {
            return Err(invalid_frame());
        }
        body.resize(length, 0);
        input.read_exact(&mut body)?;
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum TraceObservation {
            Proposed(FireProposalResult),
            Basic(InferenceResult),
        }
        let (observations, proposals) = match serde_json::from_slice(&body)? {
            TraceObservation::Proposed(result) => (result.inference, result.fire_proposals),
            TraceObservation::Basic(result) => (result, Vec::new()),
        };
        if proposals.len() > 8
            || proposals.iter().any(|p| {
                p.label == ObservationLabel::Person
                    || !p.valid()
                    || !(f64::from(0.12_f32)..=f64::from(0.20_f32)).contains(&p.score)
            })
        {
            return Err(invalid_frame());
        }
        let regions = observations
            .fire
            .filter(|items| items.len() <= 300)
            .ok_or_else(invalid_frame)?;
        let mut bytes = [0; HEADER_LEN];
        input.read_exact(&mut bytes)?;
        let header = FrameHeader::decode(&bytes)?;
        let pts = header.source_micros.ok_or_else(invalid_frame)?;
        if header.pts_quality != PtsQuality::Native
            || header.corrupt
            || last.is_some_and(|value| pts <= value)
        {
            return Err(invalid_frame());
        }
        last = Some(pts);
        rgb.resize(header.byte_length()? as usize, 0);
        input.read_exact(&mut rgb)?;
        let visibility = scene::measure_rgb(header.width, header.height, &rgb).visibility;
        let view = scene::VisibleFrame {
            width: header.width as usize,
            height: header.height as usize,
            rgb: &rgb,
            masks: &[],
            overview: visibility,
        };
        let decisions = policy
            .update(FireSample {
                scope,
                source_us: pts,
                dimensions: (header.width, header.height),
                regions: &regions,
                weak_proposals: &proposals,
                visibility,
                region_visibility: &|observation| {
                    view.region(
                        observation.region,
                        observation.label == ObservationLabel::Fire,
                    )
                },
                controlled_overlap: &|_| None,
            })
            .into_iter()
            .map(|episode| RegionDecision {
                region: episode.region,
                decision: match episode.decision {
                    FireDecision::Review => "review",
                    FireDecision::ControlledContext => "controlled_context",
                    FireDecision::Observing => "observing",
                },
                facts: episode.facts,
            })
            .collect::<Vec<_>>();
        let response = serde_json::to_vec(&serde_json::json!({
            "source_us": pts.to_string(), "visibility": visibility, "regions": decisions,
        }))?;
        output.write_all(&(response.len() as u32).to_le_bytes())?;
        output.write_all(&response)?;
        output.flush()?;
    }
}

fn invalid_frame() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid policy trace frame")
}

/// One JSON line per accepted pose inference result. Inputs contain detector
/// observations from one camera with a native source clock.
pub fn trace_pose_association(input: impl Read, mut output: impl Write) -> io::Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        epoch: u64,
        source_seconds: f64,
        observations: Vec<Observation>,
    }

    let mut input = BufReader::new(input);
    let mut tracker = PoseTracker::default();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        if (&mut input)
            .take(RESULT_MAX_BYTES as u64 + 1)
            .read_until(b'\n', &mut buffer)?
            == 0
        {
            return Ok(());
        }
        if buffer.len() > RESULT_MAX_BYTES {
            return Err(invalid_frame());
        }
        let Sample {
            epoch,
            source_seconds,
            observations,
        } = serde_json::from_slice(&buffer)?;
        let mut diagnostics = Vec::new();
        tracker
            .associate(observations, source_seconds, epoch, &mut |result| {
                diagnostics.push(result)
            })
            .map_err(io::Error::other)?;
        serde_json::to_writer(&mut output, &diagnostics)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}
