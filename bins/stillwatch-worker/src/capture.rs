use std::{
    io::{self, BufWriter, Read, Write},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use sw_capture::{Decoder, PtsQuality as SourceQuality, ReadLimits, RgbFrame, SourceAdapter};
use sw_protocol::RtspTransport;
use sw_protocol::worker::{
    CONFIG_MAX_BYTES, CaptureRequest, CaptureSource, FrameHeader, MASK_UNITS, MaskRect, PtsQuality,
    WINDOWS_RTSPS_OPEN_TIMEOUT, encode_capture_codec,
};

pub fn run(with_codec: bool, with_rate: bool) -> Result<()> {
    let mut body = Vec::new();
    io::stdin()
        .lock()
        .take(CONFIG_MAX_BYTES as u64 + 1)
        .read_to_end(&mut body)?;
    ensure!(
        body.len() <= CONFIG_MAX_BYTES,
        "capture configuration exceeds limit"
    );
    let config: CaptureRequest =
        serde_json::from_slice(&body).context("invalid capture configuration")?;
    ensure!(
        config.masks.len() <= 32 && config.masks.iter().all(|rect| rect.valid()),
        "invalid privacy masks"
    );
    let (mut source, pace) = open(config.source)?;
    let mut output = BufWriter::new(io::stdout().lock());
    let mut first = None;
    let mut frame_id = 0u64;
    while let Some(mut frame) = source.read_rgb()? {
        frame_id = frame_id
            .checked_add(1)
            .context("capture sequence exhausted")?;
        if pace && let Some(pts) = frame.timestamp.micros {
            let (first_pts, start) = first.get_or_insert_with(|| (pts, Instant::now()));
            let elapsed = Duration::from_micros(pts.saturating_sub(*first_pts).max(0) as u64);
            let wait = elapsed.saturating_sub(start.elapsed());
            if wait > Duration::from_secs(5) {
                *first_pts = pts;
                *start = Instant::now();
            } else {
                std::thread::sleep(wait);
            }
        }
        mask(&mut frame, &config.masks);
        let timestamp = frame.timestamp;
        let header = FrameHeader {
            width: frame.width,
            height: frame.height,
            frame: frame_id,
            raw_pts: timestamp.raw_pts,
            source_micros: timestamp.micros,
            time_base_num: timestamp.time_base_num,
            time_base_den: timestamp.time_base_den,
            pts_quality: match timestamp.quality {
                SourceQuality::Native => PtsQuality::Native,
                SourceQuality::Estimated => PtsQuality::Estimated,
                SourceQuality::Missing => PtsQuality::Missing,
            },
            corrupt: frame.corrupt,
        };
        ensure!(
            frame.rgb.len() == header.byte_length()? as usize,
            "invalid RGB frame length"
        );
        if with_codec && frame_id == 1 {
            output.write_all(&encode_capture_codec(source.codec())?)?;
            if with_rate {
                output.write_all(
                    &source
                        .advertised_millifps()
                        .unwrap_or_default()
                        .to_le_bytes(),
                )?;
            }
        }
        output.write_all(&header.encode()?)?;
        output.write_all(&frame.rgb)?;
        output.flush()?;
    }
    Ok(())
}

pub fn devices() -> Result<()> {
    let devices: Vec<_> = sw_capture::devices()?
        .into_iter()
        .map(|device| sw_protocol::CaptureDevice {
            device_id: device.device_id,
            name: device.name,
        })
        .collect();
    serde_json::to_writer(io::stdout().lock(), &devices)?;
    Ok(())
}

fn open(source: CaptureSource) -> Result<(Decoder, bool)> {
    let mut limits = ReadLimits::default();
    if source.uses_schannel() {
        limits.open_timeout = WINDOWS_RTSPS_OPEN_TIMEOUT;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    match source {
        CaptureSource::Replay { file, pace } => {
            Ok((Decoder::open_file(&file, limits, cancelled)?, pace))
        }
        CaptureSource::Rtsp {
            peer,
            stream_path,
            secure,
            transport,
            credentials,
        } => {
            let tcp = transport == RtspTransport::Tcp;
            let public = sw_protocol::CameraSource::Rtsp {
                peer,
                stream_path: stream_path.clone(),
                main_stream_path: None,
                secure,
                transport,
                credential_ref: None,
            };
            ensure!(public.valid(), "invalid RTSP capture configuration");
            let authentication = credentials.map_or_else(String::new, |credentials| {
                format!(
                    "{}:{}@",
                    utf8_percent_encode(&credentials.username, NON_ALPHANUMERIC),
                    utf8_percent_encode(&credentials.password, NON_ALPHANUMERIC)
                )
            });
            let scheme = if secure { "rtsps" } else { "rtsp" };
            let url = format!("{scheme}://{authentication}{peer}{stream_path}");
            Ok((Decoder::open_rtsp(&url, tcp, limits, cancelled)?, false))
        }
        CaptureSource::Usb { device_id } => {
            Ok((Decoder::open_usb(&device_id, limits, cancelled)?, false))
        }
    }
}

fn mask(frame: &mut RgbFrame, masks: &[MaskRect]) {
    let stride = frame.width as usize * 3;
    for rect in masks {
        let left = u32::from(rect.left) * frame.width / MASK_UNITS;
        let right = (u32::from(rect.right) * frame.width).div_ceil(MASK_UNITS);
        let top = u32::from(rect.top) * frame.height / MASK_UNITS;
        let bottom = (u32::from(rect.bottom) * frame.height).div_ceil(MASK_UNITS);
        for y in top as usize..bottom as usize {
            frame.rgb[y * stride + left as usize * 3..y * stride + right as usize * 3].fill(0);
        }
    }
}
