use serde::{Deserialize, Serialize};
use sw_protocol::{
    AnalysisCapacity, AnalysisWorkload, CameraConfig, CameraSampling, CapacityState, SourceKind,
    SourceMeasurement, SourceRateQuality,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CameraSourceMeasurements {
    pub analysis: SourceMeasurement,
    pub detail: Option<SourceMeasurement>,
}

/// Fixed for the lifetime of the registry and its storage commands.
pub struct AnalysisAdmission {
    pub id: String,
    pub sha256: String,
    pub workload: AnalysisWorkload,
}

impl AnalysisAdmission {
    pub fn matches_camera(
        &self,
        config: &CameraConfig,
        measurements: Option<&CameraSourceMeasurements>,
    ) -> bool {
        self.workload.sampling == config.sampling
            && measurements.is_some_and(|measured| {
                self.workload
                    .source
                    .admits_source(SourceKind::from(&config.source), &measured.analysis)
            })
    }

    pub fn camera_capacity(
        &self,
        config: &CameraConfig,
        measurements: Option<&CameraSourceMeasurements>,
        available: u32,
    ) -> AnalysisCapacity {
        self.source_capacity(&config.source, config.sampling, measurements, available)
    }

    pub fn source_capacity(
        &self,
        source: &sw_protocol::CameraSource,
        sampling: CameraSampling,
        measurements: Option<&CameraSourceMeasurements>,
        available: u32,
    ) -> AnalysisCapacity {
        let profile = &self.workload;
        let kind = SourceKind::from(source);
        let (state, resource, reason) = if sampling != profile.sampling {
            (
                CapacityState::Insufficient,
                Some("sampling"),
                "Requested sampling differs from the verified workload",
            )
        } else if kind != profile.source.kind {
            (
                CapacityState::Insufficient,
                Some("decoder"),
                "Source transport differs from the verified workload",
            )
        } else if available == 0 {
            (
                CapacityState::Insufficient,
                Some("compute_slots"),
                "The verified camera slots are already reserved",
            )
        } else if let Some(measured) = measurements
            .filter(|measured| measured.analysis.rate.quality == SourceRateQuality::Observed)
        {
            if profile.source.admits_source(kind, &measured.analysis) {
                (
                    CapacityState::Verified,
                    None,
                    "Source fits the verified workload",
                )
            } else {
                (
                    CapacityState::Insufficient,
                    Some("decoder"),
                    "Source codec, dimensions or rate differs from the verified workload",
                )
            }
        } else {
            (
                CapacityState::Unknown,
                Some("source_rate"),
                "An observed source rate is required for the verified workload",
            )
        };
        self.status(state, available, resource, reason)
    }

    pub fn status(
        &self,
        state: CapacityState,
        available: u32,
        resource: Option<&str>,
        reason: &str,
    ) -> AnalysisCapacity {
        AnalysisCapacity {
            state,
            profile_id: Some(self.id.clone()),
            profile_sha256: Some(self.sha256.clone()),
            admissible_workload: Some(self.workload.clone()),
            available_camera_slots: Some(available),
            limiting_resource: resource.map(str::to_owned),
            reason: reason.to_owned(),
        }
    }
}
