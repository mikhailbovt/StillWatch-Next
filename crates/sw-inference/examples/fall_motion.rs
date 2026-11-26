//! Score one channel-major feature window per line using the configured ONNX Runtime.

use std::{
    fs,
    io::{BufRead, BufWriter, Write},
    path::PathBuf,
};
use sw_domain::{FALL_FEATURE_CHANNELS, FALL_FEATURE_STEPS};
use sw_inference::{CudaProvider, FallMotion, FallPoseProvenance, InferenceError, Runtime};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    runtime: PathBuf,
    runtime_sha256: String,
    model: PathBuf,
    model_sha256: String,
    pose: FallPoseProvenance,
    cuda: Option<CudaOptions>,
    profile_prefix: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CudaOptions {
    device: u16,
    provider_sha256: String,
    shared_sha256: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let configuration = std::env::args_os()
        .nth(1)
        .ok_or("usage: fall_motion CONFIG.json")?;
    let options: Options = serde_json::from_slice(&fs::read(configuration)?)?;
    let runtime = Runtime::load(
        &options.runtime,
        &options.runtime_sha256,
        1,
        options.cuda.map(|cuda| CudaProvider {
            device: cuda.device,
            provider_sha256: cuda.provider_sha256,
            shared_sha256: cuda.shared_sha256,
        }),
    )?;
    let mut model = FallMotion::open(
        &runtime,
        &options.model,
        &options.model_sha256,
        &options.pose,
        options.profile_prefix.as_deref(),
    )?;
    let mut output = BufWriter::new(std::io::stdout().lock());
    for line in std::io::stdin().lock().lines() {
        let channels: Vec<[f32; FALL_FEATURE_STEPS]> = serde_json::from_str(&line?)?;
        let channels: &[[f32; FALL_FEATURE_STEPS]; FALL_FEATURE_CHANNELS] = channels
            .as_slice()
            .try_into()
            .map_err(|_| InferenceError::Tensor)?;
        serde_json::to_writer(&mut output, &model.score(channels)?)?;
        writeln!(output)?;
        output.flush()?;
    }
    if options.profile_prefix.is_some() {
        eprintln!("{}", model.finish_profile()?);
    }
    Ok(())
}
