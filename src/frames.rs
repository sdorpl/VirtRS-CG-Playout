// Pictures of the graphics for the sessions: the current one and a short
// history, so each output frame can take the picture of its own moment
// (`setFrame` / `frameAt` of the Node.js sidecar). A picture is dated when Chromium
// drew it, not when its decoding finished.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::packets::{Rect, picture_packet};
use crate::yuva::yuva_frame;

// Frames kept for sessions that write late (backpressure, a busy host).
const HISTORY: Duration = Duration::from_millis(500);

/// One picture; `seq` grows with every new picture.
#[derive(Debug)]
pub struct Frame {
    pub seq: u64,
    pub rects: Vec<Rect>,
    /// When Chromium drew it.
    pub at: Instant,
    /// Built once, shared by the sessions of the same size.
    packet: OnceLock<Vec<u8>>,
    yuva: OnceLock<(u32, u32, Arc<[u8]>)>,
}

impl Frame {
    pub fn new(seq: u64, rects: Vec<Rect>, at: Instant) -> Frame {
        Frame { seq, rects, at, packet: OnceLock::new(), yuva: OnceLock::new() }
    }

    /// ETV packet: the visible rectangles, or Clear.
    pub fn packet(&self) -> &[u8] {
        self.packet.get_or_init(|| picture_packet(&self.rects))
    }

    /// The picture as one yuva420p frame of `width`x`height`.
    pub fn yuva(&self, width: u32, height: u32) -> Arc<[u8]> {
        let (w, h, data) = self.yuva.get_or_init(|| (width, height, yuva_frame(&self.rects, width, height)));
        if (*w, *h) == (width, height) { Arc::clone(data) } else { yuva_frame(&self.rects, width, height) }
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
        Empty(Arc::new(Frame::new(0, Vec::new(), Instant::now())))
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

/// The current picture and the ones before it, for `HISTORY`.
pub struct History {
    frames: Mutex<VecDeque<Arc<Frame>>>,
}

impl Default for History {
    fn default() -> Self {
        History::new()
    }
}

impl History {
    pub fn new() -> History {
        History { frames: Mutex::new(VecDeque::from([Arc::new(Frame::new(0, Vec::new(), Instant::now()))])) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Arc<Frame>>> {
        self.frames.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn current(&self) -> Arc<Frame> {
        Arc::clone(self.lock().back().expect("history is never empty"))
    }

    /// A new picture drawn at `at`; the history stays in order. Two empty
    /// pictures in a row are one.
    pub fn set(&self, rects: Vec<Rect>, at: Instant) {
        let mut frames = self.lock();
        let current = frames.back().expect("history is never empty");
        if rects.is_empty() && current.rects.is_empty() {
            return;
        }
        let at = at.max(current.at);
        let frame = Arc::new(Frame::new(current.seq + 1, rects, at));
        frames.push_back(frame);
        // Keep the frame that was current when the window starts.
        while frames.len() > 1 && at.checked_sub(HISTORY).is_some_and(|start| frames[1].at <= start) {
            frames.pop_front();
        }
    }
}

impl Source for History {
    /// The picture shown at `time`, like a playout taking the current frame.
    fn frame_at(&self, time: Instant) -> Arc<Frame> {
        let frames = self.lock();
        let found = frames.iter().skip(1).rev().find(|f| f.at <= time).unwrap_or(&frames[0]);
        Arc::clone(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(value: u8) -> Vec<Rect> {
        vec![Rect { x: 0, y: 0, w: 1, h: 1, pixels: vec![value; 4] }]
    }

    #[test]
    fn history_gives_the_picture_of_a_past_moment() {
        let history = History::new();
        let t0 = Instant::now();
        history.set(pixel(1), t0 + Duration::from_millis(10));
        history.set(pixel(2), t0 + Duration::from_millis(50));
        assert_eq!(history.frame_at(t0).seq, 0);
        assert_eq!(history.frame_at(t0 + Duration::from_millis(30)).seq, 1);
        assert_eq!(history.frame_at(t0 + Duration::from_millis(60)).seq, 2);

        // Dated before the current picture: kept in order, not before it.
        history.set(pixel(3), t0);
        assert_eq!(history.current().at, t0 + Duration::from_millis(50));
        assert_eq!(history.frame_at(t0 + Duration::from_millis(60)).seq, 3);

        // Empty after empty is no new picture; old pictures leave the window.
        history.set(Vec::new(), t0 + Duration::from_secs(2));
        history.set(Vec::new(), t0 + Duration::from_secs(3));
        assert_eq!(history.current().seq, 4);
        assert_eq!(history.lock().len(), 2);
    }
}
