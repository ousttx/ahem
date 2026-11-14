//! 16-bit PCM mono WAV in memory.

pub fn encode(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn header_describes_16k_mono_pcm() {
        let wav = encode(&[0.0; 16_000], 16_000);
        assert_eq!(wav.len(), 44 + 32_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(&wav, 4), 36 + 32_000);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "PCM");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
        assert_eq!(u32_at(&wav, 24), 16_000);
        assert_eq!(u32_at(&wav, 28), 32_000);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(&wav, 40), 32_000);
    }

    #[test]
    fn samples_are_clamped_to_16_bit() {
        let wav = encode(&[1.5, -2.0, 0.5], 16_000);
        let value = |i: usize| i16::from_le_bytes([wav[44 + 2 * i], wav[45 + 2 * i]]);
        assert_eq!(value(0), i16::MAX);
        assert_eq!(value(1), -i16::MAX);
        assert_eq!(value(2), 16_384);
    }
}
