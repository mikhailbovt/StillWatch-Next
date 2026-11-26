use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::Mutex,
};

use serde_json::{Value, json};
use sw_protocol::worker::InferenceConfig;

use crate::CameraFrame;

const MAX_TRACE_BYTES: usize = 32 * 1024 * 1024;
const TRUNCATED: &[u8] = b"{\"type\":\"truncated\"}\n";

/// Local, opt-in metadata for a bounded diagnostic run.
pub struct AnalysisReceipts {
    trace: Mutex<Option<Trace>>,
    pub(crate) include_pose_observations: bool,
}

struct Trace {
    file: File,
    bytes: usize,
    sequence: u64,
}

impl AnalysisReceipts {
    pub fn create(path: &Path, model_manifest_sha256: &str) -> io::Result<Self> {
        Self::open(path, model_manifest_sha256, None)
    }

    pub fn create_with_pose_observations(
        path: &Path,
        inference: &InferenceConfig,
        profile: &str,
        pose_model_sha256: &str,
    ) -> io::Result<Self> {
        let provider = if inference.cuda.is_some() {
            "CUDAExecutionProvider"
        } else {
            "CPUExecutionProvider"
        };
        let model = json!({
            "profile": profile, "sha256": pose_model_sha256,
            "provider": provider, "runtime_sha256": inference.library_sha256,
            "cuda": inference.cuda,
        });
        Self::open(path, &inference.manifest_sha256, Some(model))
    }

    fn open(
        path: &Path,
        model_manifest_sha256: &str,
        pose_model: Option<Value>,
    ) -> io::Result<Self> {
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
        let mut file = open.open(path)?;
        let mut header =
            json!({"type": "header", "version": 2, "model_manifest_sha256": model_manifest_sha256});
        let include_pose_observations = pose_model.is_some();
        if let Some(model) = pose_model {
            header["pose_balanced"] = model;
        }
        let header = format!("{header}\n");
        file.write_all(header.as_bytes())?;
        Ok(Self {
            trace: Mutex::new(Some(Trace {
                file,
                bytes: header.len(),
                sequence: 0,
            })),
            include_pose_observations,
        })
    }

    pub(crate) fn policy(&self, frame: &CameraFrame, source_us: i64, task: &str, details: Value) {
        self.record(json!({
            "type": "policy",
            "camera_id": frame.scope.camera_id,
            "epoch": frame.scope.epoch,
            "scene_revision": frame.scope.scene_revision,
            "generation": frame.frame.generation,
            "ordinal": frame.frame.header.frame,
            "source_us": source_us,
            "task": task,
            "details": details,
        }));
    }

    pub(crate) fn record(&self, mut record: Value) {
        let Ok(mut state) = self.trace.lock() else {
            return;
        };
        let Some(trace) = state.as_mut() else { return };
        trace.sequence += 1;
        record["sequence"] = json!(trace.sequence);
        let line = format!("{record}\n");
        if trace.bytes + line.len() + TRUNCATED.len() > MAX_TRACE_BYTES {
            let _ = trace.file.write_all(TRUNCATED);
            *state = None;
            return;
        }
        if let Err(error) = trace.file.write_all(line.as_bytes()) {
            eprintln!("StillWatch stopped writing analysis receipts: {error}");
            *state = None;
        } else {
            trace.bytes += line.len();
        }
    }
}
