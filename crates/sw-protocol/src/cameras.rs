use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Counter;

pub const REGION_UNITS: u32 = 10_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegionPoint {
    #[cfg_attr(feature = "schema", schemars(range(max = 10000)))]
    pub x: u16,
    #[cfg_attr(feature = "schema", schemars(range(max = 10000)))]
    pub y: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ControlledSource {
    Burner,
    Candle,
    Fireplace,
    Grill,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ControlledMode {
    Steady,
    Intermittent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ControlledFireRegion {
    pub source: ControlledSource,
    pub mode: ControlledMode,
    #[cfg_attr(feature = "schema", schemars(length(min = 3, max = 16)))]
    pub polygon: Vec<RegionPoint>,
}

impl ControlledFireRegion {
    fn valid(&self) -> bool {
        let points = &self.polygon;
        if !(3..=16).contains(&points.len())
            || points
                .iter()
                .any(|p| u32::from(p.x) > REGION_UNITS || u32::from(p.y) > REGION_UNITS)
        {
            return false;
        }
        let cross = |a: RegionPoint, b: RegionPoint, p: RegionPoint| {
            let dx = i64::from(b.x) - i64::from(a.x);
            let dy = i64::from(b.y) - i64::from(a.y);
            dx * (i64::from(p.y) - i64::from(a.y)) - dy * (i64::from(p.x) - i64::from(a.x))
        };
        let orientation = cross(points[0], points[1], points[2]).signum();
        orientation != 0
            && (0..points.len()).all(|i| {
                let j = (i + 1) % points.len();
                points.iter().enumerate().all(|(k, &p)| {
                    k == i || k == j || cross(points[i], points[j], p).signum() == orientation
                })
            })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SceneReview {
    Unconfigured,
    NeedsReview,
    Confirmed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MaskRect {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl MaskRect {
    pub fn valid(self) -> bool {
        self.left < self.right
            && self.top < self.bottom
            && u32::from(self.right) <= REGION_UNITS
            && u32::from(self.bottom) <= REGION_UNITS
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CameraSource {
    Replay {
        file: String,
        pace: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repeat: bool,
    },
    Rtsp {
        peer: SocketAddr,
        stream_path: String,
        secure: bool,
        transport: RtspTransport,
        credential_ref: Option<Uuid>,
    },
    Usb {
        device_id: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RtspTransport {
    Tcp,
    Udp,
}

impl CameraSource {
    pub fn is_live(&self) -> bool {
        !matches!(self, Self::Replay { .. })
    }

    pub fn valid(&self) -> bool {
        match self {
            Self::Replay { file, pace, repeat } => bounded_text(file, 512) && (!*repeat || *pace),
            Self::Rtsp {
                peer,
                stream_path,
                secure,
                transport,
                ..
            } => {
                peer.port() != 0
                    && match peer {
                        SocketAddr::V4(peer) => {
                            let address = peer.ip();
                            !address.is_unspecified()
                                && !address.is_multicast()
                                && !address.is_broadcast()
                                && !address.is_link_local()
                        }
                        SocketAddr::V6(peer) => {
                            let address = peer.ip();
                            !address.is_unspecified()
                                && !address.is_multicast()
                                && !address.is_unicast_link_local()
                                && address.to_ipv4_mapped().is_none()
                                && peer.scope_id() == 0
                        }
                    }
                    && (!*secure || *transport == RtspTransport::Tcp)
                    && valid_rtsp_path(stream_path)
            }
            Self::Usb { device_id } => bounded_text(device_id, 256) && !device_id.contains(':'),
        }
    }
}

fn valid_rtsp_path(resource: &str) -> bool {
    if !resource.starts_with('/') || resource.len() > 512 {
        return false;
    }
    let (path, query) = resource.split_once('?').unwrap_or((resource, ""));
    path.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~'))
        && (path.len() == resource.len() || valid_rtsp_selectors(query))
}

fn valid_rtsp_selectors(query: &str) -> bool {
    let mut keys = std::collections::HashSet::new();
    query.split('&').all(|part| {
        part.split_once('=').is_some_and(|(key, value)| {
            // Only numeric channel selectors belong in public camera descriptors.
            matches!(
                key,
                "channel" | "subtype" | "stream" | "profile" | "trackID"
            ) && !value.is_empty()
                && value.len() <= 5
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && keys.insert(key)
        })
    })
}

fn bounded_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {
    pub name: String,
    pub enabled: bool,
    pub source: CameraSource,
    /// Allows semantic evidence from this camera to be sent to Firebase AI Logic.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cloud_consent: bool,
    #[serde(default)]
    pub privacy_masks: Vec<MaskRect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub controlled_fire_regions: Vec<ControlledFireRegion>,
}

impl CameraConfig {
    pub fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.chars().count() <= 80
            && !self.name.chars().any(char::is_control)
            && self.source.valid()
            && self.privacy_masks.len() <= 32
            && self.privacy_masks.iter().all(|region| region.valid())
            && self.controlled_fire_regions.len() <= 16
            && self
                .controlled_fire_regions
                .iter()
                .all(ControlledFireRegion::valid)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CameraPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub source: Option<CameraSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_consent: Option<bool>,
    pub privacy_masks: Option<Vec<MaskRect>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controlled_fire_regions: Option<Vec<ControlledFireRegion>>,
}

impl CameraPatch {
    pub fn merge(&self, current: CameraConfig) -> CameraConfig {
        CameraConfig {
            name: self.name.clone().unwrap_or(current.name),
            enabled: self.enabled.unwrap_or(current.enabled),
            source: self.source.clone().unwrap_or(current.source),
            cloud_consent: self.cloud_consent.unwrap_or(current.cloud_consent),
            privacy_masks: self.privacy_masks.clone().unwrap_or(current.privacy_masks),
            controlled_fire_regions: self
                .controlled_fire_regions
                .clone()
                .unwrap_or(current.controlled_fire_regions),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Camera {
    pub id: Uuid,
    pub revision: Counter,
    pub scene_revision: Counter,
    pub source_epoch: Counter,
    pub live: bool,
    pub config: CameraConfig,
    pub dimensions: Option<Dimensions>,
    pub scene_review: SceneReview,
    pub runtime: CameraRuntime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Disabled,
    Starting,
    Replaying,
    Streaming,
    ClockHold,
    Completed,
    Retrying,
    Unavailable,
    CapacityExceeded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CameraRuntime {
    pub state: CaptureState,
    pub frame: Option<Counter>,
    pub dropped: Counter,
    pub last_frame_age_ms: Option<Counter>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CameraPage {
    pub cameras: Vec<Camera>,
    pub next: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceProbeRequest {
    pub source: CameraSource,
    #[serde(default)]
    pub privacy_masks: Vec<MaskRect>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceProbe {
    pub dimensions: Dimensions,
    pub live: bool,
    pub native_pts: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReplayUpload {
    pub file: String,
    pub bytes: Counter,
    pub source_probe: SourceProbe,
}

pub const MAX_REPLAY_UPLOAD_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CaptureDevice {
    pub device_id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AnalysisState {
    Disabled,
    Starting,
    Ready,
    Degraded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnalysisStatus {
    pub state: AnalysisState,
    pub source_epoch: Option<Counter>,
    pub pose_frames: Counter,
    pub fire_frames: Counter,
    pub last_analysis_age_ms: Option<Counter>,
    pub last_worker_ms: Option<u32>,
    pub people: Option<u16>,
    pub smoke_regions: Option<u16>,
    pub fire_regions: Option<u16>,
}

impl AnalysisStatus {
    pub fn with_state(state: AnalysisState) -> Self {
        Self {
            state,
            source_epoch: None,
            pose_frames: Counter(0),
            fire_frames: Counter(0),
            last_analysis_age_ms: None,
            last_worker_ms: None,
            people: None,
            smoke_regions: None,
            fire_regions: None,
        }
    }
}
