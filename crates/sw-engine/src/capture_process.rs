use std::{
    io, mem,
    ops::Deref,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use process_wrap::tokio::ChildWrapper;
use sw_protocol::CaptureDevice;
use sw_protocol::worker::{
    CAPTURE_SOURCE_EXIT, CODEC_HEADER_LEN, CONFIG_MAX_BYTES, CaptureRequest, FrameHeader,
    HEADER_LEN, MAX_SOURCE_MILLIFPS, WINDOWS_RTSPS_OPEN_TIMEOUT, decode_capture_codec,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::ChildStdout,
    sync::{OnceCell, OwnedSemaphorePermit, Semaphore},
};
use uuid::Uuid;

use crate::child_process::command;

const CONFIG_DEADLINE: Duration = Duration::from_secs(2);
const EXIT_DEADLINE: Duration = Duration::from_secs(3);
const MAX_IDLE_FRAME_BUFFERS: usize = 16;

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
    #[error("incident writes did not finish before shutdown")]
    IncidentDrain,
    #[error("inference worker could not initialize its model or runtime")]
    InferenceStartup(sw_protocol::InferenceFailure),
}

#[derive(Default, serde::Deserialize)]
struct CaptureCapabilities {
    rtsp_peer_confined: bool,
    #[serde(default)]
    capture_codec: bool,
    #[serde(default)]
    capture_source_metadata: bool,
    #[serde(default)]
    inference_timing_v2: bool,
    #[serde(default)]
    fire_proposals: bool,
    #[serde(default)]
    decoder_identity: Option<String>,
}

#[derive(Clone)]
pub struct WorkerBinary {
    path: PathBuf,
    capture_capabilities: Arc<OnceCell<CaptureCapabilities>>,
}

impl WorkerBinary {
    pub fn open(path: &Path) -> Result<Self, WorkerError> {
        let path = path.canonicalize().map_err(|_| WorkerError::Unavailable)?;
        if !path.is_file() {
            return Err(WorkerError::Unavailable);
        }
        Ok(Self {
            path,
            capture_capabilities: Arc::default(),
        })
    }

    pub async fn capture(&self, config: &CaptureRequest) -> Result<CaptureProcess, WorkerError> {
        let input = serde_json::to_vec(config).map_err(|_| WorkerError::Config)?;
        if input.len() > CONFIG_MAX_BYTES {
            return Err(WorkerError::Config);
        }
        let (with_codec, with_rate) = match self.capture_capabilities().await {
            Ok(caps) => (
                caps.capture_codec || caps.capture_source_metadata,
                caps.capture_source_metadata,
            ),
            Err(WorkerError::Unreaped) => return Err(WorkerError::Unreaped),
            // Older local adapters can use the original frame format.
            Err(_) => (false, false),
        };
        let mut child = self.spawn(if with_rate {
            "capture-with-source-metadata"
        } else if with_codec {
            "capture-with-codec"
        } else {
            "capture"
        })?;
        let stdout = child.stdout().take().ok_or(WorkerError::Unavailable)?;
        let mut stdin = child.stdin().take().ok_or(WorkerError::Unavailable)?;
        let process = CaptureProcess {
            child: Some(child),
            stdout,
            generation: Uuid::new_v4(),
            last_frame: 0,
            last_frame_length: 0,
            schannel: config.source.uses_schannel(),
            with_codec,
            with_rate,
            codec: None,
            advertised_millifps: None,
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
        Ok(self.capture_capabilities().await?.rtsp_peer_confined)
    }

    pub async fn decoder_identity(&self) -> Result<Option<&str>, WorkerError> {
        Ok(self
            .capture_capabilities()
            .await?
            .decoder_identity
            .as_deref())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn inference_capabilities(&self) -> Result<(bool, bool), WorkerError> {
        match self.capture_capabilities().await {
            Ok(caps) => Ok((caps.inference_timing_v2, caps.fire_proposals)),
            Err(WorkerError::Unreaped) => Err(WorkerError::Unreaped),
            // A legacy local adapter may omit capability metadata.
            Err(_) => Ok((false, false)),
        }
    }

    async fn capture_capabilities(&self) -> Result<&CaptureCapabilities, WorkerError> {
        self.capture_capabilities
            .get_or_try_init(|| async {
                match self.capture_metadata("capture-doctor", 1024).await {
                    // A legacy worker may omit this subcommand. RTSP stays disabled.
                    Err(WorkerError::Exit) => Ok(CaptureCapabilities::default()),
                    result => result,
                }
            })
            .await
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
        Ok(command(&self.path, |command| {
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
pub struct FrameBudget(Arc<FrameBudgetInner>);

struct FrameBudgetInner {
    bytes: Arc<Semaphore>,
    pool: Mutex<FramePool>,
}

#[derive(Default)]
struct FramePool {
    in_use: usize,
    idle: Vec<FrameAllocation>,
}

struct FrameAllocation {
    rgb: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl FrameBudget {
    pub fn new(bytes: u32) -> Result<Self, WorkerError> {
        if bytes == 0 || u64::from(bytes) > 2 * 1024 * 1024 * 1024 {
            return Err(WorkerError::Config);
        }
        Ok(Self(Arc::new(FrameBudgetInner {
            bytes: Arc::new(Semaphore::new(bytes as usize)),
            pool: Mutex::new(FramePool::default()),
        })))
    }

    pub(crate) fn try_reserve(&self, bytes: u32) -> Option<OwnedSemaphorePermit> {
        self.0.bytes.clone().try_acquire_many_owned(bytes).ok()
    }

    pub(crate) fn available_bytes(&self) -> u32 {
        self.0.bytes.available_permits() as u32
    }

    fn buffer(self, bytes: u32, resized: bool) -> Option<FrameBuffer> {
        let length = bytes as usize;
        let mut pool = self.0.pool.lock().unwrap_or_else(PoisonError::into_inner);
        if resized {
            pool.idle.retain(|buffer| buffer.rgb.len() == length);
        }
        let mut allocation =
            if let Some(index) = pool.idle.iter().rposition(|item| item.rgb.len() == length) {
                pool.idle.swap_remove(index)
            } else {
                let permit = self.try_reserve(bytes).or_else(|| {
                    pool.idle.clear();
                    self.try_reserve(bytes)
                })?;
                FrameAllocation {
                    rgb: Vec::new(),
                    _permit: permit,
                }
            };
        pool.in_use += 1;
        drop(pool);
        if allocation.rgb.len() != length {
            allocation.rgb = vec![0; length];
        }
        Some(FrameBuffer {
            budget: self,
            allocation: Some(allocation),
        })
    }
}

pub struct FrameBuffer {
    budget: FrameBudget,
    allocation: Option<FrameAllocation>,
}

impl FrameBuffer {
    async fn read(mut self, input: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        let buffer = self.allocation.as_mut().expect("live frame allocation");
        input.read_exact(&mut buffer.rgb).await?;
        Ok(self)
    }
}

impl Deref for FrameBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.allocation.as_ref().expect("live frame allocation").rgb
    }
}

impl Drop for FrameBuffer {
    fn drop(&mut self) {
        let allocation = self.allocation.take().expect("live frame allocation");
        let mut pool = self
            .budget
            .0
            .pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pool.in_use -= 1;
        if pool.in_use == 0 {
            // Camera drain releases idle allocations and their byte permits.
            pool.idle.clear();
        } else if pool.idle.len() < MAX_IDLE_FRAME_BUFFERS {
            pool.idle.push(allocation);
        }
    }
}

pub struct Frame {
    pub generation: Uuid,
    pub header: FrameHeader,
    pub rgb: FrameBuffer,
}

#[cfg(test)]
impl Frame {
    pub(crate) fn test_frame(header: FrameHeader) -> Self {
        let bytes = header.byte_length().unwrap();
        let budget = FrameBudget::new(bytes).unwrap();
        Self {
            generation: Uuid::nil(),
            header,
            rgb: budget.buffer(bytes, false).unwrap(),
        }
    }
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
    last_frame_length: u32,
    schannel: bool,
    with_codec: bool,
    with_rate: bool,
    codec: Option<Arc<str>>,
    advertised_millifps: Option<u32>,
}

impl CaptureProcess {
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    pub fn codec(&self) -> Option<&Arc<str>> {
        self.codec.as_ref()
    }

    pub fn advertised_millifps(&self) -> Option<u32> {
        self.advertised_millifps
    }

    pub async fn next(
        &mut self,
        budget: &FrameBudget,
        deadline: Duration,
    ) -> Result<CaptureRead, WorkerError> {
        self.next_with(deadline, |_| Some(budget.clone())).await
    }

    pub(crate) async fn next_with(
        &mut self,
        deadline: Duration,
        select_budget: impl FnOnce(u32) -> Option<FrameBudget>,
    ) -> Result<CaptureRead, WorkerError> {
        let deadline = if self.schannel && self.last_frame == 0 {
            deadline.max(WINDOWS_RTSPS_OPEN_TIMEOUT + Duration::from_secs(1))
        } else {
            deadline
        };
        tokio::time::timeout(deadline, self.receive(select_budget))
            .await
            .map_err(|_| WorkerError::Deadline)?
    }

    async fn receive(
        &mut self,
        select_budget: impl FnOnce(u32) -> Option<FrameBudget>,
    ) -> Result<CaptureRead, WorkerError> {
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
        if self.with_codec && self.last_frame == 0 {
            let mut codec_header = [0; CODEC_HEADER_LEN];
            codec_header[0] = bytes[0];
            self.stdout.read_exact(&mut codec_header[1..]).await?;
            self.codec = Some(Arc::from(decode_capture_codec(&codec_header)?));
            if self.with_rate {
                let rate = self.stdout.read_u32_le().await?;
                if rate > MAX_SOURCE_MILLIFPS {
                    return Err(WorkerError::Config);
                }
                self.advertised_millifps = (rate != 0).then_some(rate);
            }
            self.stdout.read_exact(&mut bytes).await?;
        } else {
            self.stdout.read_exact(&mut bytes[1..]).await?;
        }
        let header = FrameHeader::decode(&bytes)?;
        if header.frame != self.last_frame.saturating_add(1) || self.last_frame == u64::MAX {
            return Err(WorkerError::Config);
        }
        self.last_frame = header.frame;
        let length = header.byte_length()?;
        let previous = mem::replace(&mut self.last_frame_length, length);
        let resized = previous != 0 && previous != length;
        if let Some(buffer) =
            select_budget(length).and_then(|budget| budget.buffer(length, resized))
        {
            Ok(CaptureRead::Frame(Frame {
                generation: self.generation,
                header,
                rgb: buffer.read(&mut self.stdout).await?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_buffer_is_reused_after_the_last_reader_and_stays_charged() {
        let budget = FrameBudget::new(48).unwrap();
        let buffer = Arc::new(budget.clone().buffer(24, false).unwrap());
        let address = buffer.as_ptr();
        let reader = buffer.clone();
        let current = budget.clone().buffer(24, false).unwrap();
        drop(buffer);
        assert!(budget.clone().buffer(24, false).is_none());

        drop(reader);
        assert_eq!(budget.available_bytes(), 0);
        let next = budget.clone().buffer(24, false).unwrap();
        assert_eq!(next.as_ptr(), address);
        drop(next);
        drop(current);
        assert_eq!(budget.available_bytes(), 48);
    }

    #[test]
    fn resize_and_budget_pressure_evict_idle_frames() {
        let budget = FrameBudget::new(24).unwrap();
        let held = budget.clone().buffer(6, false).unwrap();
        drop(budget.clone().buffer(6, false).unwrap());
        assert_eq!(budget.available_bytes(), 12);

        let resized = budget.clone().buffer(12, true).unwrap();
        assert_eq!(budget.available_bytes(), 6);
        drop(resized);
        assert_eq!(budget.available_bytes(), 6);

        let other_camera = budget.clone().buffer(18, false).unwrap();
        assert_eq!(budget.available_bytes(), 0);
        drop(held);
        drop(other_camera);
        assert_eq!(budget.available_bytes(), 24);
    }

    #[tokio::test]
    async fn incomplete_rgb_reads_recycle_the_buffer_without_publishing_pixels() {
        let budget = FrameBudget::new(12).unwrap();
        let held = budget.clone().buffer(6, false).unwrap();
        let previous = budget
            .clone()
            .buffer(6, false)
            .unwrap()
            .read(&mut &b"abcdef"[..])
            .await
            .unwrap();
        let address = previous.as_ptr();
        drop(previous);

        let failed = budget
            .clone()
            .buffer(6, false)
            .unwrap()
            .read(&mut &b"gh"[..])
            .await;
        assert_eq!(failed.err().unwrap().kind(), io::ErrorKind::UnexpectedEof);

        let (mut writer, mut reader) = tokio::io::duplex(2);
        tokio::io::AsyncWriteExt::write_all(&mut writer, b"op")
            .await
            .unwrap();
        tokio::select! {
            biased;
            _ = budget.clone().buffer(6, false).unwrap().read(&mut reader) => {
                panic!("partial pipe read completed");
            }
            _ = std::future::ready(()) => {}
        }

        let next = budget
            .clone()
            .buffer(6, false)
            .unwrap()
            .read(&mut &b"ijklmn"[..])
            .await
            .unwrap();
        assert_eq!(next.as_ptr(), address);
        assert_eq!(&*next, b"ijklmn");
        drop(held);
        drop(next);
        assert_eq!(budget.available_bytes(), 12);
    }
}
