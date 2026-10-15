// ============================================================================
// ============================================================================
// Mix many speakers into one frame and limit peaks so that the result does not
// clip.
// ============================================================================
// ============================================================================

use super::{AudioFrame, FRAME_LEN};

// Full-scale sample value, used to normalize samples to the range -1 to 1.
const PEAK: f32 = i16::MAX as f32;
// Below this normalized level the limiter does not change the signal.
const KNEE: f32 = 0.8;

// Add all frames together and apply the soft limiter, so that many loud
// speakers stay loud but do not clip.
pub fn Mix(frames: &[AudioFrame]) -> AudioFrame {
        let mut out = [0i16; FRAME_LEN];
        for (i, sample) in out.iter_mut().enumerate() {
                let sum: f32 =
                        frames.iter().map(|f| f.getSamples()[i] as f32).sum();
                *sample = ApplyKnee(sum);
        }
        AudioFrame::FromArray(out)
}

// Apply the soft limiter to one frame. Speech below the knee does not change.
pub fn Limit(frame: &AudioFrame) -> AudioFrame {
        let mut out = [0i16; FRAME_LEN];
        for (o, &s) in out.iter_mut().zip(frame.getSamples()) {
                *o = ApplyKnee(s as f32);
        }
        AudioFrame::FromArray(out)
}

// Compress one sample value above the knee with tanh, so that the result
// moves near full scale but does not reach it.
fn ApplyKnee(value: f32) -> i16 {
        let x = value / PEAK;
        let a = x.abs();
        let y = if a <= KNEE {
                x
        } else {
                let over = (a - KNEE) / (1.0 - KNEE);
                (KNEE + (1.0 - KNEE) * over.tanh()).copysign(x)
        };
        // Clamp, because rounding can land exactly on full scale.
        (y * PEAK).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
        use super::*;

        fn Tone(value: i16) -> AudioFrame {
                AudioFrame::FromStereo(&[value; FRAME_LEN])
        }

        #[test]
        fn MixOfNothingIsSilence() { assert!(Mix(&[]).isSilent()); }

        #[test]
        fn QuietSpeakersAddWithoutChange() {
                let mixed = Mix(&[Tone(1000), Tone(2000)]);
                assert_eq!(mixed.getSamples()[0], 3000);
        }

        #[test]
        fn LoudSpeakersDoNotWrap() {
                let mixed = Mix(&[Tone(i16::MAX), Tone(i16::MAX)]);
                let s = mixed.getSamples()[0];
                assert!(s > 30_000);
        }

        #[test]
        fn LimitKeepsSpeechLevels() {
                assert_eq!(Limit(&Tone(8000)), Tone(8000));
                assert!(Limit(&Tone(i16::MAX)).getSamples()[0] < i16::MAX);
        }
}
