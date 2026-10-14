//! Publishing an NDI source.

#[cfg(has_ndi)]
use crate::ffi;

/// One NDI sender. Video goes in as UYVY (or UYVA, UYVY followed by an
/// alpha plane) because the mobile SDKs silently drop RGBA; audio as
/// interleaved 16-bit 48 kHz stereo.
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
    /// (UYVA). `pts_us` is a wall-clock timestamp shared with
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
        #[cfg(has_ndi)]
        unsafe {
            let mut video: ffi::NDIlib_video_frame_v2_t = Default::default();
            video.xres = width as i32;
            video.yres = height as i32;
            video.FourCC = if alpha {
                ffi::NDIlib_FourCC_video_type_UYVA
            } else {
                ffi::NDIlib_FourCC_video_type_UYVY
            };
            video.frame_rate_N = fps_n;
            video.frame_rate_D = fps_d;
            video.picture_aspect_ratio = width as f32 / (height.max(1)) as f32;
            // Zeroed defaults are wrong for these two: 0 means "interleaved"
            // and "timecode = epoch", which receivers render as blank/frozen
            // video.
            video.frame_format_type = ffi::NDIlib_frame_format_type_progressive;
            // Explicit wall-clock timecode (100 ns units) shared with the
            // audio path so receivers lock A/V together.
            video.timecode = pts_us * 10;
            video.p_data = pixels.as_ptr() as *mut u8;
            // UYVA stride refers to the UYVY plane; alpha follows it.
            video.__bindgen_anon_1.line_stride_in_bytes = width as i32 * 2;
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
        #[cfg(not(has_ndi))]
        let _ = (pixels, width, height, alpha, fps_n, fps_d, pts_us);
    }

    /// Sends interleaved 16-bit 48 kHz stereo PCM, converted to NDI's planar
    /// float. Same clock as [`send_video`](Self::send_video).
    pub fn send_audio(&mut self, pcm: &[i16], pts_us: i64) {
        let no_samples = pcm.len() / 2;
        if no_samples == 0 {
            return;
        }
        #[cfg(has_ndi)]
        unsafe {
            let mut planar = vec![0f32; no_samples * 2];
            for i in 0..no_samples {
                planar[i] = pcm[i * 2] as f32 / 32768.0; // L plane
                planar[no_samples + i] = pcm[i * 2 + 1] as f32 / 32768.0; // R plane
            }
            let mut audio: ffi::NDIlib_audio_frame_v2_t = Default::default();
            audio.sample_rate = 48_000;
            audio.no_channels = 2;
            audio.no_samples = no_samples as i32;
            audio.p_data = planar.as_mut_ptr();
            audio.channel_stride_in_bytes = (no_samples * 4) as i32;
            // Same wall-clock as the video frame (100 ns units) so the
            // receiver aligns A/V instead of using arrival time.
            audio.timecode = pts_us * 10;
            ffi::NDIlib_send_send_audio_v2(self.instance, &audio);
        }
        #[cfg(not(has_ndi))]
        let _ = pts_us;
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
