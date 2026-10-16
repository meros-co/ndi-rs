//! Publishing an NDI source.

#[cfg(has_ndi)]
use crate::ffi;

/// The timecode NDI carries with a sent frame.
///
/// NDI timecode is a 64-bit count of 100 ns units chosen by the sender, kept
/// apart from the SDK's own arrival `timestamp`. A capture source passes its
/// own clock here (for example SMPTE time of day, or the sensor timestamp) so
/// receivers see the source's timing rather than the moment of the send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timecode {
    /// Let the SDK generate one from its clock.
    Synthesize,
    /// A caller-supplied timecode in 100 ns units.
    Ticks(i64),
}

impl Timecode {
    /// A timecode from microseconds.
    pub fn from_micros(us: i64) -> Self {
        Timecode::Ticks(us * 10)
    }

    /// A time-of-day timecode from SMPTE hours, minutes, seconds and frames
    /// at `fps_n / fps_d` frames per second (non-drop-frame counting).
    pub fn from_smpte(
        hours: u32,
        minutes: u32,
        seconds: u32,
        frames: u32,
        fps_n: u32,
        fps_d: u32,
    ) -> Self {
        let whole_seconds = (hours as i64 * 60 + minutes as i64) * 60 + seconds as i64;
        let frame_ticks = frames as i64 * 10_000_000 * fps_d.max(1) as i64 / fps_n.max(1) as i64;
        Timecode::Ticks(whole_seconds * 10_000_000 + frame_ticks)
    }

    #[cfg(has_ndi)]
    fn ticks(self) -> i64 {
        match self {
            Timecode::Synthesize => ffi::NDIlib_send_timecode_synthesize,
            Timecode::Ticks(t) => t,
        }
    }
}

/// Pixel layout of a [`VideoFrame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4:2:2 UYVY, 2 bytes per pixel.
    Uyvy,
    /// UYVY followed by a `width * height` alpha plane.
    Uyva,
}

/// One progressive video frame for [`Sender::send_video_frame`].
#[derive(Debug, Clone, Copy)]
pub struct VideoFrame<'a> {
    /// The UYVY plane (`stride * height` bytes), then for UYVA the alpha plane.
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes per UYVY row; 0 means `width * 2` (no padding).
    pub stride: u32,
    pub format: PixelFormat,
    pub fps_n: i32,
    pub fps_d: i32,
    pub timecode: Timecode,
}

/// Interleaved 16-bit PCM for [`Sender::send_audio_frame`].
#[derive(Debug, Clone, Copy)]
pub struct AudioFrame<'a> {
    /// `samples * channels` values, channel-interleaved.
    pub pcm: &'a [i16],
    pub channels: u32,
    pub sample_rate: u32,
    pub timecode: Timecode,
}

/// Tally reported by the receivers watching a sender.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Some receiver has this source on program (live).
    pub program: bool,
    /// Some receiver has this source on preview.
    pub preview: bool,
}

/// The result of [`Sender::poll_tally`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TallyPoll {
    /// The current tally.
    pub tally: Tally,
    /// Whether it changed since the previous poll.
    pub changed: bool,
}

/// One NDI sender. Video goes in as UYVY (or UYVA, UYVY followed by an
/// alpha plane) because the mobile SDKs silently drop RGBA; audio as
/// interleaved 16-bit PCM.
pub struct Sender {
    #[cfg(has_ndi)]
    instance: ffi::NDIlib_send_instance_t,
    #[cfg(has_ndi)]
    _name: std::ffi::CString,
    #[cfg(has_ndi)]
    frames_sent: u64,
}

unsafe impl Send for Sender {}

impl Sender {
    /// Creates a sender advertised as `name`. The SDK does not clock sends:
    /// the caller paces frames on its own deadlines (letting NDI clock as
    /// well made each send wait for the next slot on top of the caller's
    /// wait, halving the frame rate whenever a frame ran late).
    pub fn new(name: &str) -> Result<Self, String> {
        #[cfg(has_ndi)]
        unsafe {
            use std::ffi::CString;
            if !ffi::NDIlib_initialize() {
                return Err("NDIlib_initialize failed".into());
            }
            let c_name = CString::new(name).map_err(|e| e.to_string())?;
            let create = ffi::NDIlib_send_create_t {
                p_ndi_name: c_name.as_ptr(),
                p_groups: std::ptr::null(),
                clock_video: false,
                clock_audio: false,
            };
            let instance = ffi::NDIlib_send_create(&create);
            if instance.is_null() {
                return Err("NDIlib_send_create failed".into());
            }
            Ok(Self {
                instance,
                _name: c_name,
                frames_sent: 0,
            })
        }
        #[cfg(not(has_ndi))]
        {
            let _ = name;
            Err("NDI is not built for this target".into())
        }
    }

    /// Sends one progressive frame. `pixels` is UYVY (`width * 2` bytes per
    /// row), followed by a `width * height` alpha plane when `alpha` is set
    /// (UYVA). `pts_us` becomes the frame's timecode and is shared with
    /// [`send_audio`](Self::send_audio) so receivers lock A/V together.
    #[allow(clippy::too_many_arguments)]
    pub fn send_video(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
        alpha: bool,
        fps_n: i32,
        fps_d: i32,
        pts_us: i64,
    ) {
        self.send_video_frame(&VideoFrame {
            pixels,
            width,
            height,
            stride: 0,
            format: if alpha {
                PixelFormat::Uyva
            } else {
                PixelFormat::Uyvy
            },
            fps_n,
            fps_d,
            timecode: Timecode::from_micros(pts_us),
        });
    }

    /// Sends one progressive frame with its own timecode and row stride.
    /// A frame shorter than its size needs is dropped with a warning.
    pub fn send_video_frame(&mut self, frame: &VideoFrame) {
        let stride = if frame.stride == 0 {
            frame.width * 2
        } else {
            frame.stride
        };
        let mut needed = stride as usize * frame.height as usize;
        if frame.format == PixelFormat::Uyva {
            needed += frame.width as usize * frame.height as usize;
        }
        if frame.pixels.len() < needed {
            log::warn!(
                "NDI sink: frame of {} bytes is shorter than the {} its size needs; dropped",
                frame.pixels.len(),
                needed
            );
            return;
        }
        #[cfg(has_ndi)]
        unsafe {
            let mut video: ffi::NDIlib_video_frame_v2_t = Default::default();
            video.xres = frame.width as i32;
            video.yres = frame.height as i32;
            video.FourCC = match frame.format {
                PixelFormat::Uyvy => ffi::NDIlib_FourCC_video_type_UYVY,
                PixelFormat::Uyva => ffi::NDIlib_FourCC_video_type_UYVA,
            };
            video.frame_rate_N = frame.fps_n;
            video.frame_rate_D = frame.fps_d;
            video.picture_aspect_ratio = frame.width as f32 / (frame.height.max(1)) as f32;
            // Zeroed defaults are wrong for these two: 0 means "interleaved"
            // and "timecode = epoch", which receivers render as blank/frozen
            // video.
            video.frame_format_type = ffi::NDIlib_frame_format_type_progressive;
            video.timecode = frame.timecode.ticks();
            video.p_data = frame.pixels.as_ptr() as *mut u8;
            // UYVA stride refers to the UYVY plane; alpha follows it.
            video.__bindgen_anon_1.line_stride_in_bytes = stride as i32;
            ffi::NDIlib_send_send_video_v2(self.instance, &video);
            self.frames_sent += 1;
            if self.frames_sent % 20 == 1 {
                let conns = ffi::NDIlib_send_get_no_connections(self.instance, 0);
                log::info!(
                    "NDI sink: {} frames sent, {} receiver connection(s)",
                    self.frames_sent,
                    conns
                );
            }
        }
    }

    /// Sends interleaved 16-bit 48 kHz stereo PCM, converted to NDI's planar
    /// float. Same clock as [`send_video`](Self::send_video).
    pub fn send_audio(&mut self, pcm: &[i16], pts_us: i64) {
        self.send_audio_frame(&AudioFrame {
            pcm,
            channels: 2,
            sample_rate: 48_000,
            timecode: Timecode::from_micros(pts_us),
        });
    }

    /// Sends interleaved 16-bit PCM of any channel count and rate with its
    /// own timecode, converted to NDI's planar float.
    pub fn send_audio_frame(&mut self, frame: &AudioFrame) {
        let channels = frame.channels.max(1) as usize;
        let no_samples = frame.pcm.len() / channels;
        if no_samples == 0 {
            return;
        }
        #[cfg(has_ndi)]
        unsafe {
            let mut planar = vec![0f32; no_samples * channels];
            for i in 0..no_samples {
                for c in 0..channels {
                    planar[c * no_samples + i] = frame.pcm[i * channels + c] as f32 / 32768.0;
                }
            }
            let mut audio: ffi::NDIlib_audio_frame_v2_t = Default::default();
            audio.sample_rate = frame.sample_rate as i32;
            audio.no_channels = channels as i32;
            audio.no_samples = no_samples as i32;
            audio.p_data = planar.as_mut_ptr();
            audio.channel_stride_in_bytes = (no_samples * 4) as i32;
            // The caller's clock (100 ns units), shared with the video frames
            // so the receiver aligns A/V instead of using arrival time.
            audio.timecode = frame.timecode.ticks();
            ffi::NDIlib_send_send_audio_v2(self.instance, &audio);
        }
    }

    /// The program/preview tally the receivers watching this source report.
    /// With `timeout_ms` 0 it returns at once; otherwise it waits up to that
    /// long for a change, which suits a dedicated tally thread.
    pub fn poll_tally(&self, timeout_ms: u32) -> TallyPoll {
        #[cfg(has_ndi)]
        unsafe {
            let mut tally = ffi::NDIlib_tally_t {
                on_program: false,
                on_preview: false,
            };
            let changed = ffi::NDIlib_send_get_tally(self.instance, &mut tally, timeout_ms);
            TallyPoll {
                tally: Tally {
                    program: tally.on_program,
                    preview: tally.on_preview,
                },
                changed,
            }
        }
        #[cfg(not(has_ndi))]
        {
            let _ = timeout_ms;
            TallyPoll::default()
        }
    }

    /// Receivers currently connected.
    pub fn connections(&self) -> i32 {
        #[cfg(has_ndi)]
        unsafe {
            ffi::NDIlib_send_get_no_connections(self.instance, 0)
        }
        #[cfg(not(has_ndi))]
        0
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        #[cfg(has_ndi)]
        unsafe {
            ffi::NDIlib_send_destroy(self.instance)
        };
    }
}
