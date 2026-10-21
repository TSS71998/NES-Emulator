use std::time::{Duration, Instant};

/// How long before the deadline to stop sleeping and spin instead, since
/// sleep can overshoot.
const SPIN_MARGIN: Duration = Duration::from_micros(2000);

/// Paces the emulator to the console's frame rate.
pub struct FrameLimiter {
    frame_time: Duration,
    next_frame: Instant,
}

impl FrameLimiter {
    pub fn new(fps: f64) -> Self {
        let frame_time = Duration::from_secs_f64(1.0 / fps);
        FrameLimiter {
            frame_time,
            next_frame: Instant::now() + frame_time,
        }
    }

    /// Waits until the next frame is due. If the emulator falls more than 4
    /// frames behind, it resyncs instead of rushing to catch up.
    pub fn wait(&mut self) {
        let now = Instant::now();

        if now < self.next_frame {
            let remaining = self.next_frame - now;
            if remaining > SPIN_MARGIN {
                std::thread::sleep(remaining - SPIN_MARGIN);
            }
            while Instant::now() < self.next_frame {
                std::thread::yield_now();
            }
            self.next_frame += self.frame_time;
        } else if now - self.next_frame > self.frame_time * 4 {
            self.next_frame = now + self.frame_time;
        } else {
            self.next_frame += self.frame_time;
        }
    }
}
