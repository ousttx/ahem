//! Muting the system output while recording (a setting, on by default).
//!
//! A marker file is written before muting and removed after restoring, so a crash never
//! leaves the user's sound off: the next start finds the marker and unmutes. Sound that the
//! user had already muted is left alone.
//!
//! The default output is muted, and that same device is unmuted, even when another one became
//! the default meanwhile (headphones plugged in, a headset in its headset mode). Its id is in
//! the marker. A device that is gone by then is forgotten.

use std::fs;
use std::io;
use std::path::PathBuf;

/// The output devices by their system id.
pub trait SystemOutput {
    /// The default output device; `None` without one.
    fn default_device(&self) -> io::Result<Option<String>>;
    /// `true` for a device that cannot be muted or is gone, so nothing gets touched.
    fn muted(&self, device: &str) -> io::Result<bool>;
    /// Does nothing for a device that cannot be muted or is gone.
    fn set_muted(&self, device: &str, muted: bool) -> io::Result<()>;
}

pub struct MuteGuard<O: SystemOutput> {
    output: O,
    marker: PathBuf,
    /// The device ahem muted, while it is muted.
    muted: Option<String>,
}

/// The marker's first line; the muted device is on the second.
const MARKER: &str = "muted by ahem";

impl<O: SystemOutput> MuteGuard<O> {
    pub fn new(output: O, marker: PathBuf) -> Self {
        Self { output, marker, muted: None }
    }

    /// Unmutes sound left muted by a crash. Call once at startup; `true` if it did.
    pub fn recover(&self) -> io::Result<bool> {
        if !self.marker.exists() {
            return Ok(false);
        }
        let text = fs::read_to_string(&self.marker)?;
        // A marker of an earlier version names no device: it muted the default one.
        let device = match text.lines().nth(1).filter(|line| !line.is_empty()) {
            Some(device) => Some(device.to_string()),
            None => self.output.default_device()?,
        };
        if let Some(device) = device {
            self.output.set_muted(&device, false)?;
        }
        fs::remove_file(&self.marker)?;
        Ok(true)
    }

    pub fn mute(&mut self) -> io::Result<()> {
        if self.muted.is_some() {
            return Ok(());
        }
        let Some(device) = self.output.default_device()? else {
            return Ok(());
        };
        if self.output.muted(&device)? {
            return Ok(());
        }
        if let Some(dir) = self.marker.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&self.marker, format!("{MARKER}\n{device}\n"))?;
        if let Err(error) = self.output.set_muted(&device, true) {
            let _ = fs::remove_file(&self.marker);
            return Err(error);
        }
        self.muted = Some(device);
        Ok(())
    }

    pub fn restore(&mut self) -> io::Result<()> {
        let Some(device) = &self.muted else {
            return Ok(());
        };
        self.output.set_muted(device, false)?;
        self.muted = None;
        fs::remove_file(&self.marker)
    }
}

impl<O: SystemOutput> Drop for MuteGuard<O> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// The speakers, the default device unless a test changes it.
    const SPEAKERS: &str = "speakers";
    const HEADPHONES: &str = "headphones";

    /// The devices that are there, each muted or not, and the default one.
    #[derive(Clone)]
    struct FakeOutput {
        default: Rc<RefCell<String>>,
        devices: Rc<RefCell<HashMap<String, bool>>>,
        broken: Rc<Cell<bool>>,
    }

    impl Default for FakeOutput {
        fn default() -> Self {
            let output = Self {
                default: Rc::new(RefCell::new(SPEAKERS.into())),
                devices: Rc::default(),
                broken: Rc::default(),
            };
            output.plug(SPEAKERS, false);
            output
        }
    }

    impl FakeOutput {
        fn plug(&self, device: &str, muted: bool) {
            self.devices.borrow_mut().insert(device.into(), muted);
        }

        fn is_muted(&self, device: &str) -> bool {
            self.devices.borrow()[device]
        }
    }

    impl SystemOutput for FakeOutput {
        fn default_device(&self) -> io::Result<Option<String>> {
            Ok(Some(self.default.borrow().clone()))
        }

        fn muted(&self, device: &str) -> io::Result<bool> {
            Ok(self.devices.borrow().get(device).copied().unwrap_or(true))
        }

        fn set_muted(&self, device: &str, muted: bool) -> io::Result<()> {
            if self.broken.get() {
                return Err(io::Error::other("cannot reach the device"));
            }
            if let Some(state) = self.devices.borrow_mut().get_mut(device) {
                *state = muted;
            }
            Ok(())
        }
    }

    fn marker() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!("mute-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-artifacts").join(name);
        let _ = fs::remove_dir_all(&dir);
        dir.join("muted.marker")
    }

    #[test]
    fn mutes_and_restores_with_a_marker() {
        let output = FakeOutput::default();
        let path = marker();
        let mut guard = MuteGuard::new(output.clone(), path.clone());
        guard.mute().unwrap();
        assert!(output.is_muted(SPEAKERS));
        assert!(path.exists(), "marker while muted");
        guard.restore().unwrap();
        assert!(!output.is_muted(SPEAKERS));
        assert!(!path.exists());
    }

    #[test]
    fn sound_muted_by_the_user_is_left_alone() {
        let output = FakeOutput::default();
        output.plug(SPEAKERS, true);
        let path = marker();
        let mut guard = MuteGuard::new(output.clone(), path.clone());
        guard.mute().unwrap();
        guard.restore().unwrap();
        assert!(output.is_muted(SPEAKERS), "still muted by the user");
        assert!(!path.exists());
    }

    #[test]
    fn the_muted_device_is_restored_when_another_became_the_default() {
        let output = FakeOutput::default();
        let mut guard = MuteGuard::new(output.clone(), marker());
        guard.mute().unwrap();
        // Headphones muted by the user are plugged in while recording and become the default.
        output.plug(HEADPHONES, true);
        *output.default.borrow_mut() = HEADPHONES.into();
        guard.restore().unwrap();
        assert!(!output.is_muted(SPEAKERS));
        assert!(output.is_muted(HEADPHONES), "left as the user set it");
    }

    #[test]
    fn a_muted_device_that_is_gone_is_forgotten() {
        let output = FakeOutput::default();
        let path = marker();
        let mut guard = MuteGuard::new(output.clone(), path.clone());
        guard.mute().unwrap();
        output.devices.borrow_mut().remove(SPEAKERS);
        guard.restore().unwrap();
        assert!(!path.exists());
        // The next recording mutes again.
        output.plug(SPEAKERS, false);
        guard.mute().unwrap();
        assert!(output.is_muted(SPEAKERS));
    }

    #[test]
    fn crash_is_recovered_at_the_next_start() {
        let output = FakeOutput::default();
        let path = marker();
        let mut guard = MuteGuard::new(output.clone(), path.clone());
        guard.mute().unwrap();
        std::mem::forget(guard); // the process died while muted
        assert!(output.is_muted(SPEAKERS) && path.exists());

        // The device in the marker is unmuted, not the default one of the next start.
        output.plug(HEADPHONES, true);
        *output.default.borrow_mut() = HEADPHONES.into();
        let next_start = MuteGuard::new(output.clone(), path.clone());
        assert!(next_start.recover().unwrap());
        assert!(!output.is_muted(SPEAKERS));
        assert!(output.is_muted(HEADPHONES));
        assert!(!path.exists());
        assert!(!next_start.recover().unwrap(), "nothing left to recover");
    }

    #[test]
    fn a_marker_without_a_device_unmutes_the_default_one() {
        let output = FakeOutput::default();
        output.plug(SPEAKERS, true);
        let path = marker();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "muted by ahem\n").unwrap();
        assert!(MuteGuard::new(output.clone(), path.clone()).recover().unwrap());
        assert!(!output.is_muted(SPEAKERS));
        assert!(!path.exists());
    }

    #[test]
    fn drop_restores_sound() {
        let output = FakeOutput::default();
        let mut guard = MuteGuard::new(output.clone(), marker());
        guard.mute().unwrap();
        drop(guard);
        assert!(!output.is_muted(SPEAKERS));
    }

    #[test]
    fn failed_mute_leaves_no_marker() {
        let output = FakeOutput::default();
        output.broken.set(true);
        let path = marker();
        let mut guard = MuteGuard::new(output, path.clone());
        assert!(guard.mute().is_err());
        assert!(!path.exists());
    }
}
