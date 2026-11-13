use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use sw_inference::{
    CudaProvider, Image, InferenceError, ModelId, ModelPack, Preprocessor, Runtime, decode,
};
use sw_protocol::worker::{
    CONFIG_MAX_BYTES, CudaConfig, FrameHeader, HEADER_LEN, INFERENCE_READY, InferenceConfig,
    InferenceResult, MODEL_INITIALIZATION_FAILED, MODEL_PACK_UNAVAILABLE, PtsQuality,
    RESULT_MAX_BYTES, RUNTIME_UNAVAILABLE, Tasks,
};
use uuid::Uuid;

pub fn run() -> Result<()> {
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
    let runtime = match Runtime::load(
        &config.library,
        &config.library_sha256,
        config.threads,
        config.cuda.map(cuda_provider),
    ) {
        Ok(runtime) => runtime,
        Err(_) => return startup_failure(&mut output, RUNTIME_UNAVAILABLE),
    };
    let (mut pose, mut fire) = match pack
        .load(&runtime, ModelId::PoseBalanced)
        .and_then(|pose| pack.load(&runtime, ModelId::Fire).map(|fire| (pose, fire)))
    {
        Ok(models) => models,
        Err(
            InferenceError::Io(_) | InferenceError::DigestMismatch | InferenceError::ArtifactSize,
        ) => {
            return startup_failure(&mut output, MODEL_PACK_UNAVAILABLE);
        }
        Err(_) => return startup_failure(&mut output, MODEL_INITIALIZATION_FAILED),
    };
    let mut prepare = Preprocessor::new(pack.profile());
    let mut pixels = Vec::new();
    if using_cuda {
        // The first CUDA call can load and compile kernels. Keep that work within
        // the supervised startup deadline and leave the live frame deadline intact.
        pixels.resize(640 * 360 * 3, 0);
        let (tensor, geometry) = prepare.prepare(Image::rgb(640, 360, &pixels)?);
        let size = geometry.tensor_size();
        if pose.run(size, tensor, |_| ()).is_err() || fire.run(size, tensor, |_| ()).is_err() {
            return startup_failure(&mut output, MODEL_INITIALIZATION_FAILED);
        }
    }
    output.write_all(&INFERENCE_READY)?;
    output.flush()?;

    loop {
        let mut id = [0; 16];
        if input.read(&mut id[..1])? == 0 {
            return Ok(());
        }
        input.read_exact(&mut id[1..])?;
        let mut kind = [0];
        input.read_exact(&mut kind)?;
        let tasks = Tasks::from_byte(kind[0])?;
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
        let image = Image::rgb(header.width, header.height, &pixels)?;
        let (tensor, geometry) = prepare.prepare(image);
        let people = if tasks.pose() {
            Some(pose.run(geometry.tensor_size(), tensor, |values| {
                decode(ModelId::PoseBalanced, values, image, geometry, capture_ts)
            })??)
        } else {
            None
        };
        let hazards = if tasks.fire() {
            Some(fire.run(geometry.tensor_size(), tensor, |values| {
                decode(ModelId::Fire, values, image, geometry, capture_ts)
            })??)
        } else {
            None
        };
        let response = InferenceResult {
            request: Uuid::from_bytes(id),
            elapsed_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            pose: people,
            fire: hazards,
        };
        let payload = serde_json::to_vec(&response)?;
        ensure!(
            payload.len() <= RESULT_MAX_BYTES,
            "inference response exceeds limit"
        );
        output.write_all(&(payload.len() as u32).to_le_bytes())?;
        output.write_all(&payload)?;
        output.flush()?;
    }
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
