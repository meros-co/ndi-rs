//! Source discovery: a long-lived [`Finder`] that keeps the SDK's view of the
//! network current, and [`watch_sources`], which reports the list on its own
//! thread whenever a source appears or goes away.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(has_ndi)]
use crate::ffi;

/// Keeps NDI discovery running for as long as it lives, so the list is
/// already current when asked for, rather than after a fresh wait.
pub struct Finder {
    #[cfg(has_ndi)]
    finder: ffi::NDIlib_find_instance_t,
}

// The SDK's finder may be used from any one thread at a time.
unsafe impl Send for Finder {}

impl Finder {
    /// Starts discovery, local sources included. Fails when NDI is not
    /// available.
    pub fn new() -> Result<Self, String> {
        #[cfg(has_ndi)]
        unsafe {
            if !ffi::NDIlib_initialize() {
                return Err("NDIlib_initialize failed".into());
            }
            let create = ffi::NDIlib_find_create_t {
                show_local_sources: true,
                p_groups: std::ptr::null(),
                p_extra_ips: std::ptr::null(),
            };
            let finder = ffi::NDIlib_find_create_v2(&create);
            if finder.is_null() {
                return Err("NDIlib_find_create_v2 failed".into());
            }
            Ok(Self { finder })
        }
        #[cfg(not(has_ndi))]
        Err("NDI is not built for this target".into())
    }

    /// The sources seen so far, by name ("MACHINE (Source)"), sorted.
    pub fn sources(&self) -> Vec<String> {
        #[cfg(has_ndi)]
        unsafe {
            use std::ffi::CStr;
            let mut count: u32 = 0;
            let sources = ffi::NDIlib_find_get_current_sources(self.finder, &mut count);
            let mut names = Vec::with_capacity(count as usize);
            for i in 0..count as isize {
                let src = &*sources.offset(i);
                if !src.p_ndi_name.is_null() {
                    names.push(
                        CStr::from_ptr(src.p_ndi_name)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            names.sort();
            names
        }
        #[cfg(not(has_ndi))]
        Vec::new()
    }

    /// Waits up to `timeout_ms` for the list to change; true when it did.
    pub fn wait_for_change(&self, timeout_ms: u32) -> bool {
        #[cfg(has_ndi)]
        unsafe {
            ffi::NDIlib_find_wait_for_sources(self.finder, timeout_ms)
        }
        #[cfg(not(has_ndi))]
        {
            std::thread::sleep(std::time::Duration::from_millis(timeout_ms.into()));
            false
        }
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        #[cfg(has_ndi)]
        unsafe {
            ffi::NDIlib_find_destroy(self.finder);
        }
    }
}

/// A running [`watch_sources`]; dropping it stops the watch.
pub struct SourceWatch {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SourceWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How often the watch thread checks for a stop while nothing changes.
const WAIT_MS: u32 = 250;

/// Calls `on_change` with the full, sorted list of sources whenever a source
/// appears or goes away, on its own thread, until the returned
/// [`SourceWatch`] is dropped. The first call comes once discovery has a
/// first answer, even an empty one. Fails when NDI is not available.
pub fn watch_sources(
    on_change: impl Fn(Vec<String>) + Send + 'static,
) -> Result<SourceWatch, String> {
    let finder = Finder::new()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("ndi-find".into())
        .spawn(move || {
            let mut last: Option<Vec<String>> = None;
            // A first answer after a short settle, then on every change.
            let mut settle = 4;
            while !flag.load(Ordering::Relaxed) {
                let changed = finder.wait_for_change(WAIT_MS);
                if settle > 0 {
                    settle -= 1;
                }
                if !changed && (last.is_some() || settle > 0) {
                    continue;
                }
                let now = finder.sources();
                if last.as_ref() != Some(&now) {
                    on_change(now.clone());
                    last = Some(now);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(SourceWatch {
        stop,
        thread: Some(thread),
    })
}

#[cfg(all(test, has_ndi))]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn until(deadline: Duration, ok: impl Fn() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        ok()
    }

    /// A local sender appears in the watch, and its removal is reported.
    /// Needs the NDI runtime (on PATH on Windows).
    #[test]
    #[ignore = "finds its sender through NDI discovery (multicast); run with --ignored"]
    fn the_watch_reports_a_sender_coming_and_going() {
        let lists: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let seen = lists.clone();
        let _watch = watch_sources(move |l| seen.lock().unwrap().push(l)).expect("watch");
        let name = format!("finder-test-{}", std::process::id());
        let has = |lists: &[Vec<String>]| {
            lists
                .last()
                .is_some_and(|l| l.iter().any(|s| s.contains(&name)))
        };
        let sender = crate::Sender::new(&name).expect("sender");
        assert!(
            until(Duration::from_secs(10), || has(&lists.lock().unwrap())),
            "the sender never appeared: {:?}",
            lists.lock().unwrap().last()
        );
        drop(sender);
        assert!(
            until(Duration::from_secs(15), || !has(&lists.lock().unwrap())),
            "the sender's removal was never reported"
        );
        // Every report is a change.
        let all = lists.lock().unwrap();
        assert!(all.windows(2).all(|w| w[0] != w[1]), "{all:?}");
    }
}
