// ============================================================================
// ============================================================================
// Mix the speakers that have no proxy into one stream for the main bot, which
// then speaks for "multiple users".
// ============================================================================
// ============================================================================

use std::collections::HashMap;

use crate::audio::AudioFrame;
use crate::audio::buffer::JitterBuffer;
use crate::audio::mixer;

// Remove a speaker after this many mix ticks without audio (100 ms).
const IDLE_TICKS_MAX: u8 = 5;

// A jitter buffer for each overflow speaker, mixed on a 20 ms clock.
pub struct OverflowMixer {
        _buffers: HashMap<String, (JitterBuffer, u8)>,
        _jitterFrames: usize,
}

impl OverflowMixer {
        pub fn isEmpty(&self) -> bool { self._buffers.is_empty() }

        pub fn New(jitter_frames: usize) -> Self {
                Self { _buffers: HashMap::new(), _jitterFrames: jitter_frames }
        }

        pub fn Push(&mut self, speaker: &str, frame: AudioFrame) {
                let jitter = self._jitterFrames;
                let entry = self
                        ._buffers
                        .entry(speaker.to_string())
                        .or_insert_with(|| (JitterBuffer::New(jitter), 0));
                entry.0.Push(frame);
                entry.1 = 0;
        }

        // Take one frame from each speaker and mix them. Return None when the
        // mix is silent, so that the main bot sends nothing.
        pub fn Pull(&mut self) -> Option<AudioFrame> {
                if self._buffers.is_empty() { return None; }
                let frames: Vec<AudioFrame> = self
                        ._buffers
                        .values_mut()
                        .map(|(buffer, idle)| {
                                *idle = idle.saturating_add(1);
                                buffer.Pull()
                        })
                        .collect();
                self._buffers.retain(|_, (_, idle)| *idle < IDLE_TICKS_MAX);
                let mixed = mixer::Mix(&frames);
                if mixed.isSilent() { None } else { Some(mixed) }
        }

        pub fn Clear(&mut self) { self._buffers.clear(); }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::audio::FRAME_LEN;

        fn Tone(value: i16) -> AudioFrame {
                AudioFrame::FromStereo(&[value; FRAME_LEN])
        }

        #[test]
        fn EmptyMixerGivesNothing() {
                assert!(OverflowMixer::New(1).Pull().is_none());
        }

        #[test]
        fn TwoSpeakersAreMixed() {
                let mut mixer = OverflowMixer::New(1);
                mixer.Push("a", Tone(100));
                mixer.Push("b", Tone(30));
                assert_eq!(mixer.Pull().unwrap().getSamples()[0], 130);
        }

        #[test]
        fn IdleSpeakerIsRemoved() {
                let mut mixer = OverflowMixer::New(1);
                mixer.Push("a", Tone(100));
                for _ in 0..=IDLE_TICKS_MAX { mixer.Pull(); }
                assert!(mixer.isEmpty());
        }
}
