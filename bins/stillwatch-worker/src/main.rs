#![forbid(unsafe_code)]

mod capture;
mod inference;
mod parity;

use std::{
    ffi::OsString,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use sw_capture::{Decoder, PtsQuality, ReadLimits};
use sw_protocol::worker::{CAPTURE_SOURCE_EXIT, CudaConfig};

fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [command] if command == "capture" => match capture::run() {
            Err(error) if error.is::<sw_capture::CaptureError>() => {
                eprintln!("Capture source is unavailable");
                std::process::exit(CAPTURE_SOURCE_EXIT);
            }
            result => result,
        },
        [command] if command == "capture-devices" => capture::devices(),
        [command] if command == "infer" => inference::run(),
        [command] if command == "capture-doctor" => {
            println!(
                "{}",
                json!({"rtsp_peer_confined": sw_capture::confined_rtsp_available()})
            );
            Ok(())
        }
        [command] if command == "version" || command == "--version" => {
            println!("stillwatch-worker {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [command, source, count] if command == "decode-file" => {
            decode_file(Path::new(source), text_arg(count)?.parse()?)
        }
        [
            command,
            library,
            library_sha,
            model_pack,
            manifest_sha,
            flags @ ..,
        ] if command == "doctor" || command == "verify-model-pack" => {
            let cuda = match flags {
                [] => None,
                [option, device, provider, shared] if option == "--cuda" => {
                    Some(inference::cuda_provider(CudaConfig {
                        device: text_arg(device)?.parse()?,
                        provider_sha256: text_arg(provider)?.to_owned(),
                        shared_sha256: text_arg(shared)?.to_owned(),
                    }))
                }
                _ => bail!("expected --cuda DEVICE PROVIDER_SHA256 SHARED_SHA256"),
            };
            let library = Path::new(library);
            let library_sha = text_arg(library_sha)?;
            let model_pack = Path::new(model_pack);
            let manifest_sha = text_arg(manifest_sha)?;
            if command == "verify-model-pack" {
                return parity::verify(library, library_sha, model_pack, manifest_sha, cuda);
            }
            let provider = if cuda.is_some() { "cuda" } else { "cpu" };
            let pack = sw_inference::ModelPack::open(model_pack, manifest_sha)?;
            let runtime = sw_inference::Runtime::load(library, library_sha, 1, cuda)?;
            for id in sw_inference::ModelId::ALL {
                pack.load(&runtime, id)?;
            }
            println!(
                "{}",
                json!({"capture": true, "rtsp_peer_confined": sw_capture::confined_rtsp_available(),
                    "inference": true, "models": 3, "provider": provider})
            );
            Ok(())
        }
        _ => bail!(
            "usage: stillwatch-worker decode-file VIDEO COUNT | verify-model-pack ORT_LIBRARY ORT_SHA256 MODEL_PACK MANIFEST_SHA256"
        ),
    }
}

fn text_arg(value: &OsString) -> Result<&str> {
    value.to_str().context("argument must be UTF-8")
}

fn decode_file(path: &Path, count: usize) -> Result<()> {
    ensure!(
        (1..=50_000).contains(&count),
        "frame count must be in 1..=50000"
    );
    let mut reader = Decoder::open_file(
        path,
        ReadLimits::default(),
        Arc::new(AtomicBool::new(false)),
    )?;
    let mut decoded = 0;
    let mut native_pts = 0;
    let mut first_source_micros = None;
    let mut last_source_micros = None;
    let mut regressions = 0;
    let mut dimensions = None;
    let mut corrupt_frames = 0;
    for _ in 0..count {
        let Some(frame) = reader.next_frame()? else {
            break;
        };
        decoded += 1;
        dimensions = Some([frame.width, frame.height]);
        native_pts += usize::from(frame.timestamp.quality == PtsQuality::Native);
        corrupt_frames += usize::from(frame.corrupt);
        if let Some(micros) = frame.timestamp.micros {
            first_source_micros.get_or_insert(micros);
            if last_source_micros.is_some_and(|last| last >= micros) {
                regressions += 1;
            }
            last_source_micros = Some(micros);
        }
    }
    ensure!(decoded > 0, "video contains no decodable frames");
    println!(
        "{}",
        json!({
            "frames": decoded, "dimensions": dimensions,
            "native_pts": native_pts, "non_increasing_pts": regressions,
            "first_source_micros": first_source_micros, "last_source_micros": last_source_micros,
            "corrupt_frames": corrupt_frames,
        })
    );
    Ok(())
}
