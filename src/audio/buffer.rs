// ============================================================================
// ============================================================================
// Hold the frames of one speaker so that audio that arrives at uneven times
// can play on a steady 20 ms clock.
// ============================================================================
// ============================================================================

use std::collections::VecDeque;

use super::AudioFrame;

// Queue of frames for one speaker. It waits for a small number of frames
// before it starts to play, and waits again after it runs out.
pub struct JitterBuffer {
        _queue: VecDeque<AudioFrame>,
        _target: usize,
        _max: usize,
        _isPlaying: bool,
}

impl JitterBuffer {
        #[cfg(test)]
        pub fn getLength(&self) -> usize { self._queue.len() }

        #[cfg(test)]
        pub fn isPlaying(&self) -> bool { self._isPlaying }

        // Make a buffer that waits for `target_frames` before it plays. More
        // frames give more delay but survive larger network problems.
        pub fn New(target_frames: usize) -> Self {
                let target = target_frames.max(1);
                Self {
                        _queue: VecDeque::with_capacity(target * 4),
                        _target: target,
                        _max: target * 4,
                        _isPlaying: false,
                }
        }

        // Add a frame. Remove the oldest frames when the queue is too long, so
        // that the delay cannot grow without limit.
        pub fn Push(&mut self, frame: AudioFrame) {
                self._queue.push_back(frame);
                while self._queue.len() > self._max { self._queue.pop_front(); }
                if self._queue.len() >= self._target { self._isPlaying = true; }
        }

        // Remove one frame for this tick. Return silence while the buffer
        // fills, and start to fill again when it runs out.
        pub fn Pull(&mut self) -> AudioFrame {
                if !self._isPlaying { return AudioFrame::Silence(); }
                match self._queue.pop_front() {
                        Some(frame) => frame,
                        None => {
                                self._isPlaying = false;
                                AudioFrame::Silence()
                        }
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::audio::FRAME_LEN;

        fn Tone(value: i16) -> AudioFrame {
                AudioFrame::FromStereo(&[value; FRAME_LEN])
        }

        #[test]
        fn WaitsForTargetBeforePlaying() {
                let mut buffer = JitterBuffer::New(2);
                buffer.Push(Tone(10));
                assert!(buffer.Pull().isSilent());
                buffer.Push(Tone(20));
                assert!(buffer.isPlaying());
        }

        #[test]
        fn PlaysFramesInOrder() {
                let mut buffer = JitterBuffer::New(2);
                buffer.Push(Tone(10));
                buffer.Push(Tone(20));
                assert_eq!(buffer.Pull(), Tone(10));
                assert_eq!(buffer.Pull(), Tone(20));
        }

        #[test]
        fn EmptyBufferStopsPlaying() {
                let mut buffer = JitterBuffer::New(1);
                buffer.Push(Tone(10));
                buffer.Pull();
                assert!(buffer.Pull().isSilent());
                assert!(!buffer.isPlaying());
        }

        #[test]
        fn LongQueueDropsOldestFrames() {
                let mut buffer = JitterBuffer::New(2);
                for i in 0..20 { buffer.Push(Tone(i)); }
                assert!(buffer.getLength() <= 8);
                assert_ne!(buffer.Pull(), Tone(0));
        }
}
