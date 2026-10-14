//! Receiving one NDI source. `capture` blocks up to its timeout, so callers
//! normally run it on a dedicated thread.

#[cfg(has_ndi)]
use crate::ffi;

/// NDI bandwidth tier: full quality, or the sender's low-bandwidth preview
/// stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    /// Full quality (NDI Standard or HX at source rate).
    Highest,
    /// Preview quality stream (lower resolution/bitrate).
    Lowest,
}

/// One result of [`Receiver::capture`].
#[derive(Debug)]
pub enum Capture {
    /// A raw UYVY frame: `stride * height` bytes, `stride >= width * 2`.
    Video {
        data: Vec<u8>,
        width: i32,
        height: i32,
        stride: usize,
        /// Sender timestamp in microseconds.
        pts_us: i64,
    },
    /// Interleaved 16-bit PCM at the sender's rate and channel count. May be
    /// empty.
    Audio {
        pcm: Vec<i16>,
        sample_rate: i32,
        channels: i32,
        /// Sender timestamp in microseconds.
        pts_us: i64,
    },
    /// Nothing arrived before the timeout (or only metadata/status).
    Nothing,
    /// The SDK reported a receive error; the connection is unusable.
    Error,
}

/// A connection to one NDI source by name.
pub struct Receiver {
    #[cfg(has_ndi)]
    recv: ffi::NDIlib_recv_instance_t,
}

// The SDK's receive instance may be used from any one thread at a time.
unsafe impl Send for Receiver {}

impl Receiver {
    /// Connects to `source_name` (as returned by
    /// [`discover_sources`](crate::discover_sources)). The SDK connects in
    /// the background; frames follow from [`capture`](Self::capture).
    pub fn connect(source_name: &str, bandwidth: Bandwidth) -> Result<Self, String> {
        #[cfg(has_ndi)]
        unsafe {
            use std::ffi::CString;
            if !ffi::NDIlib_initialize() {
                return Err("NDIlib_initialize failed".into());
            }
            let c_name = CString::new(source_name).unwrap_or_default();
            let source = ffi::NDIlib_source_t {
                p_ndi_name: c_name.as_ptr(),
                __bindgen_anon_1: Default::default(),
            };
            let create = ffi::NDIlib_recv_create_v3_t {
                source_to_connect_to: source,
                color_format: ffi::NDIlib_recv_color_format_fastest,
                bandwidth: match bandwidth {
                    Bandwidth::Highest => ffi::NDIlib_recv_bandwidth_highest,
                    Bandwidth::Lowest => ffi::NDIlib_recv_bandwidth_lowest,
                },
                allow_video_fields: false,
                p_ndi_recv_name: std::ptr::null(),
            };
            let recv = ffi::NDIlib_recv_create_v3(&create);
            if recv.is_null() {
                return Err("NDIlib_recv_create_v3 failed".into());
            }
            Ok(Self { recv })
        }
        #[cfg(not(has_ndi))]
        {
            let _ = (source_name, bandwidth);
            Err("NDI is not built for this target".into())
        }
    }

    /// Waits up to `timeout_ms` for the next video or audio frame.
    pub fn capture(&mut self, timeout_ms: u32) -> Capture {
        #[cfg(has_ndi)]
        unsafe {
            let mut video: ffi::NDIlib_video_frame_v2_t = Default::default();
            let mut audio: ffi::NDIlib_audio_frame_v3_t = Default::default();
            let frame_type = ffi::NDIlib_recv_capture_v3(
                self.recv,
                &mut video,
                &mut audio,
                std::ptr::null_mut(),
                timeout_ms,
            );
            if frame_type == ffi::NDIlib_frame_type_audio {
                // NDI delivers planar float; the SDK util converts to
                // interleaved 16-bit PCM. Then free the source frame.
                let n = (audio.no_samples.max(0) * audio.no_channels.max(0)) as usize;
                let mut pcm = vec![0i16; n];
                let mut dst = ffi::NDIlib_audio_frame_interleaved_16s_t {
                    sample_rate: audio.sample_rate,
                    no_channels: audio.no_channels,
                    no_samples: audio.no_samples,
                    timecode: audio.timecode,
                    reference_level: 0,
                    p_data: pcm.as_mut_ptr(),
                };
                ffi::NDIlib_util_audio_to_interleaved_16s_v3(&audio, &mut dst);
                let (sample_rate, channels) = (audio.sample_rate, audio.no_channels);
                let pts_us = audio.timestamp * 10 / 100; // 100ns ticks → µs
                ffi::NDIlib_recv_free_audio_v3(self.recv, &audio);
                Capture::Audio {
                    pcm,
                    sample_rate,
                    channels,
                    pts_us,
                }
            } else if frame_type == ffi::NDIlib_frame_type_video {
                let stride = video.__bindgen_anon_1.line_stride_in_bytes as usize;
                let size = stride * video.yres as usize;
                let data = std::slice::from_raw_parts(video.p_data, size).to_vec();
                let capture = Capture::Video {
                    data,
                    width: video.xres,
                    height: video.yres,
                    stride,
                    pts_us: video.timestamp * 10 / 100, // 100ns ticks → µs
                };
                ffi::NDIlib_recv_free_video_v2(self.recv, &video);
                capture
            } else if frame_type == ffi::NDIlib_frame_type_error {
                Capture::Error
            } else {
                Capture::Nothing
            }
        }
        #[cfg(not(has_ndi))]
        {
            let _ = timeout_ms;
            Capture::Error
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        #[cfg(has_ndi)]
        unsafe {
            ffi::NDIlib_recv_destroy(self.recv)
        };
    }
}
