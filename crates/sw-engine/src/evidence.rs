use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use jpeg_encoder::{ColorType, Encoder};
use sw_domain::{Box2d, SourceScope};
use sw_protocol::{EvidenceCoverage, EvidenceImageKind};
use sw_storage::EvidenceImage;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use uuid::Uuid;

use crate::{CameraFrame, CameraRegistry, MICROS_PER_SECOND};

const HISTORY_US: i64 = 8_000_000;
const FRAME_MAX_AGE: Duration = Duration::from_secs(3);
const MAX_SIDE: usize = 720;
const MAX_IMAGE_BYTES: usize = 384 * 1024;
const PER_CAMERA_BYTES: usize = 512 * 1024;
const TOTAL_BYTES: usize = 32 * 1024 * 1024;

pub(crate) struct EvidenceSampler {
    cameras: Arc<CameraRegistry>,
    rings: Mutex<HashMap<Uuid, Ring>>,
    budget: Arc<Semaphore>,
}

struct Ring {
    scope: SourceScope,
    samples: VecDeque<Sample>,
    bytes: usize,
}

struct Sample {
    at: i64,
    jpeg: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

pub(crate) struct EvidenceSelection {
    pub images: Vec<EvidenceImage>,
    pub coverage: EvidenceCoverage,
    pub has_pre_event: bool,
}

impl EvidenceSampler {
    pub fn new(cameras: Arc<CameraRegistry>) -> Self {
        Self {
            cameras,
            rings: Mutex::new(HashMap::new()),
            budget: Arc::new(Semaphore::new(TOTAL_BYTES)),
        }
    }

    pub async fn run(
        self: Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), crate::WorkerError> {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => return Ok(()),
                _ = tick.tick() => {},
            }
            let sampler = self.clone();
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio::task::spawn_blocking(move || sampler.sample()),
            )
            .await
            .map_err(|_| crate::WorkerError::Deadline)?
            .map_err(|_| crate::WorkerError::Exit)?;
        }
    }

    fn sample(&self) {
        let frames = self.cameras.frames();
        if let Ok(mut rings) = self.rings.lock() {
            rings.retain(|id, ring| {
                frames.iter().any(|frame| {
                    frame.scope.camera_id == *id
                        && frame.scope == ring.scope
                        && frame.captured_at.elapsed() <= FRAME_MAX_AGE
                })
            });
        }
        for frame in frames {
            if !frame.usable_for_analysis
                || frame.captured_at.elapsed() > FRAME_MAX_AGE
                || frame.visibility == sw_domain::Visibility::Poor
            {
                continue;
            }
            let Some(at) = frame.frame.header.source_micros else {
                continue;
            };
            let interval = MICROS_PER_SECOND / i64::from(frame.sampling.evidence_fps);
            if self
                .rings
                .lock()
                .ok()
                .and_then(|rings| {
                    rings
                        .get(&frame.scope.camera_id)
                        .and_then(|ring| ring.samples.back().map(|sample| sample.at))
                })
                .is_some_and(|last| at.div_euclid(interval) <= last.div_euclid(interval))
            {
                continue;
            }
            let Some(jpeg) = encode(&frame, None) else {
                continue;
            };
            let Ok(mut rings) = self.rings.lock() else {
                return;
            };
            let ring = rings.entry(frame.scope.camera_id).or_insert_with(|| Ring {
                scope: frame.scope,
                samples: VecDeque::new(),
                bytes: 0,
            });
            if frame.scope != ring.scope {
                continue;
            }
            let bytes = jpeg.len();
            while ring.samples.front().is_some_and(|old| {
                at.saturating_sub(old.at) > HISTORY_US || ring.bytes + bytes > PER_CAMERA_BYTES
            }) {
                ring.bytes -= ring
                    .samples
                    .pop_front()
                    .expect("old evidence sample")
                    .jpeg
                    .len();
            }
            if let Ok(permit) = self.budget.clone().try_acquire_many_owned(bytes as u32) {
                ring.bytes += bytes;
                ring.samples.push_back(Sample {
                    at,
                    jpeg,
                    _permit: permit,
                });
            }
        }
    }

    pub fn select(&self, frame: &CameraFrame, onset: i64, region: Box2d) -> EvidenceSelection {
        let at = frame.frame.header.source_micros.expect("analyzed frame");
        let start = onset
            .saturating_sub(1_500_000)
            .max(at.saturating_sub(HISTORY_US));
        let mut images = Vec::new();
        let mut sampled = Vec::new();
        if let Ok(rings) = self.rings.lock()
            && let Some(ring) = rings
                .get(&frame.scope.camera_id)
                .filter(|ring| ring.scope == frame.scope)
        {
            let candidates: Vec<_> = ring
                .samples
                .iter()
                .filter(|sample| sample.at >= start && sample.at < at)
                .collect();
            sampled.extend(candidates.iter().map(|sample| sample.at));
            let picks = [
                candidates.iter().copied().find(|p| p.at < onset),
                candidates
                    .iter()
                    .copied()
                    .min_by_key(|p| p.at.abs_diff(onset)),
                candidates.last().copied(),
            ];
            for sample in picks.into_iter().flatten() {
                if images
                    .iter()
                    .all(|old: &EvidenceImage| old.source_us != sample.at)
                {
                    images.push(EvidenceImage {
                        source_us: sample.at,
                        kind: EvidenceImageKind::Overview,
                        jpeg: sample.jpeg.clone(),
                    });
                }
            }
        }
        if let Some(jpeg) = encode(frame, None) {
            sampled.push(at);
            images.push(EvidenceImage {
                source_us: at,
                kind: EvidenceImageKind::Overview,
                jpeg,
            });
            if let Some(jpeg) = encode(frame, Some(region)) {
                images.push(EvidenceImage {
                    source_us: at,
                    kind: EvidenceImageKind::Crop,
                    jpeg,
                });
            }
        } else {
            // An evidence packet always includes a current full-frame observation.
            images.clear();
        }
        images.sort_by_key(|image| image.source_us);
        let has_pre_event = images.iter().any(|image| image.source_us < onset);
        let max_gap = sampled
            .iter()
            .copied()
            .chain([at])
            .scan(start, |last, time| {
                let gap = time.saturating_sub(*last);
                *last = time;
                Some(gap)
            })
            .max()
            .unwrap_or(0);
        let received = sampled.len().min(u16::MAX as usize) as u16;
        let interval = MICROS_PER_SECOND / i64::from(frame.sampling.evidence_fps);
        let expected =
            (at.saturating_sub(start) / interval + 1).clamp(1, i64::from(u16::MAX)) as u16;
        EvidenceSelection {
            images,
            has_pre_event,
            coverage: EvidenceCoverage {
                expected_samples: expected.max(received),
                received_samples: received,
                max_gap_ms: (max_gap / 1000).min(i64::from(u32::MAX)) as u32,
            },
        }
    }
}

pub(crate) fn encode(frame: &CameraFrame, region: Option<Box2d>) -> Option<Vec<u8>> {
    let width = frame.frame.header.width as usize;
    let height = frame.frame.header.height as usize;
    let (x, y, w, h) = if let Some(region) = region {
        let dx = (region.width() * 0.20).max(8.0);
        let dy = (region.height() * 0.20).max(8.0);
        let left = (region.x1 - dx).floor().max(0.0) as usize;
        let top = (region.y1 - dy).floor().max(0.0) as usize;
        let right = (region.x2 + dx).ceil().min(width as f64) as usize;
        let bottom = (region.y2 + dy).ceil().min(height as f64) as usize;
        (
            left,
            top,
            right.checked_sub(left)?,
            bottom.checked_sub(top)?,
        )
    } else {
        (0, 0, width, height)
    };
    if w == 0 || h == 0 {
        return None;
    }
    let divisor = w.max(h).div_ceil(MAX_SIDE);
    let out_w = w.div_ceil(divisor);
    let out_h = h.div_ceil(divisor);
    let mut pixels = Vec::with_capacity(out_w * out_h * 3);
    let rgb = &frame.frame.rgb;
    for i in (0..h).step_by(divisor) {
        for j in (0..w).step_by(divisor) {
            let sample_y = y + (i + divisor / 2).min(h - 1);
            let sample_x = x + (j + divisor / 2).min(w - 1);
            let index = (sample_y * width + sample_x) * 3;
            pixels.extend_from_slice(&rgb[index..index + 3]);
        }
    }
    let mut jpeg = Vec::with_capacity(32 * 1024);
    Encoder::new(&mut jpeg, 82)
        .encode(&pixels, out_w as u16, out_h as u16, ColorType::Rgb)
        .ok()?;
    (jpeg.len() <= MAX_IMAGE_BYTES).then_some(jpeg)
}
