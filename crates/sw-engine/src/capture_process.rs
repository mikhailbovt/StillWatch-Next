use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use process_wrap::tokio::ChildWrapper;
use sw_protocol::CaptureDevice;
use sw_protocol::worker::{
    CAPTURE_SOURCE_EXIT, CONFIG_MAX_BYTES, CaptureRequest, FrameHeader, HEADER_LEN,
    WINDOWS_RTSPS_OPEN_TIMEOUT,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::ChildStdout,
    sync::{OwnedSemaphorePermit, Semaphore},
};
use uuid::Uuid;

use crate::child_process::command;

const CONFIG_DEADLINE: Duration = Duration::from_secs(2);
const EXIT_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("worker executable is unavailable")]
    Unavailable,
    #[error("capture worker does not enforce RTSP peer confinement")]
    NetworkPolicyUnavailable,
    #[error("capture worker configuration is invalid")]
    Config,
    #[error("capture worker pipe failed")]
    Io(#[from] io::Error),
    #[error("capture worker failed")]
    Exit,
    #[error("capture source is unavailable")]
    Source,
    #[error("capture worker did not respond before the deadline")]
    Deadline,
    #[error("capture worker termination could not be verified")]
    Unreaped,
    #[error("inference worker could not initialize its model or runtime")]
    InferenceStartup(sw_protocol::InferenceFailure),
}

#[derive(Clone)]
pub struct WorkerBinary(PathBuf);

impl WorkerBinary {
    pub fn open(path: &Path) -> Result<Self, WorkerError> {
        let path = path.canonicalize().map_err(|_| WorkerError::Unavailable)?;
        if !path.is_file() {
            return Err(WorkerError::Unavailable);
        }
        Ok(Self(path))
    }

    pub async fn capture(&self, config: &CaptureRequest) -> Result<CaptureProcess, WorkerError> {
        let input = serde_json::to_vec(config).map_err(|_| WorkerError::Config)?;
        if input.len() > CONFIG_MAX_BYTES {
            return Err(WorkerError::Config);
        }
        let mut child = self.spawn("capture")?;
        let stdout = child.stdout().take().ok_or(WorkerError::Unavailable)?;
        let mut stdin = child.stdin().take().ok_or(WorkerError::Unavailable)?;
        let process = CaptureProcess {
            child: Some(child),
            stdout,
            generation: Uuid::new_v4(),
            last_frame: 0,
            schannel: config.source.uses_schannel(),
        };
        let sent = tokio::time::timeout(CONFIG_DEADLINE, stdin.write_all(&input)).await;
        drop(stdin);
        match sent {
            Ok(Ok(())) => Ok(process),
            failure => {
                process.stop().await?;
                match failure {
                    Ok(Err(error)) => Err(error.into()),
                    _ => Err(WorkerError::Deadline),
                }
            }
        }
    }

    pub async fn devices(&self) -> Result<Vec<CaptureDevice>, WorkerError> {
        self.capture_metadata("capture-devices", 64 * 1024).await
    }

    pub async fn confined_rtsp(&self) -> Result<bool, WorkerError> {
        #[derive(serde::Deserialize)]
        struct CaptureCapabilities {
            rtsp_peer_confined: bool,
        }
        let capabilities: CaptureCapabilities =
            self.capture_metadata("capture-doctor", 1024).await?;
        Ok(capabilities.rtsp_peer_confined)
    }

    async fn capture_metadata<T: serde::de::DeserializeOwned>(
        &self,
        command: &str,
        limit: u64,
    ) -> Result<T, WorkerError> {
        let mut child = self.spawn(command)?;
        drop(child.stdin().take());
        let stdout = child.stdout().take().ok_or(WorkerError::Unavailable)?;
        let mut bytes = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            stdout.take(limit + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > limit {
                return Err(WorkerError::Config);
            }
            if !child.wait().await?.success() {
                return Err(WorkerError::Exit);
            }
            serde_json::from_slice(&bytes).map_err(|_| WorkerError::Config)
        })
        .await;
        match result {
            Ok(Ok(devices)) => Ok(devices),
            failure => {
                reap(&mut *child).await?;
                failure.unwrap_or(Err(WorkerError::Deadline))
            }
        }
    }

    pub(crate) fn spawn(&self, subcommand: &str) -> Result<Box<dyn ChildWrapper>, WorkerError> {
        Ok(command(&self.0, |command| {
            command
                .arg(subcommand)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            // These local workers never use an HTTP proxy. RTSPS must connect
            // directly to the configured camera peer.
            for key in [
                "http_proxy",
                "HTTP_PROXY",
                "https_proxy",
                "HTTPS_PROXY",
                "all_proxy",
                "ALL_PROXY",
            ] {
                command.env_remove(key);
            }
        })
        .spawn()?)
    }
}

#[derive(Clone)]
pub struct FrameBudget(Arc<Semaphore>);

impl FrameBudget {
    pub fn new(bytes: u32) -> Result<Self, WorkerError> {
        if bytes == 0 || u64::from(bytes) > 2 * 1024 * 1024 * 1024 {
            return Err(WorkerError::Config);
        }
        Ok(Self(Arc::new(Semaphore::new(bytes as usize))))
    }
}

pub struct Frame {
    pub generation: Uuid,
    pub header: FrameHeader,
    pub rgb: Vec<u8>,
    _budget: OwnedSemaphorePermit,
}

pub enum CaptureRead {
    Frame(Frame),
    Dropped(FrameHeader),
    End,
}

pub struct CaptureProcess {
    child: Option<Box<dyn ChildWrapper>>,
    stdout: ChildStdout,
    generation: Uuid,
    last_frame: u64,
    schannel: bool,
}

impl CaptureProcess {
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    pub async fn next(
        &mut self,
        budget: &FrameBudget,
        deadline: Duration,
    ) -> Result<CaptureRead, WorkerError> {
        let deadline = if self.schannel && self.last_frame == 0 {
            deadline.max(WINDOWS_RTSPS_OPEN_TIMEOUT + Duration::from_secs(1))
        } else {
            deadline
        };
        tokio::time::timeout(deadline, self.receive(budget))
            .await
            .map_err(|_| WorkerError::Deadline)?
    }

    async fn receive(&mut self, budget: &FrameBudget) -> Result<CaptureRead, WorkerError> {
        let mut bytes = [0u8; HEADER_LEN];
        if self.stdout.read(&mut bytes[..1]).await? == 0 {
            let child = self.child.as_mut().ok_or(WorkerError::Exit)?;
            let status = child.wait().await?;
            self.child = None;
            return if status.success() {
                Ok(CaptureRead::End)
            } else if status.code() == Some(CAPTURE_SOURCE_EXIT) {
                Err(WorkerError::Source)
            } else {
                Err(WorkerError::Exit)
            };
        }
        self.stdout.read_exact(&mut bytes[1..]).await?;
        let header = FrameHeader::decode(&bytes)?;
        if header.frame != self.last_frame.saturating_add(1) || self.last_frame == u64::MAX {
            return Err(WorkerError::Config);
        }
        self.last_frame = header.frame;
        let length = header.byte_length()?;
        if let Ok(permit) = budget.0.clone().try_acquire_many_owned(length) {
            let mut rgb = vec![0; length as usize];
            self.stdout.read_exact(&mut rgb).await?;
            Ok(CaptureRead::Frame(Frame {
                generation: self.generation,
                header,
                rgb,
                _budget: permit,
            }))
        } else {
            let mut left = length as usize;
            let mut sink = [0u8; 16 * 1024];
            while left > 0 {
                let part = left.min(sink.len());
                self.stdout.read_exact(&mut sink[..part]).await?;
                left -= part;
            }
            Ok(CaptureRead::Dropped(header))
        }
    }

    pub async fn stop(mut self) -> Result<(), WorkerError> {
        if let Some(child) = &mut self.child {
            reap(&mut **child).await?;
            self.child = None;
        }
        Ok(())
    }
}

pub(crate) async fn reap(child: &mut dyn ChildWrapper) -> Result<(), WorkerError> {
    if child.start_kill().is_err() && !child.try_wait().is_ok_and(|status| status.is_some()) {
        return Err(WorkerError::Unreaped);
    }
    match tokio::time::timeout(EXIT_DEADLINE, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        _ => Err(WorkerError::Unreaped),
    }
}

impl Drop for CaptureProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}
