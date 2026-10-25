// Frame stream of one session: ETV packets for an ErsatzTV script element, or
// raw yuva420p frames for ffmpeg (the ffmpeg overlay). The reader takes one frame per
// output frame; the stream is paced by the clock with a lead, so it follows
// `-readrate 1.0` with its initial burst but never runs far ahead of the live
// VizRS graphics. Every output frame takes the picture that was current at
// its own moment.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::frames::Source;
use crate::packets::repeat_packet;

const TICK: Duration = Duration::from_millis(20);
/// Pictures are dated when Chromium drew them and arrive decoded up to ~100 ms
/// later; output frames look this far back so each finds the picture of its moment.
pub const SAMPLE_DELAY: Duration = Duration::from_millis(100);

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

/// What a session reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    /// ETV packets: a picture once, then one `Repeat` for any run of the same picture.
    Packets,
    /// Raw yuva420p of `width`x`height`: every output frame carries the whole picture.
    Yuva { width: u32, height: u32 },
}

pub struct Options {
    pub fps: f64,
    pub lead_seconds: f64,
    pub format: Format,
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
            let written = if let Format::Yuva { width, height } = options.format {
                sent += 1;
                out.write_all(&frame.yuva(width, height))
            } else if sent_seq != Some(frame.seq) {
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
    use crate::frames::{Empty, Frame, History};
    use crate::packets::Rect;

    const PACKETS: Format = Format::Packets;

    #[test]
    fn lead_frames_at_once_then_paced() {
        let control = Control::new(false);
        let mut out = Vec::new();
        let closer = Arc::clone(&control);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(210));
            closer.close();
        });
        let sent = stream_frames(&Empty::new(), &mut out, &Options { fps: 25.0, lead_seconds: 0.2, format: PACKETS }, &control);
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
        let sent = stream_frames(&Empty::new(), &mut out, &Options { fps: 25.0, lead_seconds: 0.2, format: PACKETS }, &control);
        timer.join().unwrap();
        // The lead again from the resume, not the 100 ms held.
        assert_eq!(sent, 6);
    }

    #[test]
    fn raw_stream_writes_the_whole_picture_on_every_output_frame() {
        struct Fixed(Arc<Frame>);
        impl Source for Fixed {
            fn frame_at(&self, _: Instant) -> Arc<Frame> {
                Arc::clone(&self.0)
            }
        }
        let history = Fixed(Arc::new(Frame::new(1, vec![Rect { x: 1, y: 0, w: 1, h: 1, pixels: vec![0, 0, 255, 255] }], Instant::now())));
        let control = Control::new(false);
        let closer = Arc::clone(&control);
        let timer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            closer.close();
        });
        let mut out = Vec::new();
        let sent = stream_frames(
            &history,
            &mut out,
            &Options { fps: 25.0, lead_seconds: 0.2, format: Format::Yuva { width: 4, height: 2 } },
            &control,
        );
        timer.join().unwrap();
        assert_eq!(sent, 6);
        assert_eq!(out.len(), 6 * 20);
        assert!(out.chunks(20).all(|frame| frame[12 + 1] == 255));
    }

    /// Chromium draws every 40 ms; each picture arrives decoded 0–85 ms later.
    /// Every output frame still shows its own picture: steps of exactly one.
    #[test]
    fn pictures_decoded_with_varying_delay_each_land_in_their_own_output_frame() {
        let history = Arc::new(History::new());
        let control = Control::new(false);
        let (feeder, closer) = (Arc::clone(&history), Arc::clone(&control));
        let started = Instant::now();
        let deliver = std::thread::spawn(move || {
            let mut pending: Vec<(Instant, u8, Instant)> = Vec::new();
            let mut drawn = 0u64;
            let mut seed = 0x2545_f491_4f6c_dd1du64;
            while started.elapsed() < Duration::from_millis(1500) {
                let now = Instant::now();
                while started + Duration::from_millis(10 + drawn * 40) <= now {
                    let at = started + Duration::from_millis(10 + drawn * 40);
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    pending.push((at, (drawn % 250) as u8 + 1, at + Duration::from_millis(seed % 85)));
                    drawn += 1;
                }
                while pending.first().is_some_and(|p| p.2 <= now) {
                    let (at, value, _) = pending.remove(0);
                    feeder.set(vec![Rect { x: 0, y: 0, w: 1, h: 1, pixels: vec![value; 4] }], at);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            closer.close();
        });
        let mut out = Vec::new();
        stream_frames(&*history, &mut out, &Options { fps: 25.0, lead_seconds: 0.0, format: PACKETS }, &control);
        deliver.join().unwrap();

        // Pixel value of every output frame, as ErsatzTV draws them.
        let mut values = Vec::new();
        let (mut offset, mut value) = (0, 0u8);
        while offset + 11 <= out.len() {
            let length = u32::from_be_bytes(out[offset + 7..offset + 11].try_into().unwrap()) as usize;
            let payload = &out[offset + 11..offset + 11 + length];
            let repeat = match out[offset + 6] {
                1 => u32::from_be_bytes(payload.try_into().unwrap()) as usize,
                2 => {
                    value = 0;
                    0
                }
                _ => {
                    value = payload[10];
                    0
                }
            };
            values.extend(std::iter::repeat_n(value, repeat + 1));
            offset += 11 + length;
        }
        let values: Vec<u8> = values.into_iter().filter(|&v| v > 0).collect();
        let steps: Vec<i32> = values.windows(2).map(|w| i32::from(w[1]) - i32::from(w[0])).collect();
        let exact = steps.iter().filter(|&&s| s == 1).count();
        assert!(exact + 1 >= steps.len(), "steps: {steps:?}");
    }
}
