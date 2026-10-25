// HTTP side of the sidecar.
//
//   POST /etv?channel=1[&render=720p]
//                         body: ErsatzTV template data (JSON from the script
//                         element's stdin); reply: endless ETV packet stream
//   GET  /ffmpeg?channel=1[&wait=2000]
//                         the ffmpeg overlay (client/vizrs-ffmpeg.py) takes over
//                         a script session of the channel: its packets
//                         stop, the reply gives the session, size and rate
//   GET  /ffmpeg/frames?session=N[&fps=25/1]
//                         raw yuva420p frames of that session for ffmpeg
//   GET  /health          captures and sessions

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crate::capture::{Capture, CaptureOptions, IDLE, find_browser};
use crate::config::Config;
use crate::frames::{Empty, Source};
use crate::http::{self, Request};
use crate::json::{self, Value};
use crate::scale::render_size;
use crate::stream::{self, Control, Format, Options};
use crate::template::{AdMatcher, ClipContext, js_string};
use crate::vizrs::ChannelEvents;

pub const MAX_BODY: usize = 4 * 1024 * 1024;
// A channel served by the ffmpeg overlay holds its script packets this long
// for the overlay to take the session over; then ErsatzTV draws them itself.
const HOLD: Duration = Duration::from_millis(3000);
// A session taken over waits this long for the overlay's frame stream.
const FRAMES_WAIT: Duration = Duration::from_millis(10000);
const MAX_WAIT_MS: f64 = 10000.0;

/// `Number(text)` of JavaScript for the rates and waits in a query.
fn js_number(text: &str) -> f64 {
    let text = text.trim();
    if text.is_empty() { 0.0 } else { text.parse().unwrap_or(f64::NAN) }
}

/// "25", "25/1" or "30000/1001" as frames per second; 0 if not a rate.
pub fn parse_rate(value: Option<&str>) -> f64 {
    let mut parts = value.unwrap_or("").split('/');
    let num = js_number(parts.next().unwrap_or(""));
    let den = parts.next().map_or(1.0, js_number);
    let rate = num / den;
    if rate.is_finite() && rate > 0.0 { rate } else { 0.0 }
}

/// A delayed action that can be called off (`clearTimeout`).
struct Timer(Arc<AtomicBool>);

impl Timer {
    fn after(delay: Duration, action: impl FnOnce() + Send + 'static) -> Timer {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let _ = thread::Builder::new().name("timer".into()).spawn(move || {
            thread::sleep(delay);
            if !flag.load(Ordering::Relaxed) {
                action();
            }
        });
        Timer(cancelled)
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Graphics of a session: a capture, or transparent frames (burn-in off).
#[derive(Clone)]
enum Graphics {
    Empty,
    Capture(Arc<Capture>),
}

/// A script element session, for the ffmpeg overlay to take over.
struct Script {
    id: u64,
    channel: String,
    ctx: ClipContext,
    fps: f64,
    capture: Option<Arc<Capture>>,
    /// The packet stream to ErsatzTV: paused while taken over, closed when
    /// the overlay's frame stream ends.
    stream: Arc<Control>,
    claimed: AtomicBool,
    frames: AtomicBool,
    hold: Mutex<Option<Timer>>,
    claim_timer: Mutex<Option<Timer>>,
}

fn cancel(slot: &Mutex<Option<Timer>>) {
    if let Some(timer) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
        timer.cancel();
    }
}

/// A `/ffmpeg` request waiting for a script session of its channel.
type Waiter = (u64, Sender<Arc<Script>>);

#[derive(Default)]
struct Sessions {
    captures: Vec<(String, Arc<Capture>)>,
    scripts: BTreeMap<u64, Arc<Script>>,
    waiters: HashMap<String, Vec<Waiter>>,
    /// Channels whose ffmpeg runs through the overlay: their sessions start held.
    overlay: Vec<String>,
    next_id: u64,
    next_waiter: u64,
}

type Shared = Arc<Mutex<Sessions>>;

fn lock(sessions: &Shared) -> MutexGuard<'_, Sessions> {
    sessions.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Server {
    pub config: Arc<Config>,
    ads: Arc<AdMatcher>,
    browser: Option<String>,
    channels: Mutex<HashMap<String, Arc<ChannelEvents>>>,
    sessions: Shared,
    count: AtomicUsize,
    empty: Empty,
}

impl Server {
    pub fn new(config: Config) -> Arc<Server> {
        let (ads, warnings) = AdMatcher::new(&config.ad_match);
        for warning in warnings {
            crate::warn!("{warning}");
        }
        let browser = if config.browser.is_empty() { find_browser() } else { Some(config.browser.clone()) };
        Arc::new(Server {
            config: Arc::new(config),
            ads: Arc::new(ads),
            browser,
            channels: Mutex::new(HashMap::new()),
            sessions: Shared::default(),
            count: AtomicUsize::new(0),
            empty: Empty::new(),
        })
    }

    fn events_for(&self, channel: &str) -> Arc<ChannelEvents> {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let events =
            channels.entry(channel.to_owned()).or_insert_with(|| ChannelEvents::new(Arc::clone(&self.config), Arc::clone(&self.ads)));
        Arc::clone(events)
    }

    /// One capture per channel size and render size, shared by the channels that have both.
    fn capture_for(&self, width: u32, height: u32, fps: f64, spec: &str) -> Graphics {
        if self.config.output_url.is_empty() || width == 0 || height == 0 {
            return Graphics::Empty;
        }
        let (rw, rh) = render_size(spec, width, height).unwrap_or_else(|error| {
            crate::warn!("render size: {error}; using {width}x{height}");
            (width, height)
        });
        let key = format!("{width}x{height}/{rw}x{rh}");
        let mut sessions = lock(&self.sessions);
        if let Some((_, capture)) = sessions.captures.iter().find(|(k, _)| *k == key) {
            return Graphics::Capture(Arc::clone(capture));
        }
        let capture = Capture::start(CaptureOptions {
            url: self.config.output_url.clone(),
            width: rw,
            height: rh,
            out_width: width,
            out_height: height,
            fps,
            max_fps: self.config.max_fps,
            browser: self.browser.clone(),
            no_sandbox: self.config.no_sandbox,
            extra_args: self.config.browser_args.clone(),
            idle: IDLE,
            history: stream::history(self.config.lead_seconds),
        });
        sessions.captures.push((key, Arc::clone(&capture)));
        Graphics::Capture(capture)
    }

    /// `/health`: script sessions, captures, channels served by the ffmpeg overlay.
    pub fn health(&self) -> Value {
        let sessions = lock(&self.sessions);
        let captures = sessions
            .captures
            .iter()
            .map(|(_, c)| {
                json::object([
                    ("size", c.size().into()),
                    ("render", c.render().into()),
                    ("fps", c.fps().into()),
                    ("status", c.status().into()),
                    ("seq", c.current().seq.into()),
                ])
            })
            .collect();
        json::object([
            ("sessions", (self.count.load(Ordering::Relaxed) as u64).into()),
            ("captures", Value::Array(captures)),
            ("overlay", sessions.overlay.iter().map(String::as_str).collect::<Vec<_>>().into()),
        ])
    }

    /// Kills every browser (the sidecar stops).
    pub fn stop_captures(&self) {
        for (_, capture) in &lock(&self.sessions).captures {
            capture.stop();
        }
    }

    pub fn handle(&self, request: Request, mut stream: TcpStream) {
        let result = match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/health") => http::respond(&mut stream, 200, Some("application/json"), self.health().to_string().as_bytes()),
            ("GET", "/ffmpeg") => self.claim_request(&request, stream),
            ("GET", "/ffmpeg/frames") => {
                self.frames_request(&request, stream);
                Ok(())
            }
            ("POST", "/etv") => {
                self.script_session(&request, stream);
                Ok(())
            }
            _ => http::respond(&mut stream, 404, None, b""),
        };
        if let Err(error) = result {
            crate::debug!("http {} {}: {error}", request.method, request.path);
        }
    }

    fn free(sessions: &Sessions, channel: &str) -> Option<Arc<Script>> {
        sessions.scripts.values().find(|s| s.channel == channel && !s.claimed.load(Ordering::Relaxed) && s.capture.is_some()).cloned()
    }

    /// The overlay takes the session: ErsatzTV gets no more packets from it.
    fn claim(sessions: &mut Sessions, script: &Arc<Script>) {
        script.claimed.store(true, Ordering::Relaxed);
        cancel(&script.hold);
        script.stream.pause();
        if !sessions.overlay.contains(&script.channel) {
            sessions.overlay.push(script.channel.clone());
        }
        // No frame stream after all (the overlay gave up): ErsatzTV draws again.
        let back = Arc::clone(script);
        let timer = Timer::after(FRAMES_WAIT, move || {
            back.claimed.store(false, Ordering::Relaxed);
            back.stream.resume();
        });
        *script.claim_timer.lock().unwrap_or_else(|e| e.into_inner()) = Some(timer);
    }

    fn wait_for_script(&self, channel: &str, wait: Duration) -> Option<Arc<Script>> {
        let (id, receiver) = {
            let mut sessions = lock(&self.sessions);
            if let Some(script) = Self::free(&sessions, channel) {
                Self::claim(&mut sessions, &script);
                return Some(script);
            }
            let (sender, receiver) = mpsc::channel();
            sessions.next_waiter += 1;
            let id = sessions.next_waiter;
            sessions.waiters.entry(channel.to_owned()).or_default().push((id, sender));
            (id, receiver)
        };
        if let Ok(script) = receiver.recv_timeout(wait) {
            return Some(script);
        }
        let mut sessions = lock(&self.sessions);
        if let Some(list) = sessions.waiters.get_mut(channel) {
            list.retain(|(w, _)| *w != id);
        }
        // A session may have been handed over just as the wait ran out.
        receiver.try_recv().ok()
    }

    fn register(&self, script: &Arc<Script>) {
        let mut sessions = lock(&self.sessions);
        sessions.scripts.insert(script.id, Arc::clone(script));
        let waiter = sessions.waiters.get_mut(&script.channel).filter(|list| !list.is_empty()).map(|list| list.remove(0));
        if let Some((_, waiter)) = waiter {
            Self::claim(&mut sessions, script);
            let _ = waiter.send(Arc::clone(script));
        }
    }

    fn claim_request(&self, request: &Request, mut stream: TcpStream) -> std::io::Result<()> {
        let channel = request.param("channel").unwrap_or("1").to_owned();
        let wait = js_number(request.param("wait").unwrap_or("2000"));
        let wait = if wait.is_nan() { 0.0 } else { wait.clamp(0.0, MAX_WAIT_MS) };
        let Some(script) = self.wait_for_script(&channel, Duration::from_secs_f64(wait / 1000.0)) else {
            return http::respond(&mut stream, 404, None, b"");
        };

        crate::info!("channel {channel}: ffmpeg overlay took over the graphics of {}", script.ctx.title);
        let reply = json::object([
            ("session", script.id.into()),
            ("width", u64::from(script.ctx.width).into()),
            ("height", u64::from(script.ctx.height).into()),
            ("fps", script.fps.into()),
        ]);
        http::respond(&mut stream, 200, Some("application/json"), reply.to_string().as_bytes())
    }

    fn frames_request(&self, request: &Request, mut stream: TcpStream) {
        let id = js_number(request.param("session").unwrap_or(""));
        let script = lock(&self.sessions).scripts.get(&(id as u64)).filter(|_| id.fract() == 0.0).cloned();
        let taken = script.filter(|s| s.claimed.load(Ordering::Relaxed) && !s.frames.swap(true, Ordering::Relaxed));
        let Some((script, capture)) = taken.and_then(|s| s.capture.clone().map(|c| (s, c))) else {
            let _ = http::respond(&mut stream, 404, None, b"");
            return;
        };

        cancel(&script.claim_timer);
        let fps = match parse_rate(request.param("fps")) {
            0.0 => script.fps,
            fps => fps,
        };
        capture.raise_fps(fps);
        capture.acquire();
        let control = Control::new(false);
        watch_close(&stream, &control);
        if let Ok(mut out) = http::Stream::start(stream, request) {
            let format = Format::Yuva { width: script.ctx.width, height: script.ctx.height };
            stream::stream_frames(
                &*capture,
                &mut out,
                &Options { fps, lead_seconds: self.config.lead_seconds, format, air: script.ctx.air_ms() },
                &control,
            );
        }
        capture.release();
        // The transcode is over: the script ends with it, as without the overlay.
        script.stream.close();
    }

    /// A script element session: commands in the background, frames at once,
    /// for as long as ErsatzTV reads them.
    fn script_session(&self, request: &Request, mut stream: TcpStream) {
        let body = String::from_utf8_lossy(&request.body);
        let data = match json::parse(if body.trim().is_empty() { "{}" } else { &body }) {
            Ok(data) => data,
            Err(error) => {
                let _ = http::respond(&mut stream, 400, None, format!("bad template data: {error}").as_bytes());
                return;
            }
        };
        let ctx = ClipContext::from_data(&data);
        let channel = request.param("channel").unwrap_or("1").to_owned();
        let fps = if ctx.fps > 0.0 { ctx.fps } else { 25.0 };
        crate::info!(
            "channel {channel}: {} ({}x{}@{}, seek {:.1} s)",
            ctx.title,
            ctx.width,
            ctx.height,
            js_string(&Value::Number(fps)),
            ctx.seek
        );

        // Commands go out in the background; frames start at once.
        let events = self.events_for(&channel);
        events.session(&ctx);

        let spec = request.param("render").unwrap_or(&self.config.render_size).to_owned();
        let capture = match self.capture_for(ctx.width, ctx.height, fps, &spec) {
            Graphics::Capture(capture) => Some(capture),
            Graphics::Empty => None,
        };
        if let Some(capture) = &capture {
            capture.acquire();
        }

        let held = capture.is_some() && lock(&self.sessions).overlay.contains(&channel);
        let control = Control::new(held);
        watch_close(&stream, &control);
        // A held session writes nothing at first; the client must see the reply at once.
        if let Ok(mut out) = http::Stream::start(stream, request) {
            self.count.fetch_add(1, Ordering::Relaxed);
            let script = {
                let mut sessions = lock(&self.sessions);
                sessions.next_id += 1;
                Arc::new(Script {
                    id: sessions.next_id,
                    channel: channel.clone(),
                    ctx: ctx.clone(),
                    fps,
                    capture: capture.clone(),
                    stream: Arc::clone(&control),
                    claimed: AtomicBool::new(false),
                    frames: AtomicBool::new(false),
                    hold: Mutex::new(None),
                    claim_timer: Mutex::new(None),
                })
            };
            if held {
                let (sessions, resume, channel) = (Arc::clone(&self.sessions), Arc::clone(&control), channel.clone());
                let timer = Timer::after(HOLD, move || {
                    lock(&sessions).overlay.retain(|c| *c != channel);
                    crate::warn!("channel {channel}: no ffmpeg overlay; ErsatzTV draws the graphics");
                    resume.resume();
                });
                *script.hold.lock().unwrap_or_else(|e| e.into_inner()) = Some(timer);
            }
            self.register(&script);

            let source: &dyn Source = match &capture {
                Some(capture) => &**capture,
                None => &self.empty,
            };
            let options = Options { fps, lead_seconds: self.config.lead_seconds, format: Format::Packets, air: ctx.air_ms() };
            stream::stream_frames(source, &mut out, &options, &control);
            let _ = out.finish();

            self.count.fetch_sub(1, Ordering::Relaxed);
            lock(&self.sessions).scripts.remove(&script.id);
            cancel(&script.hold);
            cancel(&script.claim_timer);
        }
        if let Some(capture) = &capture {
            capture.release();
        }
        events.ended(&ctx);
    }
}

/// Closes `control` once the client hangs up: a stream that writes nothing
/// (held for the ffmpeg overlay) would not notice otherwise.
fn watch_close(stream: &TcpStream, control: &Arc<Control>) {
    let Ok(mut reader) = stream.try_clone() else { return };
    let control = Arc::clone(control);
    let _ = thread::Builder::new().name("http-close".into()).spawn(move || {
        let mut buffer = [0; 512];
        while matches!(reader.read(&mut buffer), Ok(n) if n > 0) {}
        control.close();
    });
}
