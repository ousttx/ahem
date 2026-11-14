//! The computer's sound on macOS: a Core Audio tap of every process, whatever
//! device plays it, behind a private aggregate device that cpal records like a microphone.
//!
//! cpal's own loopback taps one output device and, for a device that also has a microphone
//! (a USB headset), records that microphone instead; the global tap has neither problem.
//! Needs macOS 14.2 and the "system audio recording" permission (`NSAudioCaptureUsageDescription`).

use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectID,
    CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSOperatingSystemVersion, NSProcessInfo, NSString,
};

use super::SystemAudio;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// Destroys the aggregate device and the tap when the recording of this track ends.
struct Tap {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
}

impl Drop for Tap {
    fn drop(&mut self) {
        // SAFETY: both objects were created by this process and are destroyed only here.
        unsafe {
            AudioHardwareDestroyAggregateDevice(self.aggregate);
            AudioHardwareDestroyProcessTap(self.tap);
        }
    }
}

fn key(name: &CStr) -> Retained<NSString> {
    NSString::from_str(&name.to_string_lossy())
}

/// Core Audio taps came with macOS 14.2; Core Audio is weakly linked (`build.rs`), so on an
/// older system their functions are missing and must not be called.
pub fn taps_available() -> bool {
    let version = NSOperatingSystemVersion { majorVersion: 14, minorVersion: 2, patchVersion: 0 };
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(version)
}

pub fn open_system_audio() -> Result<SystemAudio, String> {
    if !taps_available() {
        return Err("recording the computer's sound needs macOS 14.2 or later".into());
    }
    let instance = format!("{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    // SAFETY: plain Objective-C calls on objects created here; the tap description lives until
    // the tap is created from it.
    let (tap, tap_uid) = unsafe {
        let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
            CATapDescription::alloc(),
            &NSArray::new(),
        );
        description.setName(&NSString::from_str(&format!("ahem meeting {instance}")));
        description.setPrivate(true);
        // The user keeps hearing the call.
        description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        let mut tap: AudioObjectID = 0;
        let status = AudioHardwareCreateProcessTap(Some(&description), &mut tap);
        if status != 0 {
            return Err(format!("cannot tap the system sound (status {status})"));
        }
        (tap, description.UUID().UUIDString())
    };
    let uid = format!("com.useahem.ahem.meeting.{instance}");
    let sub_tap: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
        &[&*key(kAudioSubTapUIDKey), &*key(kAudioSubTapDriftCompensationKey)],
        &[Retained::into_super(Retained::into_super(tap_uid)), number(true)],
    );
    let taps = NSArray::from_retained_slice(&[sub_tap]);
    let description: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_retained_objects(
            &[
                &*key(kAudioAggregateDeviceNameKey),
                &*key(kAudioAggregateDeviceUIDKey),
                &*key(kAudioAggregateDeviceTapListKey),
                &*key(kAudioAggregateDeviceTapAutoStartKey),
                &*key(kAudioAggregateDeviceIsPrivateKey),
            ],
            &[
                Retained::into_super(Retained::into_super(NSString::from_str("ahem meeting"))),
                Retained::into_super(Retained::into_super(NSString::from_str(&uid))),
                Retained::into_super(Retained::into_super(taps)),
                number(true),
                number(true),
            ],
        );
    let mut aggregate: AudioObjectID = 0;
    // SAFETY: NSDictionary and CFDictionary are toll-free bridged; the dictionary outlives the
    // call and `aggregate` is a valid place for the new id.
    let status = unsafe {
        let dictionary = &*(Retained::as_ptr(&description).cast::<CFDictionary>());
        AudioHardwareCreateAggregateDevice(dictionary, NonNull::from(&mut aggregate))
    };
    if status != 0 {
        // SAFETY: created above, not used elsewhere.
        unsafe { AudioHardwareDestroyProcessTap(tap) };
        return Err(format!("cannot create the recording device (status {status})"));
    }
    let guard = Tap { tap, aggregate };
    // The new device shows up in the device list right away, or a moment later.
    for _ in 0..10 {
        if let Some(device) = find_device(&uid) {
            return Ok(SystemAudio { device, _keep: Box::new(guard) });
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("the recording device did not appear".into())
}

fn number(value: bool) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_bool(value))))
}

/// cpal's id of a Core Audio device is `coreaudio:<UID>`.
fn find_device(uid: &str) -> Option<cpal::Device> {
    cpal::default_host()
        .input_devices()
        .ok()?
        .find(|device| device.id().is_ok_and(|id| id.to_string().ends_with(uid)))
}
