use serde::{Deserialize, Serialize};

use crate::{CameraSampling, CameraSource, Dimensions, RtspTransport};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    PacedReplay,
    UnpacedReplay,
    RtspTcp,
    RtspUdp,
    Rtsps,
    Usb,
}

impl From<&CameraSource> for SourceKind {
    fn from(source: &CameraSource) -> Self {
        match source {
            CameraSource::Replay { pace: true, .. } => Self::PacedReplay,
            CameraSource::Replay { .. } => Self::UnpacedReplay,
            CameraSource::Rtsp { secure: true, .. } => Self::Rtsps,
            CameraSource::Rtsp {
                transport: RtspTransport::Tcp,
                ..
            } => Self::RtspTcp,
            CameraSource::Rtsp { .. } => Self::RtspUdp,
            CameraSource::Usb { .. } => Self::Usb,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SourceRateQuality {
    #[default]
    Unknown,
    Observed,
    Variable,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceRate {
    pub advertised_millifps: Option<u32>,
    pub observed_millifps: Option<u32>,
    pub quality: SourceRateQuality,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceMeasurement {
    pub dimensions: Dimensions,
    pub codec: Option<String>,
    pub rate: SourceRate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostAccelerator {
    pub name: String,
    pub architecture: String,
    pub driver: String,
    pub memory_mib: u64,
    pub device_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHost {
    pub platform: String,
    pub architecture: String,
    pub os_release: String,
    pub cpu: String,
    pub logical_cores: u32,
    pub core_runtime_threads: usize,
    pub ram_bytes: u64,
    pub accelerator: Option<HostAccelerator>,
}

/// Named profiles admit only sources whose decoder workload matches this class.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceWorkload {
    pub kind: SourceKind,
    pub codec: String,
    pub dimensions: Dimensions,
    pub min_millifps: u32,
    pub max_millifps: u32,
}

impl SourceWorkload {
    pub fn valid(&self) -> bool {
        (1..=crate::MAX_WIDTH).contains(&self.dimensions.width)
            && (1..=crate::MAX_HEIGHT).contains(&self.dimensions.height)
            && !self.codec.is_empty()
            && self.codec.len() <= 32
            && self
                .codec
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            && (1..=240_000).contains(&self.max_millifps)
            && (1..=self.max_millifps).contains(&self.min_millifps)
            && self.kind != SourceKind::UnpacedReplay
    }

    pub fn admits_source(
        &self,
        source: SourceKind,
        source_measurement: &SourceMeasurement,
    ) -> bool {
        let range = self.min_millifps..=self.max_millifps;
        self.kind == source
            && source_measurement.dimensions == self.dimensions
            && source_measurement.codec.as_deref() == Some(self.codec.as_str())
            && source_measurement.rate.quality == SourceRateQuality::Observed
            && source_measurement
                .rate
                .advertised_millifps
                .is_some_and(|fps| range.contains(&fps))
            && source_measurement
                .rate
                .observed_millifps
                .is_some_and(|fps| range.contains(&fps))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AnalysisWorkload {
    pub source: SourceWorkload,
    pub sampling: CameraSampling,
    pub cameras: u32,
    pub min_pose_per_wall_minute: u32,
    pub min_fire_per_wall_minute: u32,
    pub detail_source: Option<SourceWorkload>,
    pub media: crate::MediaWorkloadLimits,
}

impl AnalysisWorkload {
    pub fn valid(&self) -> bool {
        self.source.valid()
            && self
                .detail_source
                .as_ref()
                .is_none_or(SourceWorkload::valid)
            && self.sampling.valid()
            && (1..=64).contains(&self.cameras)
            && (u32::from(self.sampling.pose_fps) * 57..=u32::from(self.sampling.pose_fps) * 60)
                .contains(&self.min_pose_per_wall_minute)
            && (u32::from(self.sampling.fire_fps) * 57..=u32::from(self.sampling.fire_fps) * 60)
                .contains(&self.min_fire_per_wall_minute)
            && self.media.valid()
            && self.media.preview_publishers + self.media.detail_publishers > 0
            && self.media.viewer_sessions > 0
            && self.media.snapshot_encoders > 0
            && self.media.snapshot_requests_per_minute > 0
            && self.detail_source.is_some() == (self.media.detail_decoders > 0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CapacityState {
    Verified,
    Unknown,
    Insufficient,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnalysisCapacity {
    pub state: CapacityState,
    pub profile_id: Option<String>,
    pub profile_sha256: Option<String>,
    pub admissible_workload: Option<AnalysisWorkload>,
    pub available_camera_slots: Option<u32>,
    pub limiting_resource: Option<String>,
    pub reason: String,
}
