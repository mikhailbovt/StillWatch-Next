//! Versioned capture and inference IPC for trusted supervised workers.

use std::{io, net::SocketAddr, path::PathBuf, time::Duration};

use crate::{InferenceFailure, RtspTransport};
pub use crate::{MAX_HEIGHT, MAX_WIDTH, MaskRect, REGION_UNITS as MASK_UNITS};
use serde::{Deserialize, Serialize};

pub const CONFIG_MAX_BYTES: usize = 16 * 1024;
pub const MODEL_SELECTION_MAX_BYTES: usize = 12 * 1024;
/// A valid capture request whose configured source cannot be opened or decoded.
pub const CAPTURE_SOURCE_EXIT: i32 = 4;
/// SChannel can take several seconds to validate a cold certificate chain.
pub const WINDOWS_RTSPS_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
pub const RESULT_MAX_BYTES: usize = 1_500_000;
pub const INFERENCE_READY: [u8; 4] = *b"SWI1";
pub const MODEL_PACK_UNAVAILABLE: [u8; 4] = *b"SWIM";
pub const MODEL_INITIALIZATION_FAILED: [u8; 4] = *b"SWIS";
pub const RUNTIME_UNAVAILABLE: [u8; 4] = *b"SWIR";
pub const HEADER_LEN: usize = 56;
pub const CODEC_HEADER_LEN: usize = 36;
pub const MAX_FRAME_BYTES: u32 = MAX_WIDTH * MAX_HEIGHT * 3;
const MISSING_PTS: i64 = i64::MIN;

pub fn encode_capture_codec(codec: &str) -> io::Result<[u8; CODEC_HEADER_LEN]> {
    let mut bytes = [0; CODEC_HEADER_LEN];
    if codec.len() > CODEC_HEADER_LEN - 4 || !valid_capture_codec(codec.as_bytes()) {
        return Err(invalid_codec());
    }
    bytes[..4].copy_from_slice(b"SWC1");
    bytes[4..4 + codec.len()].copy_from_slice(codec.as_bytes());
    Ok(bytes)
}

pub fn decode_capture_codec(bytes: &[u8; CODEC_HEADER_LEN]) -> io::Result<&str> {
    let (marker, name) = bytes.split_at(4);
    let length = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len());
    let (codec, padding) = name.split_at(length);
    if marker != b"SWC1" || !valid_capture_codec(codec) || padding.iter().any(|&byte| byte != 0) {
        return Err(invalid_codec());
    }
    std::str::from_utf8(codec).map_err(|_| invalid_codec())
}

fn valid_capture_codec(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn invalid_codec() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid capture codec header")
}

pub fn inference_startup_failure(marker: [u8; 4]) -> Option<InferenceFailure> {
    match marker {
        MODEL_PACK_UNAVAILABLE => Some(InferenceFailure::ModelPackUnavailable),
        MODEL_INITIALIZATION_FAILED => Some(InferenceFailure::ModelInitializationFailed),
        RUNTIME_UNAVAILABLE => Some(InferenceFailure::RuntimeUnavailable),
        _ => None,
    }
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub source: CaptureSource,
    pub masks: Vec<MaskRect>,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureSource {
    Replay {
        file: PathBuf,
        pace: bool,
    },
    Rtsp {
        peer: SocketAddr,
        stream_path: String,
        secure: bool,
        transport: RtspTransport,
        credentials: Option<CameraCredentials>,
    },
    Usb {
        device_id: String,
    },
}

impl CaptureSource {
    pub fn uses_schannel(&self) -> bool {
        cfg!(windows) && matches!(self, Self::Rtsp { secure: true, .. })
    }
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CameraCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InferenceConfig {
    pub library: PathBuf,
    pub library_sha256: String,
    pub models: PathBuf,
    pub manifest_sha256: String,
    pub threads: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda: Option<CudaConfig>,
}

/// Administrator-owned selection written after validating and warming the pack.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub library: PathBuf,
    pub library_sha256: String,
    pub models: PathBuf,
    pub manifest_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda: Option<CudaConfig>,
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CudaConfig {
    pub device: u16,
    pub provider_sha256: String,
    pub shared_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tasks {
    Pose,
    Fire,
    Both,
}

impl Tasks {
    pub fn pose(self) -> bool {
        matches!(self, Self::Pose | Self::Both)
    }
    pub fn fire(self) -> bool {
        matches!(self, Self::Fire | Self::Both)
    }

    pub fn byte(self) -> u8 {
        match self {
            Self::Pose => 1,
            Self::Fire => 2,
            Self::Both => 3,
        }
    }

    pub fn from_byte(byte: u8) -> io::Result<Self> {
        match byte {
            1 => Ok(Self::Pose),
            2 => Ok(Self::Fire),
            3 => Ok(Self::Both),
            _ => Err(invalid()),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InferenceResult {
    pub request: uuid::Uuid,
    pub elapsed_ms: u32,
    pub pose: Option<Vec<sw_domain::Observation>>,
    pub fire: Option<Vec<sw_domain::Observation>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtsQuality {
    Native,
    Estimated,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    pub width: u32,
    pub height: u32,
    pub frame: u64,
    pub raw_pts: Option<i64>,
    pub source_micros: Option<i64>,
    pub time_base_num: i32,
    pub time_base_den: i32,
    pub pts_quality: PtsQuality,
    pub corrupt: bool,
}

impl FrameHeader {
    pub fn byte_length(self) -> io::Result<u32> {
        if !(1..=MAX_WIDTH).contains(&self.width)
            || !(1..=MAX_HEIGHT).contains(&self.height)
            || self.frame == 0
        {
            return Err(invalid());
        }
        Ok(self.width * self.height * 3)
    }

    pub fn encode(self) -> io::Result<[u8; HEADER_LEN]> {
        let mut bytes = [0; HEADER_LEN];
        bytes[..4].copy_from_slice(b"SWF1");
        bytes[4] = match self.pts_quality {
            PtsQuality::Missing => 0,
            PtsQuality::Native => 1,
            PtsQuality::Estimated => 2,
        };
        bytes[5] = u8::from(self.corrupt);
        bytes[8..12].copy_from_slice(&self.width.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.height.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.byte_length()?.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.frame.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.raw_pts.unwrap_or(MISSING_PTS).to_le_bytes());
        bytes[40..48].copy_from_slice(&self.source_micros.unwrap_or(MISSING_PTS).to_le_bytes());
        bytes[48..52].copy_from_slice(&self.time_base_num.to_le_bytes());
        bytes[52..56].copy_from_slice(&self.time_base_den.to_le_bytes());
        Self::decode(&bytes)?;
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8; HEADER_LEN]) -> io::Result<Self> {
        if &bytes[..4] != b"SWF1"
            || bytes[5] > 1
            || bytes[6..8] != [0; 2]
            || bytes[20..24] != [0; 4]
        {
            return Err(invalid());
        }
        let field = |start: usize| -> [u8; 4] {
            [
                bytes[start],
                bytes[start + 1],
                bytes[start + 2],
                bytes[start + 3],
            ]
        };
        let wide = |start: usize| -> [u8; 8] {
            [
                bytes[start],
                bytes[start + 1],
                bytes[start + 2],
                bytes[start + 3],
                bytes[start + 4],
                bytes[start + 5],
                bytes[start + 6],
                bytes[start + 7],
            ]
        };
        let raw_pts = i64::from_le_bytes(wide(32));
        let source_micros = i64::from_le_bytes(wide(40));
        let header = Self {
            width: u32::from_le_bytes(field(8)),
            height: u32::from_le_bytes(field(12)),
            frame: u64::from_le_bytes(wide(24)),
            raw_pts: (raw_pts != MISSING_PTS).then_some(raw_pts),
            source_micros: (source_micros != MISSING_PTS).then_some(source_micros),
            time_base_num: i32::from_le_bytes(field(48)),
            time_base_den: i32::from_le_bytes(field(52)),
            pts_quality: match bytes[4] {
                0 => PtsQuality::Missing,
                1 => PtsQuality::Native,
                2 => PtsQuality::Estimated,
                _ => return Err(invalid()),
            },
            corrupt: bytes[5] != 0,
        };
        let timestamp_ok = match header.pts_quality {
            PtsQuality::Missing => header.source_micros.is_none(),
            PtsQuality::Estimated => {
                header.source_micros.is_some()
                    && header.raw_pts.is_none()
                    && header.time_base_num > 0
                    && header.time_base_den > 0
            }
            PtsQuality::Native => {
                header.time_base_num > 0
                    && header.time_base_den > 0
                    && header.raw_pts.is_some_and(|pts| {
                        let micros = i128::from(pts) * i128::from(header.time_base_num) * 1_000_000
                            / i128::from(header.time_base_den);
                        i64::try_from(micros).ok() == header.source_micros
                            && header.source_micros.is_some()
                    })
            }
        };
        if !timestamp_ok || header.byte_length()? != u32::from_le_bytes(field(16)) {
            return Err(invalid());
        }
        Ok(header)
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid capture frame header")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_capture_codec_and_padding() {
        let mut bytes = encode_capture_codec("h264").unwrap();
        assert_eq!(decode_capture_codec(&bytes).unwrap(), "h264");
        bytes[9] = b'x';
        assert!(decode_capture_codec(&bytes).is_err());
        assert!(encode_capture_codec("").is_err());
        assert!(encode_capture_codec("rtsp://127.0.0.1").is_err());
        assert!(encode_capture_codec(&"x".repeat(CODEC_HEADER_LEN - 3)).is_err());
    }

    #[test]
    fn validates_dimensions_payload_length_and_pts_before_allocation() {
        let header = FrameHeader {
            width: 1920,
            height: 1080,
            frame: 12,
            raw_pts: Some(-90),
            source_micros: Some(-1_000),
            time_base_num: 1,
            time_base_den: 90_000,
            pts_quality: PtsQuality::Native,
            corrupt: false,
        };
        let mut bytes = header.encode().unwrap();
        assert_eq!(FrameHeader::decode(&bytes).unwrap(), header);
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(FrameHeader::decode(&bytes).is_err());
        assert!(
            FrameHeader {
                width: u32::MAX,
                ..header
            }
            .encode()
            .is_err()
        );
        assert!(
            FrameHeader {
                source_micros: Some(0),
                ..header
            }
            .encode()
            .is_err()
        );
    }
}
