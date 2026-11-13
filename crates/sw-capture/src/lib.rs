#![deny(unsafe_code)]

mod native;

use std::io;

use thiserror::Error;

pub use native::{Decoder, ReadLimits};
pub use native::{confined_rtsp_available, devices};

pub struct CaptureDevice {
    pub device_id: String,
    pub name: String,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("source file cannot be opened")]
    Io(#[from] io::Error),
    #[error("source is not an ordinary file with a valid UTF-8 path")]
    InvalidFile,
    #[error("capture device is unavailable")]
    InvalidDevice,
    #[cfg(not(any(windows, target_os = "linux")))]
    #[error("USB capture is unsupported on this operating system")]
    UnsupportedDevice,
    #[error("network capture source is invalid")]
    InvalidNetwork,
    #[error("the installed decoder cannot enforce RTSP peer confinement")]
    NetworkPolicyUnavailable,
    #[error("capture limits are invalid")]
    InvalidLimits,
    #[error("source did not return a frame within its time budget")]
    Deadline,
    #[error("capture was cancelled")]
    Cancelled,
    #[error("source has no decodable video stream")]
    NoVideo,
    #[error("source dimensions exceed capture limits")]
    Dimensions,
    #[error("native video operation failed: {0}")]
    Native(#[from] ffmpeg_next::Error),
    #[error("native frame allocation failed")]
    Allocation,
    #[error("native decoder or scaler returned inconsistent frame data")]
    BadFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtsQuality {
    Native,
    Estimated,
    Missing,
}

#[derive(Clone, Copy, Debug)]
pub struct SourceTimestamp {
    pub raw_pts: Option<i64>,
    pub time_base_num: i32,
    pub time_base_den: i32,
    pub micros: Option<i64>,
    pub quality: PtsQuality,
}

impl SourceTimestamp {
    fn from_pts(raw: Option<i64>, estimated: Option<i64>, num: i32, den: i32) -> Self {
        let micros = raw
            .or(estimated)
            .filter(|_| num > 0 && den > 0)
            .and_then(|pts| {
                i64::try_from(i128::from(pts) * i128::from(num) * 1_000_000 / i128::from(den)).ok()
            });
        let quality = match (micros, raw) {
            (None, _) => PtsQuality::Missing,
            (Some(_), None) => PtsQuality::Estimated,
            (Some(_), Some(_)) => PtsQuality::Native,
        };
        Self {
            raw_pts: raw,
            time_base_num: num,
            time_base_den: den,
            micros,
            quality,
        }
    }
}

pub struct RgbFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp: SourceTimestamp,
    pub corrupt: bool,
    pub rgb: Vec<u8>,
}

/// One authorized source within a capture worker. `None` denotes end of stream.
/// Implementations return owned RGB frames with source timestamps and honor the
/// decoder's read limits. The supervisor owns cancellation at the process boundary.
pub trait SourceAdapter {
    type Error: std::error::Error + Send + Sync + 'static;

    fn read_rgb(&mut self) -> Result<Option<RgbFrame>, Self::Error>;
}

impl SourceAdapter for Decoder {
    type Error = CaptureError;

    fn read_rgb(&mut self) -> Result<Option<RgbFrame>, CaptureError> {
        self.next_frame()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_signed_pts_and_distinguishes_missing_estimated_and_overflow() {
        let pts = SourceTimestamp::from_pts(Some(-900), Some(0), 1, 90_000);
        assert_eq!(pts.micros, Some(-10_000));
        assert_eq!(pts.quality, PtsQuality::Native);
        let estimated = SourceTimestamp::from_pts(None, Some(1_001), 1, 30_000);
        assert_eq!(estimated.micros, Some(33_366));
        assert_eq!(estimated.quality, PtsQuality::Estimated);
        assert_eq!(
            SourceTimestamp::from_pts(Some(i64::MAX), None, i32::MAX, 1).quality,
            PtsQuality::Missing
        );
        assert_eq!(SourceTimestamp::from_pts(Some(0), None, 1, 0).micros, None);
    }
}
