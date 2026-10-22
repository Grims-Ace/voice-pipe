// ============================================================================
// ============================================================================
// Decide which frames of one speaker are speech. A loud frame opens the gate.
// The gate stays open until the speaker is quiet for the hangover time, so
// that quiet sounds inside speech are not cut out.
// ============================================================================
// ============================================================================

use std::time::Duration;

use super::{AudioFrame, FRAME_TIME};

// ============================================================================
// Types
// ============================================================================

// The speech gate of one speaker. `_threshold` is a level (root mean
// square, 0 to 32768). When it is 0, all frames that are not digital silence
// pass.
pub struct SpeechGate {
        _threshold: f32,
        _hangoverFrames: u32,
        _quietFrames: u32,
        _isOpen: bool,
}

// ============================================================================
// Speech Gate
// ============================================================================

impl SpeechGate {
        pub fn New(threshold: u16, hangover: Duration) -> Self {
                let frames = hangover.as_millis() / FRAME_TIME.as_millis();
                Self {
                        _threshold: f32::from(threshold),
                        _hangoverFrames: u32::try_from(frames)
                                .unwrap_or(u32::MAX),
                        _quietFrames: 0,
                        _isOpen: false,
                }
        }

        // Return true if the frame must go to the bridge.
        pub fn Pass(&mut self, frame: &AudioFrame) -> bool {
                if frame.isSilent() { return self.Close(); }
                if self._threshold == 0.0 { return true; }
                if frame.getLevel() >= self._threshold {
                        self._isOpen = true;
                        self._quietFrames = 0;
                        return true;
                }
                if !self._isOpen { return false; }
                self._quietFrames += 1;
                if self._quietFrames > self._hangoverFrames {
                        return self.Close();
                }
                true
        }

        // Close the gate and block the frame. Digital silence means that the
        // speaker stopped, so the gate does not wait for the hangover.
        fn Close(&mut self) -> bool {
                self._isOpen = false;
                false
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::audio::FRAME_LEN;

        fn Frame(level: i16) -> AudioFrame {
                AudioFrame::FromStereo(&[level; FRAME_LEN])
        }

        // A 300 ms hangover is 15 frames.
        fn Gate(threshold: u16) -> SpeechGate {
                SpeechGate::New(threshold, Duration::from_millis(300))
        }

        #[test]
        fn QuietNoiseAloneDoesNotOpen() {
                let mut gate = Gate(50);
                assert!(!gate.Pass(&Frame(10)));
                assert!(!gate.Pass(&Frame(49)));
        }

        #[test]
        fn QuietFramesInsideSpeechPass() {
                let mut gate = Gate(50);
                assert!(gate.Pass(&Frame(500)));
                assert!(gate.Pass(&Frame(10)));
                assert!(gate.Pass(&Frame(500)));
        }

        #[test]
        fn GateClosesAfterTheHangover() {
                let mut gate = Gate(50);
                assert!(gate.Pass(&Frame(500)));
                for _ in 0..15 { assert!(gate.Pass(&Frame(10))); }
                assert!(!gate.Pass(&Frame(10)));
                assert!(!gate.Pass(&Frame(10)));
        }

        #[test]
        fn DigitalSilenceNeverPasses() {
                let mut gate = Gate(0);
                assert!(!gate.Pass(&AudioFrame::Silence()));
                assert!(gate.Pass(&Frame(1)));
        }
}
