//! The format of the default output device. A wireless headset whose microphone opens switches
//! into a headset mode with another rate and channel count; its format coming back tells when
//! it plays at full quality again, so the sound can return without a gap.

use cpal::traits::{DeviceTrait, HostTrait};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFormat {
    pub device: String,
    pub rate: u32,
    pub channels: u16,
}

/// `None` when there is no output device or the audio system does not answer.
pub fn current() -> Option<OutputFormat> {
    let device = cpal::default_host().default_output_device()?;
    let id = device.id().ok()?.to_string();
    let config = device.default_output_config().ok()?;
    Some(OutputFormat { device: id, rate: config.sample_rate(), channels: config.channels() })
}

/// Whether the sound may return: the output is back in the format it had before the
/// microphone opened, or it is another device now, or its format is unknown.
pub fn settled(before: &OutputFormat, now: Option<&OutputFormat>) -> bool {
    match now {
        Some(now) => now.device != before.device || now == before,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(device: &str, rate: u32, channels: u16) -> OutputFormat {
        OutputFormat { device: device.into(), rate, channels }
    }

    #[test]
    fn settled_once_the_headset_format_is_back() {
        let before = format("airpods", 48_000, 2);
        assert!(!settled(&before, Some(&format("airpods", 24_000, 1))), "headset mode");
        assert!(settled(&before, Some(&before)));
        assert!(settled(&before, Some(&format("speakers", 44_100, 2))), "another device");
        assert!(settled(&before, None), "nothing to wait for");
    }
}
