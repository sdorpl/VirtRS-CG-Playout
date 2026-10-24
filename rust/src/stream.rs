// Frame stream of one session: ETV packets for an ErsatzTV script element (and
// raw yuva420p frames for ffmpeg, etap 6). The reader takes one frame per
// output frame; the stream is paced by the clock with a lead, so it follows
// `-readrate 1.0` with its initial burst but never runs far ahead of the live
// VizRS graphics. Every output frame takes the picture that was current at
// its own moment (src/stream.js).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::packets::{Rect, picture_packet, repeat_packet};

const TICK: Duration = Duration::from_millis(20);
/// Pictures are dated when Chromium drew them and arrive decoded up to ~100 ms
/// later; output frames look this far back so each finds the picture of its moment.
pub const SAMPLE_DELAY: Duration = Duration::from_millis(100);

/// One picture of the graphics; `seq` grows with every new picture.
#[derive(Debug, Default)]
pub struct Frame {
    pub seq: u64,
    pub rects: Vec<Rect>,
    /// Built once, shared by the sessions of the same size.
    packet: OnceLock<Vec<u8>>,
}

impl Frame {
    pub fn new(seq: u64, rects: Vec<Rect>) -> Frame {
        Frame { seq, rects, packet: OnceLock::new() }
    }

    pub fn packet(&self) -> &[u8] {
        self.packet.get_or_init(|| picture_packet(&self.rects))
    }
}

/// Pictures for the sessions: the one shown at a moment.
pub trait Source: Send + Sync {
    fn frame_at(&self, time: Instant) -> Arc<Frame>;
}

/// Transparent frames: burn-in off, or the channel size unknown.
pub struct Empty(Arc<Frame>);

impl Empty {
    pub fn new() -> Empty {
        Empty(Arc::new(Frame::new(0, Vec::new())))
    }
}

impl Default for Empty {
    fn default() -> Self {
        Empty::new()
    }
}

impl Source for Empty {
    fn frame_at(&self, _: Instant) -> Arc<Frame> {
        Arc::clone(&self.0)
    }
}

/// Pause, resume and end of a stream from other threads.
pub struct Control {
    paused: Mutex<bool>,
    wake: Condvar,
    closed: AtomicBool,
}

impl Control {
    pub fn new(paused: bool) -> Arc<Control> {
        Arc::new(Control { paused: Mutex::new(paused), wake: Condvar::new(), closed: AtomicBool::new(false) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, bool> {
        self.paused.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn pause(&self) {
        *self.lock() = true;
    }

    pub fn resume(&self) {
        *self.lock() = false;
        self.wake.notify_all();
    }

    /// The reader is gone or the session is over: the stream ends.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        let _guard = self.lock();
        self.wake.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Blocks while paused; `true` if it was paused (the timeline starts again).
    fn wait_while_paused(&self) -> bool {
        let mut paused = self.lock();
        let was = *paused;
        while *paused && !self.is_closed() {
            paused = self.wake.wait(paused).unwrap_or_else(|e| e.into_inner());
        }
        was
    }
}

pub struct Options {
    pub fps: f64,
    pub lead_seconds: f64,
}

/// Writes the stream until the writer fails or `control` closes; returns the
/// output frames written. A resumed stream starts its timeline (and lead)
/// again from the frames already sent.
pub fn stream_frames(source: &dyn Source, out: &mut dyn Write, options: &Options, control: &Control) -> u64 {
    let frame_s = 1.0 / options.fps;
    let lead = (options.lead_seconds * options.fps).round().max(0.0) as u64;
    let mut started = Instant::now();
    let mut sent: u64 = 0;
    let mut sent_seq: Option<u64> = None;

    // Moment of output frame `n`: the first `lead` go out at once.
    let frame_at = |started: Instant, n: u64| {
        let at = started + Duration::from_secs_f64(n.saturating_sub(lead) as f64 * frame_s);
        source.frame_at(at.checked_sub(SAMPLE_DELAY).unwrap_or(at))
    };

    while !control.is_closed() {
        if control.wait_while_paused() {
            started = Instant::now() - Duration::from_secs_f64(sent.saturating_sub(lead) as f64 * frame_s);
            continue;
        }

        let due = (started.elapsed().as_secs_f64() / frame_s).floor() as u64 + 1 + lead;
        while sent < due {
            let frame = frame_at(started, sent);
            let written = if sent_seq != Some(frame.seq) {
                sent_seq = Some(frame.seq);
                sent += 1;
                out.write_all(frame.packet())
            } else {
                let mut frames = 1;
                while sent + frames < due && frame_at(started, sent + frames).seq == frame.seq {
                    frames += 1;
                }
                sent += frames;
                out.write_all(&repeat_packet(frames))
            };
            if written.and_then(|()| out.flush()).is_err() {
                control.close();
                return sent;
            }
        }
        std::thread::sleep(TICK);
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_frames_at_once_then_paced() {
        let control = Control::new(false);
        let mut out = Vec::new();
        let closer = Arc::clone(&control);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(210));
            closer.close();
        });
        let sent = stream_frames(&Empty::new(), &mut out, &Options { fps: 25.0, lead_seconds: 0.2 }, &control);
        timer.join().unwrap();
        // 5 lead frames, then one per 40 ms for ~210 ms.
        assert!((10..=13).contains(&sent), "sent {sent}");
        // One Clear, then only Repeats.
        assert_eq!(&out[6..7], &[2]);
        assert!(out[11..].chunks(15).all(|p| p[6] == 1));
    }

    #[test]
    fn a_paused_stream_writes_nothing_until_resumed() {
        let control = Control::new(true);
        let resumer = Arc::clone(&control);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            resumer.resume();
            std::thread::sleep(Duration::from_millis(10));
            resumer.close();
        });
        let mut out = Vec::new();
        let sent = stream_frames(&Empty::new(), &mut out, &Options { fps: 25.0, lead_seconds: 0.2 }, &control);
        timer.join().unwrap();
        // The lead again from the resume, not the 100 ms held.
        assert_eq!(sent, 6);
    }
}
