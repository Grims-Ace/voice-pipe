// ============================================================================
// ============================================================================
// Define the one audio format that moves through the bridge: 20 ms frames of
// 48 kHz stereo 16-bit PCM. Both voice platforms convert to this format.
// ============================================================================
// ============================================================================

pub mod buffer;
pub mod mixer;

use std::fmt;
use std::time::Duration;

// ============================================================================
// Format Constants
// ============================================================================

// Samples per second for each channel. Discord and LiveKit both use 48 kHz.
pub const SAMPLE_RATE: u32 = 48_000;
// Stereo audio with interleaved samples (L, R, L, R, ...).
pub const CHANNELS: usize = 2;
// Length of one frame. Both platforms send audio in 20 ms packets.
pub const FRAME_TIME: Duration = Duration::from_millis(20);
// 48_000 samples per second * 0.020 seconds = 960 samples per channel.
pub const SAMPLES_PER_CHANNEL: usize = SAMPLE_RATE as usize / 50;
// Total interleaved samples in one stereo frame.
pub const FRAME_LEN: usize = SAMPLES_PER_CHANNEL * CHANNELS;

// One 20 ms frame of interleaved stereo PCM audio.
#[derive(Clone, PartialEq, Eq)]
pub struct AudioFrame {
        _samples: [i16; FRAME_LEN],
}

impl AudioFrame {
        pub fn getSamples(&self) -> &[i16] { &self._samples }

        // Return the root mean square level of the frame (0 to 32768).
        pub fn getLevel(&self) -> f32 {
                let sum: f64 = self
                        ._samples
                        .iter()
                        .map(|&s| (s as f64) * (s as f64))
                        .sum();
                (sum / FRAME_LEN as f64).sqrt() as f32
        }

        pub fn isSilent(&self) -> bool { self._samples.iter().all(|&s| s == 0) }

        pub fn Silence() -> Self { Self { _samples: [0; FRAME_LEN] } }

        pub fn FromArray(samples: [i16; FRAME_LEN]) -> Self {
                Self { _samples: samples }
        }

        // Copy interleaved stereo samples. Pad short input with silence and
        // cut long input, because SDKs do not always give exactly one frame.
        pub fn FromStereo(src: &[i16]) -> Self {
                let mut frame = Self::Silence();
                let len = src.len().min(FRAME_LEN);
                frame._samples[..len].copy_from_slice(&src[..len]);
                frame
        }

        // Copy mono samples into both stereo channels.
        pub fn FromMono(src: &[i16]) -> Self {
                let mut frame = Self::Silence();
                let len = src.len().min(SAMPLES_PER_CHANNEL);
                for (i, &s) in src[..len].iter().enumerate() {
                        frame._samples[i * 2] = s;
                        frame._samples[i * 2 + 1] = s;
                }
                frame
        }

        // Make a frame from decoded audio of unknown channel count. A full
        // stereo frame has FRAME_LEN samples, so shorter input is mono.
        pub fn FromDecoded(src: &[i16]) -> Self {
                if src.len() >= FRAME_LEN {
                        Self::FromStereo(src)
                } else {
                        Self::FromMono(src)
                }
        }

        // Write the samples as little-endian f32 bytes, the raw PCM format
        // that Songbird plays.
        pub fn WriteFloatBytes(&self, out: &mut Vec<u8>) {
                out.reserve(FRAME_LEN * 4);
                for &s in &self._samples {
                        out.extend_from_slice(
                                &(s as f32 / 32768.0).to_le_bytes(),
                        );
                }
        }
}

impl fmt::Debug for AudioFrame {
        // Show the level only, because 1920 samples are not useful in logs.
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "AudioFrame {{ level: {:.1} }}", self.getLevel())
        }
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn FrameHasTwentyMillisecondsOfStereo() {
                assert_eq!(SAMPLES_PER_CHANNEL, 960);
                assert_eq!(FRAME_LEN, 1920);
        }

        #[test]
        fn FromStereoPadsShortInput() {
                let frame = AudioFrame::FromStereo(&[100, -100]);
                assert_eq!(&frame.getSamples()[..3], &[100, -100, 0]);
        }

        #[test]
        fn FromMonoCopiesToBothChannels() {
                let frame = AudioFrame::FromMono(&[500, -250]);
                assert_eq!(&frame.getSamples()[..4], &[500, 500, -250, -250]);
        }

        #[test]
        fn OnlyZeroFramesAreSilent() {
                assert!(AudioFrame::Silence().isSilent());
                assert!(!AudioFrame::FromStereo(&[1; FRAME_LEN]).isSilent());
        }
}
