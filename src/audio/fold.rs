// ============================================================================
// ============================================================================
// Turn the stereo audio of one speaker into mono from one channel, so that
// two channels that differ cannot make a hollow, echoing sound when they are
// added together.
// ============================================================================
// ============================================================================

use super::AudioFrame;

// Weight of the newest frame in the average energy of each channel. With
// 50 frames each second, the average follows the level over about 0.4 s.
const ENERGY_WEIGHT: f64 = 0.05;
// The fold changes to the other channel only when that channel has this
// many times the energy of the channel in use. Without this margin, the
// fold could jump between channels in the middle of a word.
const SWITCH_RATIO: f64 = 2.0;

// Picks one channel of a speaker and copies it to both channels. Some
// senders give two channels that are not equal: one is a little late, or
// was processed in another way. Fluxer adds the two channels together, and
// two channels like that make a hollow, echoing sound when they are added.
// One channel alone does not. For a normal microphone both channels are
// equal, so the audio does not change.
#[derive(Default)]
pub struct MonoFold {
        _energy: [f64; 2],
        _channel: usize,
}

impl MonoFold {
        #[cfg(test)]
        pub fn getChannel(&self) -> usize { self._channel }

        // Return the frame with the louder channel copied to both channels.
        pub fn Fold(&mut self, frame: &AudioFrame) -> AudioFrame {
                self.UpdateChannel(frame);
                frame.CopyChannel(self._channel)
        }

        // Update the average energy of each channel, and change to the other
        // channel when it is clearly louder.
        fn UpdateChannel(&mut self, frame: &AudioFrame) {
                let energy = frame.getChannelEnergy();
                for (average, new) in self._energy.iter_mut().zip(energy) {
                        *average += (new - *average) * ENERGY_WEIGHT;
                }
                let other = 1 - self._channel;
                let current = self._energy[self._channel];
                if self._energy[other] > current * SWITCH_RATIO {
                        self._channel = other;
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::audio::FRAME_LEN;

        // A frame with `left` in every left sample and `right` in every right
        // sample.
        fn Stereo(left: i16, right: i16) -> AudioFrame {
                let mut samples = [0i16; FRAME_LEN];
                for pair in samples.as_chunks_mut::<2>().0 {
                        *pair = [left, right];
                }
                AudioFrame::FromArray(samples)
        }

        #[test]
        fn EqualChannelsDoNotChange() {
                let mut fold = MonoFold::default();
                let frame = Stereo(1000, 1000);
                assert_eq!(fold.Fold(&frame), frame);
        }

        #[test]
        fn OneChannelIsCopiedToBoth() {
                let mut fold = MonoFold::default();
                let folded = fold.Fold(&Stereo(1000, -400));
                let start = &folded.getSamples()[..4];
                assert_eq!(start, &[1000, 1000, 1000, 1000]);
        }

        #[test]
        fn SilentLeftChannelUsesTheRight() {
                // A microphone that only sends on the right channel.
                let mut fold = MonoFold::default();
                let folded = fold.Fold(&Stereo(0, 500));
                assert_eq!(fold.getChannel(), 1);
                assert_eq!(&folded.getSamples()[..2], &[500, 500]);
        }

        #[test]
        fn SmallDifferencesDoNotSwitch() {
                // The right channel is a little louder, but not twice as
                // loud, so the fold stays on the left channel.
                let mut fold = MonoFold::default();
                for _ in 0..100 { fold.Fold(&Stereo(1000, 1200)); }
                assert_eq!(fold.getChannel(), 0);
        }

        #[test]
        fn ClearlyLouderChannelWins() {
                let mut fold = MonoFold::default();
                fold.Fold(&Stereo(1000, 1000));
                for _ in 0..100 { fold.Fold(&Stereo(200, 3000)); }
                assert_eq!(fold.getChannel(), 1);
        }
}
