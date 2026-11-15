use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use sw_domain::Observation;
use sw_inference::{
    CudaProvider, Image, InferenceError, Letterbox, ModelId, ModelPack, PerceptionBackend,
    Preprocessor, Runtime, TensorSize, decode, decode_fire_with_proposals, trace_fire,
};
use sw_protocol::worker::{
    CONFIG_MAX_BYTES, CudaConfig, ExecutionTiming, FIRE_DIAGNOSTIC_READY, FireProposalResult,
    FrameHeader, HEADER_LEN, INFERENCE_READY, InferenceConfig, InferenceResult, LiveModelProfile,
    MODEL_INITIALIZATION_FAILED, MODEL_PACK_UNAVAILABLE, PtsQuality, RESULT_MAX_BYTES,
    RUNTIME_UNAVAILABLE, TIMED_INFERENCE_READY, Tasks,
};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Legacy,
    Timed,
    TimedProposals,
    FireDiagnostic,
}

pub fn run(mode: Mode) -> Result<()> {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(
        (1..=CONFIG_MAX_BYTES).contains(&length),
        "invalid inference configuration length"
    );
    let mut config = vec![0; length];
    input.read_exact(&mut config)?;
    let config: InferenceConfig = serde_json::from_slice(&config)?;
    let Ok(pack) = ModelPack::open(&config.models, &config.manifest_sha256) else {
        return startup_failure(&mut output, MODEL_PACK_UNAVAILABLE);
    };
    let using_cuda = config.cuda.is_some();
    let Ok(capture_shape) = graph_shape(config.cuda.as_ref()) else {
        return startup_failure(&mut output, MODEL_PACK_UNAVAILABLE);
    };
    let warm_shapes = if let Some(profile) = &config.live_profile {
        match profile_shapes(&pack, profile) {
            Some(shapes) if capture_shape.is_none_or(|size| shapes == [size]) => shapes,
            _ => return startup_failure(&mut output, MODEL_PACK_UNAVAILABLE),
        }
    } else if let Some(size) = capture_shape {
        vec![size]
    } else {
        let inventory = pack.declared_shapes().unwrap_or_default();
        inventory
            .iter()
            .map(|&[h, w]| TensorSize::new(h, w))
            .collect::<Result<Vec<_>, _>>()?
    };
    let runtime = match Runtime::load(
        &config.library,
        &config.library_sha256,
        config.threads,
        config.cuda.map(cuda_provider),
    ) {
        Ok(runtime) => runtime,
        Err(_) => return startup_failure(&mut output, RUNTIME_UNAVAILABLE),
    };
    let load = |id| match capture_shape {
        Some(size) => pack.load_captured(&runtime, id, size),
        None => pack.load(&runtime, id),
    };
    let (mut pose, mut fire) = match load(ModelId::PoseBalanced)
        .and_then(|pose| load(ModelId::Fire).map(|fire| (pose, fire)))
    {
        Ok(models) => models,
        Err(_) => return startup_failure(&mut output, MODEL_INITIALIZATION_FAILED),
    };
    let mut prepare = Preprocessor::new(pack.profile());
    drop(pack);
    let mut pixels = Vec::new();
    if !warm_shapes.is_empty() {
        let mut input = Vec::new();
        for &shape in &warm_shapes {
            input.resize(shape.input_values(), 114.0 / 255.0);
            if pose.evaluate(shape, &input, |_| ()).is_err()
                || fire.evaluate(shape, &input, |_| ()).is_err()
            {
                return startup_failure(&mut output, MODEL_INITIALIZATION_FAILED);
            }
        }
    } else if using_cuda {
        // The first CUDA call can load and compile kernels. Keep that work within
        // the supervised startup deadline and leave the live frame deadline intact.
        pixels.resize(640 * 360 * 3, 0);
        let (tensor, geometry) = prepare.prepare(Image::rgb(640, 360, &pixels)?);
        let size = geometry.tensor_size();
        if pose.evaluate(size, tensor, |_| ()).is_err()
            || fire.evaluate(size, tensor, |_| ()).is_err()
        {
            return startup_failure(&mut output, MODEL_INITIALIZATION_FAILED);
        }
    }
    output.write_all(&match mode {
        Mode::Legacy => INFERENCE_READY,
        Mode::Timed | Mode::TimedProposals => TIMED_INFERENCE_READY,
        Mode::FireDiagnostic => FIRE_DIAGNOSTIC_READY,
    })?;
    output.flush()?;

    let mut payload = Vec::new();
    let timed = matches!(mode, Mode::Timed | Mode::TimedProposals);
    loop {
        let mut id = [0; 16];
        if input.read(&mut id[..1])? == 0 {
            return Ok(());
        }
        let read_started = timed.then(Instant::now);
        input.read_exact(&mut id[1..])?;
        let mut kind = [0];
        input.read_exact(&mut kind)?;
        let tasks = Tasks::from_byte(kind[0])?;
        ensure!(
            mode != Mode::FireDiagnostic || tasks == Tasks::Fire,
            "fire diagnostics accepts only fire requests"
        );
        let mut header = [0; HEADER_LEN];
        input.read_exact(&mut header)?;
        let header = FrameHeader::decode(&header)?;
        ensure!(
            header.pts_quality == PtsQuality::Native && !header.corrupt,
            "invalid inference source clock"
        );
        let capture_ts = header.source_micros.context("missing source time")? as f64 / 1_000_000.0;
        pixels.resize(header.byte_length()? as usize, 0);
        input.read_exact(&mut pixels)?;
        let started = Instant::now();
        let mut timing = ExecutionTiming::default();
        if let Some(read_started) = read_started {
            timing.input_read_us = micros(started.duration_since(read_started));
        }
        let image = Image::rgb(header.width, header.height, &pixels)?;
        let (tensor, geometry) = prepare.prepare(image);
        ensure!(
            warm_shapes.is_empty() || warm_shapes.contains(&geometry.tensor_size()),
            "frame tensor shape falls outside the assigned live model profile"
        );
        if timed {
            timing.preprocess_us = micros(started.elapsed());
        }
        if mode == Mode::FireDiagnostic {
            let (observations, trace) =
                fire.evaluate(geometry.tensor_size(), tensor, |values| {
                    trace_fire(values, image, geometry, capture_ts)
                })??;
            let payload = serde_json::to_vec(&serde_json::json!({
                "request": Uuid::from_bytes(id),
                "source_us": header.source_micros.map(|pts| pts.to_string()),
                "source_dimensions": [header.height, header.width],
                "fire": observations,
                "trace": trace,
            }))?;
            write_result(&mut output, &payload, None)?;
            continue;
        }
        let input = InferenceInput { tensor, geometry };
        let people = if tasks.pose() {
            let (observations, run, decode) = observe(&mut pose, input, timed, |values| {
                decode(ModelId::PoseBalanced, values, image, geometry, capture_ts)
            })?;
            timing.pose_run_us = run;
            timing.pose_decode_us = decode;
            Some(observations)
        } else {
            None
        };
        let mut proposals = Vec::new();
        let hazards = if tasks.fire() {
            let (observations, run, decode) = observe(&mut fire, input, timed, |values| {
                if mode == Mode::TimedProposals {
                    let (detections, weak) =
                        decode_fire_with_proposals(values, image, geometry, capture_ts)?;
                    proposals = weak;
                    Ok(detections)
                } else {
                    decode(ModelId::Fire, values, image, geometry, capture_ts)
                }
            })?;
            timing.fire_run_us = run;
            timing.fire_decode_us = decode;
            Some(observations)
        } else {
            None
        };
        let response = InferenceResult {
            request: Uuid::from_bytes(id),
            elapsed_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            pose: people,
            fire: hazards,
        };
        payload.clear();
        let serialize_started = timed.then(Instant::now);
        if mode == Mode::TimedProposals {
            serde_json::to_writer(
                &mut payload,
                &FireProposalResult {
                    inference: response,
                    fire_proposals: proposals,
                },
            )?;
        } else {
            serde_json::to_writer(&mut payload, &response)?;
        }
        let timed_reply = serialize_started.map(|at| {
            timing.serialize_us = micros(at.elapsed());
            timing
        });
        write_result(&mut output, &payload, timed_reply)?;
    }
}

pub(super) fn write_result(
    output: &mut impl Write,
    payload: &[u8],
    timing: Option<ExecutionTiming>,
) -> Result<()> {
    ensure!(
        payload.len() <= RESULT_MAX_BYTES,
        "inference response exceeds limit"
    );
    output.write_all(&(payload.len() as u32).to_le_bytes())?;
    output.write_all(payload)?;
    if let Some(timing) = timing {
        output.write_all(&timing.encode())?;
    }
    output.flush()?;
    Ok(())
}

#[derive(Clone, Copy)]
struct InferenceInput<'a> {
    tensor: &'a [f32],
    geometry: Letterbox,
}

fn observe(
    backend: &mut impl PerceptionBackend,
    input: InferenceInput<'_>,
    timed: bool,
    decode: impl FnOnce(&[f32]) -> Result<Vec<Observation>, InferenceError>,
) -> Result<(Vec<Observation>, u32, u32), InferenceError> {
    let started = timed.then(Instant::now);
    backend.evaluate(input.geometry.tensor_size(), input.tensor, |values| {
        let decoding = timed.then(Instant::now);
        let observations = decode(values)?;
        let run = started
            .zip(decoding)
            .map_or(0, |(a, b)| micros(b.duration_since(a)));
        let decode = decoding.map_or(0, |at| micros(at.elapsed()));
        Ok((observations, run, decode))
    })?
}

pub(super) fn micros(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
}

fn startup_failure(output: &mut impl Write, marker: [u8; 4]) -> Result<()> {
    output.write_all(&marker)?;
    output.flush()?;
    Ok(())
}

pub fn cuda_provider(config: CudaConfig) -> CudaProvider {
    CudaProvider {
        device: config.device,
        provider_sha256: config.provider_sha256,
        shared_sha256: config.shared_sha256,
    }
}

pub fn graph_shape(config: Option<&CudaConfig>) -> Result<Option<TensorSize>, InferenceError> {
    config
        .and_then(|cuda| cuda.graph_shape)
        .map(|[h, w]| TensorSize::new(usize::from(h), usize::from(w)))
        .transpose()
}

fn profile_shapes(pack: &ModelPack, profile: &LiveModelProfile) -> Option<Vec<TensorSize>> {
    if pack.artifact_digest(ModelId::PoseBalanced)? != profile.pose_balanced_sha256
        || pack.artifact_digest(ModelId::Fire)? != profile.fire_sha256
        || !(1..=16).contains(&profile.warm_shapes.len())
    {
        return None;
    }
    let mut shapes = Vec::with_capacity(profile.warm_shapes.len());
    for &[height, width] in &profile.warm_shapes {
        let shape = TensorSize::new(usize::from(height), usize::from(width)).ok()?;
        if !pack.supports_shape(shape) || shapes.contains(&shape) {
            return None;
        }
        shapes.push(shape);
    }
    Some(shapes)
}
