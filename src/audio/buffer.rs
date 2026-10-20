// ============================================================================
// ============================================================================
// Hold the frames of one speaker so that audio that arrives at uneven times
// can play on a steady 20 ms clock.
// ============================================================================
// ============================================================================

use std::collections::VecDeque;

use super::AudioFrame;

// Largest target, in frames. A sender's packet holds at most 120 ms (six
// frames), and one more frame covers late arrival.
const BURST_TARGET_MAX: usize = 7;
// The longest queue is this many times the target.
const MAX_PER_TARGET: usize = 4;

// Queue of frames for one speaker. It waits for a small number of frames
// before it starts to play, and waits again after it runs out.
pub struct JitterBuffer {
        _queue: VecDeque<AudioFrame>,
        _target: usize,
        _max: usize,
        _isPlaying: bool,
        _pushedSincePull: usize,
}

impl JitterBuffer {
        pub fn isEmpty(&self) -> bool { self._queue.is_empty() }

        #[cfg(test)]
        pub fn getLength(&self) -> usize { self._queue.len() }

        #[cfg(test)]
        pub fn getTarget(&self) -> usize { self._target }

        #[cfg(test)]
        pub fn isPlaying(&self) -> bool { self._isPlaying }

        // Make a buffer that waits for `target_frames` before it plays. More
        // frames give more delay but survive larger network problems.
        pub fn New(target_frames: usize) -> Self {
                let target = target_frames.max(1);
                let max = target * MAX_PER_TARGET;
                Self {
                        _queue: VecDeque::with_capacity(max),
                        _target: target,
                        _max: max,
                        _isPlaying: false,
                        _pushedSincePull: 0,
                }
        }

        // Add a frame. Remove the oldest frames when the queue is too long, so
        // that the delay cannot grow without limit.
        pub fn Push(&mut self, frame: AudioFrame) {
                self._pushedSincePull += 1;
                if self._isPlaying { self.FitBurst(self._pushedSincePull); }
                self._queue.push_back(frame);
                while self._queue.len() > self._max { self._queue.pop_front(); }
                if self._queue.len() >= self._target { self._isPlaying = true; }
        }

        // Raise the target above the size of a burst. A sender with 40, 60,
        // or 120 ms packets gives two, three, or six frames at once. With a
        // target that is not larger than the burst, the queue runs empty just
        // before the next packet, and the buffer stops to fill again. Only
        // bursts during play count, because the first fill is always one.
        fn FitBurst(&mut self, burst: usize) {
                if burst < self._target { return; }
                self._target = (burst + 1).min(BURST_TARGET_MAX);
                self._max = self._target * MAX_PER_TARGET;
        }

        // Remove one frame for this tick. Return silence while the buffer
        // fills, and start to fill again when it runs out.
        pub fn Pull(&mut self) -> AudioFrame {
                self._pushedSincePull = 0;
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
                // Two frames come for each frame that plays, so the queue
                // grows until the limit removes the oldest frames.
                let mut buffer = JitterBuffer::New(2);
                for i in 0..20 {
                        buffer.Push(Tone(i * 2));
                        buffer.Push(Tone(i * 2 + 1));
                        buffer.Pull();
                }
                let max = buffer.getTarget() * MAX_PER_TARGET;
                assert!(buffer.getLength() <= max);
                assert_ne!(buffer.Pull(), Tone(20));
        }

        #[test]
        fn BurstsRaiseTheTarget() {
                // 60 ms packets: three frames at once, then three ticks.
                let mut buffer = JitterBuffer::New(3);
                for packet in 0..4 {
                        for i in 0..3 { buffer.Push(Tone(packet * 3 + i + 1)); }
                        for _ in 0..3 { buffer.Pull(); }
                }
                assert_eq!(buffer.getTarget(), 4);
                assert!(buffer.isPlaying());
        }

        #[test]
        fn TargetHasALimit() {
                let mut buffer = JitterBuffer::New(1);
                buffer.Push(Tone(1));
                for i in 0..20 { buffer.Push(Tone(i + 2)); }
                assert_eq!(buffer.getTarget(), BURST_TARGET_MAX);
        }
}
