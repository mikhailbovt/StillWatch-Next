use std::time::Duration;

use process_wrap::tokio::ChildWrapper;
use sw_domain::{Observation, ObservationLabel};
use sw_protocol::worker::{
    CONFIG_MAX_BYTES, INFERENCE_READY, InferenceConfig, InferenceResult, RESULT_MAX_BYTES, Tasks,
    inference_startup_failure,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, ChildStdout},
};
use uuid::Uuid;

use crate::{Frame, WorkerBinary, WorkerError, capture_process::reap};

pub(crate) struct InferenceProcess {
    child: Option<Box<dyn ChildWrapper>>,
    stdin: ChildStdin,
    stdout: ChildStdout,
    result: Vec<u8>,
}

impl InferenceProcess {
    pub async fn start(
        binary: &WorkerBinary,
        config: &InferenceConfig,
    ) -> Result<Self, WorkerError> {
        let mut child = binary.spawn("infer")?;
        let stdin = child.stdin().take().ok_or(WorkerError::Unavailable)?;
        let stdout = child.stdout().take().ok_or(WorkerError::Unavailable)?;
        let mut process = Self {
            child: Some(child),
            stdin,
            stdout,
            result: Vec::new(),
        };
        let initialized =
            tokio::time::timeout(Duration::from_secs(30), process.bootstrap(config)).await;
        match initialized {
            Ok(Ok(())) => Ok(process),
            Ok(Err(error)) => {
                process.stop().await?;
                Err(error)
            }
            Err(_) => {
                process.stop().await?;
                Err(WorkerError::Deadline)
            }
        }
    }

    async fn bootstrap(&mut self, config: &InferenceConfig) -> Result<(), WorkerError> {
        let payload = serde_json::to_vec(config).map_err(|_| WorkerError::Config)?;
        if payload.is_empty() || payload.len() > CONFIG_MAX_BYTES {
            return Err(WorkerError::Config);
        }
        self.stdin.write_u32_le(payload.len() as u32).await?;
        self.stdin.write_all(&payload).await?;
        self.stdin.flush().await?;
        let mut ready = [0; 4];
        self.stdout.read_exact(&mut ready).await?;
        if let Some(failure) = inference_startup_failure(ready) {
            return Err(WorkerError::InferenceStartup(failure));
        }
        if ready != INFERENCE_READY {
            return Err(WorkerError::Config);
        }
        Ok(())
    }

    pub async fn infer(
        &mut self,
        frame: &Frame,
        tasks: Tasks,
    ) -> Result<InferenceResult, WorkerError> {
        tokio::time::timeout(Duration::from_secs(2), self.exchange(frame, tasks))
            .await
            .map_err(|_| WorkerError::Deadline)?
    }

    async fn exchange(
        &mut self,
        frame: &Frame,
        tasks: Tasks,
    ) -> Result<InferenceResult, WorkerError> {
        let request = Uuid::new_v4();
        self.stdin.write_all(request.as_bytes()).await?;
        self.stdin.write_u8(tasks.byte()).await?;
        self.stdin.write_all(&frame.header.encode()?).await?;
        self.stdin.write_all(&frame.rgb).await?;
        self.stdin.flush().await?;
        let length = self.stdout.read_u32_le().await? as usize;
        if !(1..=RESULT_MAX_BYTES).contains(&length) {
            return Err(WorkerError::Config);
        }
        self.result.resize(length, 0);
        self.stdout.read_exact(&mut self.result).await?;
        let result: InferenceResult =
            serde_json::from_slice(&self.result).map_err(|_| WorkerError::Config)?;
        if result.request != request
            || result.pose.is_some() != tasks.pose()
            || result.fire.is_some() != tasks.fire()
        {
            return Err(WorkerError::Config);
        }
        if result
            .pose
            .as_deref()
            .is_some_and(|items| !validate(items, frame, true))
            || result
                .fire
                .as_deref()
                .is_some_and(|items| !validate(items, frame, false))
        {
            return Err(WorkerError::Config);
        }
        Ok(result)
    }

    pub async fn stop(mut self) -> Result<(), WorkerError> {
        if let Some(child) = &mut self.child {
            reap(&mut **child).await?;
        }
        self.child = None;
        Ok(())
    }
}

impl Drop for InferenceProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

fn validate(items: &[Observation], frame: &Frame, pose: bool) -> bool {
    let width = f64::from(frame.header.width);
    let height = f64::from(frame.header.height);
    let Some(source) = frame.header.source_micros else {
        return false;
    };
    let capture_ts = source as f64 / 1_000_000.0;
    items.len() <= 300
        && items.iter().all(|item| {
            item.valid()
                && item.track_id.is_none()
                && (item.capture_ts - capture_ts).abs() <= 1e-8
                && item.region.x1 >= 0.0
                && item.region.y1 >= 0.0
                && item.region.x2 <= width
                && item.region.y2 <= height
                && if pose {
                    item.label == ObservationLabel::Person && item.keypoints.len() == 17
                } else {
                    item.label != ObservationLabel::Person && item.keypoints.is_empty()
                }
        })
}
