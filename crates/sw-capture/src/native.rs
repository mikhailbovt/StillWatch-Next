//! The libav interrupt context and frame allocation live at this boundary.
#![allow(unsafe_code)]

use std::{
    ffi::{CStr, CString, c_int, c_void},
    path::Path,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use ffmpeg::{Dictionary, Packet, Rational, ffi, format, frame::Video, software::scaling};
use ffmpeg_next as ffmpeg;

use crate::{CaptureDevice, CaptureError, RgbFrame, SourceTimestamp};

#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub open_timeout: Duration,
    pub frame_timeout: Duration,
    pub max_width: u32,
    pub max_height: u32,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            open_timeout: Duration::from_secs(5),
            frame_timeout: Duration::from_secs(3),
            max_width: 3840,
            max_height: 2160,
        }
    }
}

impl ReadLimits {
    fn validate(self) -> Result<Self, CaptureError> {
        if self.open_timeout.is_zero()
            || self.open_timeout > Duration::from_secs(60)
            || self.frame_timeout.is_zero()
            || self.frame_timeout > Duration::from_secs(60)
            || !(1..=7680).contains(&self.max_width)
            || !(1..=4320).contains(&self.max_height)
        {
            return Err(CaptureError::InvalidLimits);
        }
        Ok(self)
    }

    fn check_dimensions(self, width: u32, height: u32) -> Result<(), CaptureError> {
        if width == 0 || height == 0 || width > self.max_width || height > self.max_height {
            return Err(CaptureError::Dimensions);
        }
        Ok(())
    }
}

struct Interrupt {
    origin: Instant,
    deadline_ns: AtomicU64,
    cancelled: Arc<AtomicBool>,
}

impl Interrupt {
    fn new(cancelled: Arc<AtomicBool>, budget: Duration) -> Self {
        let state = Self {
            origin: Instant::now(),
            deadline_ns: AtomicU64::new(0),
            cancelled,
        };
        state.arm(budget);
        state
    }

    fn arm(&self, budget: Duration) {
        let until = self.origin.elapsed().saturating_add(budget);
        self.deadline_ns.store(
            u64::try_from(until.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn check(&self) -> Result<(), CaptureError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(CaptureError::Cancelled);
        }
        if self.origin.elapsed().as_nanos() >= u128::from(self.deadline_ns.load(Ordering::Relaxed))
        {
            return Err(CaptureError::Deadline);
        }
        Ok(())
    }
}

extern "C" fn interrupt_callback(opaque: *mut c_void) -> c_int {
    // SAFETY: open_input supplies a stable boxed Interrupt. The owning input
    // closes libavformat before freeing the box. All callback state is read-only or atomic.
    let state = unsafe { &*opaque.cast::<Interrupt>() };
    c_int::from(state.check().is_err())
}

struct Input {
    context: format::context::Input,
    // The caller-owned AVIO handle closes after the format context. Both can
    // invoke the interrupt callback during close.
    _replay_io: Option<ReplayIo>,
    interrupt: Box<Interrupt>,
}

struct ReplayIo(*mut ffi::AVIOContext);

impl Drop for ReplayIo {
    fn drop(&mut self) {
        // SAFETY: avio_open2 created this file handle and the format context
        // uses AVFMT_FLAG_CUSTOM_IO, so ownership remains here.
        unsafe { ffi::avio_closep(&mut self.0) };
    }
}

extern "C" fn refuse_nested_io(
    _: *mut ffi::AVFormatContext,
    _: *mut *mut ffi::AVIOContext,
    _: *const std::ffi::c_char,
    _: c_int,
    _: *mut *mut ffi::AVDictionary,
) -> c_int {
    ffi::AVERROR_INVALIDDATA
}

fn open_input(
    path: &CStr,
    demuxer: Option<&CStr>,
    options: Dictionary<'_>,
    interrupt: Box<Interrupt>,
    replay: bool,
) -> Result<Input, CaptureError> {
    interrupt.check()?;
    let callback = ffi::AVIOInterruptCB {
        callback: Some(interrupt_callback),
        opaque: ptr::from_ref(interrupt.as_ref()).cast_mut().cast(),
    };
    let replay_io = if replay {
        let mut io = ptr::null_mut();
        // SAFETY: path is a canonical regular file supplied by open_file.
        // libav copies the callback, which points to the live boxed Interrupt.
        let result = unsafe {
            ffi::avio_open2(
                &mut io,
                path.as_ptr(),
                ffi::AVIO_FLAG_READ,
                &callback,
                ptr::null_mut(),
            )
        };
        if result < 0 {
            interrupt.check()?;
            return Err(ffmpeg::Error::from(result).into());
        }
        Some(ReplayIo(io))
    } else {
        None
    };
    // SAFETY: libav owns the allocated context after open_input. On success it
    // is wrapped once so avformat_close_input runs before the interrupt is dropped.
    // The dictionary pointer is returned by libav and re-owned immediately.
    let mut context = unsafe {
        let format = if let Some(name) = demuxer {
            let format = ffi::av_find_input_format(name.as_ptr());
            if format.is_null() {
                return Err(CaptureError::NoVideo);
            }
            format
        } else {
            ptr::null()
        };
        let mut pointer = ffi::avformat_alloc_context();
        if pointer.is_null() {
            return Err(CaptureError::Allocation);
        }
        (*pointer).interrupt_callback = callback;
        if let Some(io) = &replay_io {
            (*pointer).pb = io.0;
            (*pointer).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
            (*pointer).io_open = Some(refuse_nested_io);
        }
        let mut dictionary = options.disown();
        // A fixed name gives demuxers no server-side path to resolve. The
        // byte stream comes solely from the caller-owned regular file.
        let name = if replay { c"replay" } else { path };
        let result = ffi::avformat_open_input(&mut pointer, name.as_ptr(), format, &mut dictionary);
        drop(Dictionary::own(dictionary));
        if result < 0 {
            interrupt.check()?;
            return Err(ffmpeg::Error::from(result).into());
        }
        format::context::Input::wrap(pointer)
    };
    // SAFETY: the open context is uniquely borrowed and no stream options are passed.
    let result = unsafe { ffi::avformat_find_stream_info(context.as_mut_ptr(), ptr::null_mut()) };
    interrupt.check()?;
    if result < 0 {
        return Err(ffmpeg::Error::from(result).into());
    }
    Ok(Input {
        context,
        _replay_io: replay_io,
        interrupt,
    })
}

pub struct Decoder {
    input: Input,
    stream_index: usize,
    time_base: Rational,
    advertised_millifps: Option<u32>,
    decoder: ffmpeg::decoder::Video,
    decoded: Video,
    converter: Option<Converter>,
    limits: ReadLimits,
    draining: bool,
}

pub fn confined_rtsp_available() -> bool {
    // SAFETY: the input format and AVClass are static libav objects. Fake
    // option lookup receives the address of a class pointer and does not mutate it.
    unsafe {
        let format = ffi::av_find_input_format(c"rtsp".as_ptr());
        let Some(format) = format.as_ref() else {
            return false;
        };
        if format.priv_class.is_null() {
            return false;
        }
        let class = format.priv_class;
        !ffi::av_opt_find(
            ptr::from_ref(&class).cast_mut().cast(),
            c"same_peer".as_ptr(),
            c"rtsp_flags".as_ptr(),
            0,
            ffi::AV_OPT_SEARCH_FAKE_OBJ,
        )
        .is_null()
    }
}

pub fn decoder_identity() -> String {
    format!(
        "libavcodec-{}-libavformat-{}-libswscale-{}",
        ffmpeg::codec::version(),
        ffmpeg::format::version(),
        ffmpeg::software::scaling::version()
    )
}

impl Decoder {
    pub fn open_file(
        path: &Path,
        limits: ReadLimits,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, CaptureError> {
        let path = path.canonicalize()?;
        if !path.metadata()?.is_file() {
            return Err(CaptureError::InvalidFile);
        }
        let path = CString::new(path.to_str().ok_or(CaptureError::InvalidFile)?)
            .map_err(|_| CaptureError::InvalidFile)?;
        let mut options = Dictionary::new();
        options.set("protocol_whitelist", "file");
        options.set(
            "format_whitelist",
            "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,avi,mpegts",
        );
        options.set("enable_drefs", "0");
        Self::open(&path, None, options, limits, cancelled, true)
    }

    pub fn open_rtsp(
        url: &str,
        tcp: bool,
        limits: ReadLimits,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, CaptureError> {
        let Some((scheme, resource)) = url.split_once("://") else {
            return Err(CaptureError::InvalidNetwork);
        };
        let authority = resource
            .split_once('/')
            .and_then(|(authority, _)| authority.rsplit('@').next());
        if !(scheme == "rtsp" || (tcp && scheme == "rtsps"))
            || !authority.is_some_and(|value| value.parse::<std::net::SocketAddr>().is_ok())
        {
            return Err(CaptureError::InvalidNetwork);
        }
        if !confined_rtsp_available() {
            return Err(CaptureError::NetworkPolicyUnavailable);
        }
        let url = CString::new(url).map_err(|_| CaptureError::InvalidNetwork)?;
        let mut options = Dictionary::new();
        options.set(
            "protocol_whitelist",
            if url.to_bytes().starts_with(b"rtsps:") {
                "tcp,tls"
            } else if tcp {
                "tcp"
            } else {
                "tcp,udp,rtp"
            },
        );
        options.set("rtsp_transport", if tcp { "tcp" } else { "udp" });
        options.set("rtsp_flags", "filter_src+same_peer");
        options.set("allowed_media_types", "video");
        options.set("tls_verify", "1");
        options.set("timeout", "3000000");
        format::network::init();
        Self::open(&url, Some(c"rtsp"), options, limits, cancelled, false)
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub fn open_usb(
        _device: &str,
        _limits: ReadLimits,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<Self, CaptureError> {
        Err(CaptureError::UnsupportedDevice)
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub fn open_usb(
        device: &str,
        limits: ReadLimits,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, CaptureError> {
        if device.is_empty()
            || device.len() > 256
            || device.contains(':')
            || device.chars().any(char::is_control)
        {
            return Err(CaptureError::InvalidDevice);
        }
        #[cfg(windows)]
        let (path, format) = (format!("video={device}"), c"dshow");
        #[cfg(target_os = "linux")]
        let (path, format) = {
            use std::os::unix::fs::FileTypeExt;
            if device.contains('/') || device.contains('\\') || matches!(device, "." | "..") {
                return Err(CaptureError::InvalidDevice);
            }
            let path = Path::new("/dev/v4l/by-id")
                .join(device)
                .canonicalize()
                .map_err(|_| CaptureError::InvalidDevice)?;
            let is_video = path.parent() == Some(Path::new("/dev"))
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix("video"))
                    .is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                    });
            if !is_video
                || !path
                    .metadata()
                    .is_ok_and(|meta| meta.file_type().is_char_device())
            {
                return Err(CaptureError::InvalidDevice);
            }
            (path.to_string_lossy().into_owned(), c"v4l2")
        };
        let path = CString::new(path).map_err(|_| CaptureError::InvalidDevice)?;
        Self::open(
            &path,
            Some(format),
            Dictionary::new(),
            limits,
            cancelled,
            false,
        )
    }

    fn open(
        path: &CStr,
        demuxer: Option<&CStr>,
        mut options: Dictionary<'_>,
        limits: ReadLimits,
        cancelled: Arc<AtomicBool>,
        replay: bool,
    ) -> Result<Self, CaptureError> {
        let limits = limits.validate()?;
        ffmpeg::init()?;
        ffmpeg::log::set_level(ffmpeg::log::Level::Quiet);
        options.set("max_streams", "32");
        options.set("probesize", "5242880");
        let input = open_input(
            path,
            demuxer,
            options,
            Box::new(Interrupt::new(cancelled, limits.open_timeout)),
            replay,
        )?;
        let stream = input
            .context
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or(CaptureError::NoVideo)?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let rate = stream.avg_frame_rate();
        let advertised_millifps = if rate.numerator() > 0 && rate.denominator() > 0 {
            let rate = u64::try_from(rate.numerator()).unwrap() * 1_000 / rate.denominator() as u64;
            u32::try_from(rate)
                .ok()
                .filter(|rate| (1..=240_000).contains(rate))
        } else {
            None
        };
        let mut decoder = ffmpeg::codec::Context::from_parameters(stream.parameters())?.decoder();
        let codec = ffmpeg::decoder::find(decoder.id()).ok_or(CaptureError::NoVideo)?;
        decoder.set_packet_time_base(time_base);
        let mut options = Dictionary::new();
        options.set(
            "max_pixels",
            &(u64::from(limits.max_width) * u64::from(limits.max_height)).to_string(),
        );
        options.set("threads", "1");
        let decoder = decoder.open_as_with(codec, options)?.video()?;
        if decoder.width() > limits.max_width || decoder.height() > limits.max_height {
            return Err(CaptureError::Dimensions);
        }
        Ok(Self {
            input,
            stream_index,
            time_base,
            advertised_millifps,
            decoder,
            decoded: alloc_frame()?,
            converter: None,
            limits,
            draining: false,
        })
    }

    pub fn codec(&self) -> &'static str {
        self.decoder.id().name()
    }

    pub fn advertised_millifps(&self) -> Option<u32> {
        self.advertised_millifps
    }

    pub fn next_frame(&mut self) -> Result<Option<RgbFrame>, CaptureError> {
        self.next_frame_reusing(Vec::new())
    }

    pub fn next_frame_reusing(&mut self, rgb: Vec<u8>) -> Result<Option<RgbFrame>, CaptureError> {
        self.input.interrupt.arm(self.limits.frame_timeout);
        loop {
            self.input.interrupt.check()?;
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    self.input.interrupt.check()?;
                    return self.copy_frame(rgb).map(Some);
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {}
                Err(error) => return Err(error.into()),
            }
            if self.draining {
                return Err(CaptureError::BadFrame);
            }
            let mut packet = Packet::empty();
            match packet.read(&mut self.input.context) {
                Ok(()) if packet.stream() == self.stream_index => {
                    self.decoder.send_packet(&packet)?
                }
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => {
                    self.decoder.send_eof()?;
                    self.draining = true;
                }
                Err(error) => {
                    self.input.interrupt.check()?;
                    return Err(error.into());
                }
            }
        }
    }

    fn copy_frame(&mut self, mut rgb: Vec<u8>) -> Result<RgbFrame, CaptureError> {
        let frame = &self.decoded;
        let width = frame.width();
        let height = frame.height();
        self.limits.check_dimensions(width, height)?;
        if self
            .converter
            .as_ref()
            .is_none_or(|converter| !converter.matches(frame))
        {
            self.converter = Some(Converter::new(frame)?);
        }
        let converter = self.converter.as_mut().ok_or(CaptureError::BadFrame)?;
        converter.convert(frame, &mut rgb)?;
        self.input.interrupt.check()?;
        Ok(RgbFrame {
            width,
            height,
            rgb,
            corrupt: frame.is_corrupt(),
            timestamp: SourceTimestamp::from_pts(
                frame.pts(),
                frame.timestamp(),
                self.time_base.numerator(),
                self.time_base.denominator(),
            ),
        })
    }
}

fn alloc_frame() -> Result<Video, CaptureError> {
    // SAFETY: allocation is checked and Video takes sole ownership of a live AVFrame.
    unsafe {
        let pointer = ffi::av_frame_alloc();
        if pointer.is_null() {
            return Err(CaptureError::Allocation);
        }
        Ok(Video::wrap(pointer))
    }
}

#[cfg(target_os = "linux")]
pub fn devices() -> Result<Vec<CaptureDevice>, CaptureError> {
    use std::os::unix::fs::FileTypeExt;
    let mut devices = Vec::new();
    let entries = match std::fs::read_dir("/dev/v4l/by-id") {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(devices),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.take(128) {
        let entry = entry?;
        if !entry
            .path()
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_char_device())
        {
            continue;
        }
        let Ok(device_id) = entry.file_name().into_string() else {
            continue;
        };
        let name = entry
            .path()
            .canonicalize()
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_owned()))
            .and_then(|name| {
                std::fs::read_to_string(Path::new("/sys/class/video4linux").join(name).join("name"))
                    .ok()
            })
            .map(|name| name.trim().chars().take(128).collect())
            .unwrap_or_else(|| device_id.clone());
        devices.push(CaptureDevice { device_id, name });
    }
    devices.sort_unstable_by(|a, b| a.device_id.cmp(&b.device_id));
    Ok(devices)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn devices() -> Result<Vec<CaptureDevice>, CaptureError> {
    Err(CaptureError::UnsupportedDevice)
}

#[cfg(windows)]
pub fn devices() -> Result<Vec<CaptureDevice>, CaptureError> {
    struct List(*mut ffi::AVDeviceInfoList);
    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: avdevice_list_input_sources initializes this owned list pointer.
            unsafe { ffi::avdevice_free_list_devices(&mut self.0) };
        }
    }
    ffmpeg::init()?;
    ffmpeg::log::set_level(ffmpeg::log::Level::Quiet);
    let mut list = List(ptr::null_mut());
    // SAFETY: the named DirectShow demuxer is immutable. libavdevice owns and
    // initializes the output list. All copied strings are consumed before it is freed.
    unsafe {
        let format = ffi::av_find_input_format(c"dshow".as_ptr());
        if format.is_null() {
            return Err(CaptureError::InvalidDevice);
        }
        let count =
            ffi::avdevice_list_input_sources(format, ptr::null(), ptr::null_mut(), &mut list.0);
        if count < 0 {
            return Err(ffmpeg::Error::from(count).into());
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        if list.0.is_null() || (*list.0).devices.is_null() {
            return Err(CaptureError::InvalidDevice);
        }
        let mut devices = Vec::new();
        for index in 0..(count as usize).min(128) {
            let Some(entry) = (*(*list.0).devices.add(index)).as_ref() else {
                continue;
            };
            if entry.device_name.is_null()
                || entry.media_types.is_null()
                || !(1..=32).contains(&entry.nb_media_types)
            {
                continue;
            }
            let media =
                std::slice::from_raw_parts(entry.media_types, entry.nb_media_types as usize);
            if !media.contains(&ffi::AVMediaType::AVMEDIA_TYPE_VIDEO) {
                continue;
            }
            let device_id = CStr::from_ptr(entry.device_name)
                .to_string_lossy()
                .into_owned();
            let name = if entry.device_description.is_null() {
                device_id.clone()
            } else {
                CStr::from_ptr(entry.device_description)
                    .to_string_lossy()
                    .into_owned()
            };
            if device_id.len() <= 256 && name.len() <= 512 {
                devices.push(CaptureDevice { device_id, name });
            }
        }
        devices.sort_unstable_by(|a, b| a.device_id.cmp(&b.device_id));
        Ok(devices)
    }
}

struct Converter {
    context: scaling::Context,
    output: Video,
}

impl Converter {
    fn new(source: &Video) -> Result<Self, CaptureError> {
        let width = source.width();
        let height = source.height();
        let format = format::Pixel::RGB24;
        let context = scaling::Context::get(
            source.format(),
            width,
            height,
            format,
            width,
            height,
            scaling::Flags::BILINEAR,
        )?;
        let mut output = alloc_frame()?;
        output.set_format(format);
        output.set_width(width);
        output.set_height(height);
        // SAFETY: output is a new AVFrame with format and validated dimensions set.
        let result = unsafe { ffi::av_frame_get_buffer(output.as_mut_ptr(), 32) };
        if result < 0 {
            return Err(ffmpeg::Error::from(result).into());
        }
        Ok(Self { context, output })
    }

    fn matches(&self, frame: &Video) -> bool {
        let input = self.context.input();
        (input.format, input.width, input.height) == (frame.format(), frame.width(), frame.height())
    }

    fn convert(&mut self, source: &Video, pixels: &mut Vec<u8>) -> Result<(), CaptureError> {
        // SAFETY: source is a successfully decoded software frame and matches
        // the scaler definition. output owns an allocated, writable RGB24 buffer.
        let rows = unsafe {
            let source = &*source.as_ptr();
            let output = &mut *self.output.as_mut_ptr();
            ffi::sws_scale(
                self.context.as_mut_ptr(),
                source.data.as_ptr().cast(),
                source.linesize.as_ptr(),
                0,
                source.height,
                output.data.as_ptr(),
                output.linesize.as_ptr(),
            )
        };
        if rows != self.output.height() as i32 {
            return Err(CaptureError::BadFrame);
        }
        let row_bytes = self.output.width() as usize * 3;
        let stride = self.output.stride(0);
        if stride < row_bytes {
            return Err(CaptureError::BadFrame);
        }
        let height = self.output.height() as usize;
        pixels.clear();
        pixels.reserve(row_bytes * height);
        for row in self.output.data(0).chunks_exact(stride).take(height) {
            pixels.extend_from_slice(&row[..row_bytes]);
        }
        if pixels.len() != row_bytes * height {
            return Err(CaptureError::BadFrame);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};

    use super::*;

    #[test]
    fn rgb_conversion_reuses_buffer_and_replaces_every_pixel() {
        ffmpeg::init().unwrap();
        let mut frame = Video::new(format::Pixel::RGB24, 17, 3);
        let mut converter = Converter::new(&frame).unwrap();
        let stride = frame.stride(0);
        let row_bytes = 17 * 3;
        let mut pixels = vec![0xcd; row_bytes * 3];
        let allocation = pixels.as_ptr();

        for shade in [11, 73] {
            frame.data_mut(0).fill(0xff);
            for (row, data) in frame
                .data_mut(0)
                .chunks_exact_mut(stride)
                .take(3)
                .enumerate()
            {
                data[..row_bytes].fill(shade + row as u8);
            }
            converter.convert(&frame, &mut pixels).unwrap();
            assert_eq!(pixels.as_ptr(), allocation);
            assert_eq!(pixels.len(), row_bytes * 3);
            for (row, data) in pixels.chunks_exact(row_bytes).enumerate() {
                assert!(data.iter().all(|&value| value == shade + row as u8));
            }
        }
    }

    #[test]
    fn libav_interrupts_a_stalled_loopback_stream() {
        ffmpeg::init().unwrap();
        format::network::init();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = CString::new(format!("tcp://{}", listener.local_addr().unwrap())).unwrap();
        let mut options = Dictionary::new();
        options.set("protocol_whitelist", "tcp");
        let started = Instant::now();
        let state = Box::new(Interrupt::new(
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(150),
        ));
        assert!(matches!(
            open_input(&url, None, options, state, false),
            Err(CaptureError::Deadline)
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
