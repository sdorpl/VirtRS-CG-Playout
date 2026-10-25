// Real headless Chromium on a local transparent page; skipped without a
// browser (VIZRS_BROWSER or PATH).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use virtrs_cg::capture::{Capture, CaptureOptions, find_browser};

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
        browser: Some(browser),
        no_sandbox,
        extra_args: Vec::new(),
        idle,
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
    capture.current().rects.iter().map(|r| (r.x, r.y, r.w, r.h)).collect()
}

#[test]
fn chromium_capture_gives_the_visible_box_of_a_transparent_page() {
    let _serial = serial();
    let Some(capture) = capture(serve(PAGE), (640, 360), (640, 360), Duration::from_secs(30)) else { return };
    // A cold browser may first send a half-painted frame; the picture must settle on the page.
    until("the red box", || boxes(&capture) == [(40, 300, 400, 60)]);
    assert_eq!(&capture.current().rects[0].pixels[..4], &[255, 0, 0, 255]);
    capture.stop();
}

#[test]
fn chromium_capture_pauses_without_sessions_and_resumes_with_a_fresh_frame() {
    let _serial = serial();
    let Some(capture) = capture(serve(PAGE), (640, 360), (640, 360), Duration::from_millis(200)) else { return };
    capture.acquire();
    until("a first frame", || !capture.current().rects.is_empty());
    capture.release();
    until("paused", || capture.status() == "paused");

    // A static page still sends a frame when the screencast starts again.
    let seq = capture.current().seq;
    capture.acquire();
    assert_eq!(capture.status(), "capturing");
    until("a fresh frame", || capture.current().seq > seq);
    assert_eq!(capture.current().rects.len(), 1);
    capture.stop();
}

#[test]
fn chromium_capture_renders_smaller_and_scales_to_the_channel() {
    let _serial = serial();
    // Proportional layout, like VizRS scaling its 1920x1080 stage to the viewport.
    const SCALED: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:10%;top:80%;width:50%;height:10%;background:rgb(255,0,0)"></div></body></html>"#;
    let Some(capture) = capture(serve(SCALED), (320, 180), (640, 360), Duration::from_secs(30)) else { return };
    until("the scaled box", || !capture.current().rects.is_empty());

    // Red box 32,144 160x18 at 320x180, plus 4 px margin, doubled.
    let frame = capture.current();
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
    until("a first frame", || !capture.current().rects.is_empty());
    thread::sleep(Duration::from_secs(1));
    let seq = capture.current().seq;
    thread::sleep(Duration::from_secs(2));
    let rate = (capture.current().seq - seq) as f64 / 2.0;
    capture.stop();
    // Chromium at 60 Hz / 2 = 30, minus what a busy test host drops.
    assert!(rate >= 12.0, "{rate} pictures per second");
}
