// Real headless Chromium on a local transparent page; skipped without a
// browser (VIZRS_BROWSER or PATH).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use virtrs_cg::capture::{Capture, CaptureMode, CaptureOptions, find_browser};
use virtrs_cg::frames::Source;

/// Serves `page` to every request; returns its URL.
fn serve(page: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
        }
    });
    url
}

/// One browser at a time: cold starts of several at once on a small CI runner
/// take longer than the first DevTools call may wait.
fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn capture(url: String, render: (u32, u32), out: (u32, u32), idle: Duration) -> Option<Arc<Capture>> {
    capture_in(CaptureMode::Screencast, url, render, out, idle)
}

fn capture_in(mode: CaptureMode, url: String, render: (u32, u32), out: (u32, u32), idle: Duration) -> Option<Arc<Capture>> {
    let Some(browser) = find_browser() else {
        eprintln!("no Chromium: skipped");
        return None;
    };
    // Without the sandbox where the host forbids it (CI runners, containers).
    let no_sandbox = std::env::var("VIZRS_BROWSER_NO_SANDBOX").as_deref() == Ok("1");
    Some(Capture::start(CaptureOptions {
        url,
        width: render.0,
        height: render.1,
        out_width: out.0,
        out_height: out.1,
        fps: 25.0,
        max_fps: 0.0,
        mode,
        browser: Some(browser),
        no_sandbox,
        extra_args: Vec::new(),
        idle,
        history: Duration::from_secs(5),
    }))
}

fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while !check() {
        assert!(Instant::now() < deadline, "timeout: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

const PAGE: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:40px;top:300px;width:400px;height:60px;background:rgb(255,0,0)"></div>
</body></html>"#;

fn boxes(capture: &Capture) -> Vec<(u32, u32, u32, u32)> {
    capture.current().picture().rects.iter().map(|r| (r.x, r.y, r.w, r.h)).collect()
}

#[test]
fn chromium_capture_gives_the_visible_box_of_a_transparent_page() {
    let _serial = serial();
    let Some(capture) = capture(serve(PAGE), (640, 360), (640, 360), Duration::from_secs(30)) else { return };
    // A cold browser may first send a half-painted frame; the picture must settle on the page.
    until("the red box", || boxes(&capture) == [(40, 300, 400, 60)]);
    assert_eq!(&capture.current().picture().rects[0].pixels[..4], &[255, 0, 0, 255]);
    capture.stop();
}

#[test]
fn chromium_capture_pauses_without_sessions_and_resumes_with_a_fresh_frame() {
    let _serial = serial();
    let Some(capture) = capture(serve(PAGE), (640, 360), (640, 360), Duration::from_millis(200)) else { return };
    capture.acquire();
    until("a first frame", || !capture.current().picture().rects.is_empty());
    capture.release();
    until("paused", || capture.status() == "paused");

    // A static page still sends a frame when the screencast starts again.
    let seq = capture.current().seq;
    capture.acquire();
    assert_eq!(capture.status(), "capturing");
    until("a fresh frame", || capture.current().seq > seq);
    assert_eq!(capture.current().picture().rects.len(), 1);
    capture.stop();
}

#[test]
fn chromium_capture_renders_smaller_and_scales_to_the_channel() {
    let _serial = serial();
    // Proportional layout, like VizRS scaling its 1920x1080 stage to the viewport.
    const SCALED: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:10%;top:80%;width:50%;height:10%;background:rgb(255,0,0)"></div></body></html>"#;
    let Some(capture) = capture(serve(SCALED), (320, 180), (640, 360), Duration::from_secs(30)) else { return };
    until("the scaled box", || !capture.current().picture().rects.is_empty());

    // Red box 32,144 160x18 at 320x180, plus 4 px margin, doubled.
    let frame = capture.current().picture();
    let rect = &frame.rects[0];
    assert_eq!((rect.x, rect.y, rect.w, rect.h), (56, 280, 336, 52));
    let middle = (((rect.h >> 1) * rect.w + (rect.w >> 1)) * 4) as usize;
    assert_eq!(&rect.pixels[middle..middle + 4], &[255, 0, 0, 255]);
    assert_eq!(capture.key(), "640x360 (render 320x180)");
    capture.stop();
}

/// An animation at the channel rate: the screencast takes every second frame
/// at 25 fps (and screenshots the final state after 150 ms of quiet, without
/// holding back the screencast acks), so new pictures keep coming.
#[test]
fn chromium_capture_follows_an_animation_at_the_channel_rate() {
    let _serial = serial();
    const ANIMATED: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<div id="box" style="position:absolute;top:100px;width:80px;height:40px;background:rgb(0,0,255)"></div>
<script>const box = document.getElementById("box");
(function step(t) { box.style.left = (t / 10 % 500) + "px"; requestAnimationFrame(step); })(0);</script>
</body></html>"#;
    let Some(capture) = capture(serve(ANIMATED), (640, 360), (640, 360), Duration::from_secs(30)) else { return };
    until("a first frame", || !capture.current().picture().rects.is_empty());
    thread::sleep(Duration::from_secs(1));
    let seq = capture.current().seq;
    thread::sleep(Duration::from_secs(2));
    let rate = (capture.current().seq - seq) as f64 / 2.0;
    capture.stop();
    // Chromium at 60 Hz / 2 = 30, minus what a busy test host drops.
    assert!(rate >= 12.0, "{rate} pictures per second");
}

/// The frames mode on chromium-headless-shell; another browser has no begin
/// frames and falls back to the screencast, which must show the page as well.
#[test]
fn chromium_frames_capture_shows_the_page_pauses_and_resumes() {
    let _serial = serial();
    let Some(capture) = capture_in(CaptureMode::Frames, serve(PAGE), (640, 360), (640, 360), Duration::from_millis(200)) else { return };
    capture.acquire();
    until("the red box", || boxes(&capture) == [(40, 300, 400, 60)]);
    eprintln!("capture mode: {}", capture.mode().name());
    capture.release();
    until("paused", || capture.status() == "paused");

    let seq = capture.current().seq;
    capture.acquire();
    until("a fresh frame", || capture.current().seq > seq);
    assert_eq!(boxes(&capture), [(40, 300, 400, 60)]);
    capture.stop();
}

/// Begin frames: one picture per frame of the channel, dated on its grid
/// (40 ms at 25 fps), the animation one step further in each.
#[test]
fn chromium_frames_capture_draws_on_the_frame_grid_of_the_channel() {
    let _serial = serial();
    const STEPS: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<div id="box" style="position:absolute;top:100px;width:20px;height:20px;background:rgb(0,0,255)"></div>
<script>const box = document.getElementById("box");
(function step(t) { box.style.left = (Math.floor(t / 40 + 0.001) % 600) + "px"; requestAnimationFrame(step); })(0);</script>
</body></html>"#;
    let Some(capture) = capture_in(CaptureMode::Frames, serve(STEPS), (640, 360), (640, 360), Duration::from_secs(30)) else { return };
    until("a first frame", || !capture.current().picture().rects.is_empty());
    thread::sleep(Duration::from_secs(2));
    if capture.mode() != CaptureMode::Frames {
        eprintln!("no begin frames in this browser: skipped");
        return capture.stop();
    }
    let seq = capture.current().seq;
    thread::sleep(Duration::from_secs(2));
    let end = capture.current();
    let rate = (end.seq - seq) as f64 / 2.0;

    // Back from the newest picture: every picture is on the grid, mostly one step apart.
    let (mut frame, mut steps, mut one) = (end, 0, 0);
    while steps < 40 {
        let before = capture.frame_at(frame.at - Duration::from_micros(1));
        if before.seq == 0 {
            break;
        }
        let gap = (frame.at - before.at).as_secs_f64() * 1000.0;
        let slots = (gap / 40.0).round();
        assert!(slots >= 1.0 && (gap - slots * 40.0).abs() < 0.05, "{gap} ms between pictures");
        let x = |f: &virtrs_cg::frames::Frame| f.picture().rects[0].x;
        if slots == 1.0 && x(&frame) == (x(&before) + 1) % 600 {
            one += 1;
        }
        (frame, steps) = (before, steps + 1);
    }
    capture.stop();
    assert!(rate >= 12.0, "{rate} pictures per second");
    assert!(one * 10 >= steps * 7, "{one} of {steps} pictures one step after the one before");
}
