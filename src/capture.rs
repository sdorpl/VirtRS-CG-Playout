// Burn-in source: one headless Chromium per channel size renders the VizRS
// `/output` page with a transparent background. Chromium sends a PNG
// (DevTools `Page.startScreencast`) only when the page changes; it becomes the
// current frame for all sessions, with a history as long as the lead of the
// streams so each output frame can take the picture of its own moment. A PNG
// is decoded only when a session shows it. In the `frames` mode Chromium draws
// only when asked (`HeadlessExperimental.beginFrame`), once per frame of the
// channel, with that frame's time: animations advance one step per output frame.

use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::devtools::{DevTools, Incoming};
use crate::frames::{Decode, Frame, History, Source};
use crate::json::{self, Value};
use crate::packets::{Rect, crop};
use crate::png::decode_png;
use crate::rects::visible_rects;
use crate::scale::scale_rects;
use crate::{base64, sys};

// First start on a cold host (CI runner, small ARM VM) can take 15–20 s.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_DELAY: Duration = Duration::from_secs(3);
// Without sessions (ErsatzTV transcodes on demand) the page stops being
// captured after this long; the gap between two items is much shorter.
pub const IDLE: Duration = Duration::from_secs(30);
// Chromium composites at 60 Hz; the screencast takes every n-th frame.
const BROWSER_FPS: f64 = 60.0;
// With n > 1 a lone change (static page, last frame of an animation) may be
// skipped: after this long without frames a screenshot fetches the final state.
const SETTLE: Duration = Duration::from_millis(150);
// Frames mode: a page standing still is probed for damage without a picture;
// a screenshot this often catches a change the probes missed.
const CHECK_MS: f64 = 1000.0;
// Frames mode: missed slots are counted over this long; above a tenth, a warning.
const MISSED_WINDOW: Duration = Duration::from_secs(60);
const BROWSERS: [&str; 7] =
    ["chromium-headless-shell", "chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome", "headless_shell"];

/// `VIZRS_BROWSER`, or the first Chromium-like browser on `PATH`.
pub fn find_browser() -> Option<String> {
    if let Some(browser) = std::env::var_os("VIZRS_BROWSER").filter(|b| !b.is_empty()) {
        return Some(browser.to_string_lossy().into_owned());
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .flat_map(|dir| BROWSERS.iter().map(move |name| dir.join(name)))
        .find(|candidate| is_executable(candidate))
        .map(|path| path.to_string_lossy().into_owned())
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// How Chromium's pictures are taken (`capture_mode`, DOKUMENTACJA §5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaptureMode {
    /// `Page.startScreencast`: Chromium draws at 60 Hz, every n-th frame is taken.
    Screencast,
    /// `HeadlessExperimental.beginFrame`: one frame per frame of the channel,
    /// at its time (chromium-headless-shell only).
    Frames,
}

impl CaptureMode {
    /// `""` or `screencast`, `frames`.
    pub fn parse(spec: &str) -> Result<CaptureMode, String> {
        match spec.trim().to_ascii_lowercase().as_str() {
            "" | "screencast" => Ok(CaptureMode::Screencast),
            "frames" => Ok(CaptureMode::Frames),
            _ => Err(format!("capture mode \"{spec}\" (expected screencast or frames)")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CaptureMode::Screencast => "screencast",
            CaptureMode::Frames => "frames",
        }
    }
}

/// Screencast step that keeps frames at or above the channel rate: 25 fps takes every second frame.
pub fn every_nth_frame(fps: f64) -> u32 {
    (BROWSER_FPS / fps.max(1.0)).floor().max(1.0) as u32
}

/// The channel rate, at most `max` (0 = no limit).
pub fn capped_fps(fps: f64, max: f64) -> f64 {
    if max > 0.0 { fps.min(max) } else { fps }
}

/// When Chromium drew a screencast frame (`metadata.timestamp`, seconds since
/// the epoch), on the monotonic clock; arrival time if missing or off.
pub fn drawn_at(timestamp: Option<f64>) -> Instant {
    let now = Instant::now();
    let Some(timestamp) = timestamp.filter(|t| t.is_finite()) else { return now };
    let wall = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let ago = wall - timestamp;
    if (0.0..1.0).contains(&ago) { now.checked_sub(Duration::from_secs_f64(ago)).unwrap_or(now) } else { now }
}

/// Removes a browser profile; its helper processes may still write for a
/// moment after the browser is killed, so a few tries (as the Node.js build).
fn remove_profile(profile: &Path) {
    for _ in 0..5 {
        if std::fs::remove_dir_all(profile).is_ok() || !profile.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub struct CaptureOptions {
    pub url: String,
    /// Render size of the page.
    pub width: u32,
    pub height: u32,
    /// Size of the frames (the channel).
    pub out_width: u32,
    pub out_height: u32,
    pub fps: f64,
    /// Capture rate limit (`max_fps`); 0 = the channel rate.
    pub max_fps: f64,
    pub mode: CaptureMode,
    pub browser: Option<String>,
    pub no_sandbox: bool,
    pub extra_args: Vec<String>,
    pub idle: Duration,
    /// How far back sessions take pictures (their lead).
    pub history: Duration,
}

/// What the browser loop is told from other threads.
enum Control {
    DevTools(Incoming),
    /// Start or stop capturing (sessions came or left, another rate).
    Active(bool),
}

struct State {
    status: &'static str,
    users: usize,
    active: bool,
    fps: f64,
    /// Bumped by every acquire and release: an older idle timer does nothing.
    idle_generation: u64,
    control: Option<Sender<Control>>,
    child: Option<Child>,
    profile: Option<PathBuf>,
}

pub struct Capture {
    options: CaptureOptions,
    history: History,
    decode: Arc<Decode>,
    state: Mutex<State>,
    stopped: AtomicBool,
    /// The browser has no begin frames (not chromium-headless-shell): screencast instead.
    no_frames: AtomicBool,
}

static PROFILES: AtomicU64 = AtomicU64::new(0);

impl Capture {
    /// Starts the browser; the capture is active at first.
    pub fn start(options: CaptureOptions) -> Arc<Capture> {
        let fps = options.fps;
        let history = History::new(options.history);
        let decode = decoder(&options);
        let capture = Arc::new(Capture {
            options,
            history,
            decode,
            state: Mutex::new(State {
                status: "starting",
                users: 0,
                active: true,
                fps,
                idle_generation: 0,
                control: None,
                child: None,
                profile: None,
            }),
            stopped: AtomicBool::new(false),
            no_frames: AtomicBool::new(false),
        });
        let browser = Arc::clone(&capture);
        let _ = thread::Builder::new().name("capture".into()).spawn(move || browser.supervise());
        capture
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Channel size, as `/health` reports it.
    pub fn size(&self) -> String {
        format!("{}x{}", self.options.out_width, self.options.out_height)
    }

    /// Render size of the page.
    pub fn render(&self) -> String {
        format!("{}x{}", self.options.width, self.options.height)
    }

    pub fn key(&self) -> String {
        if self.size() == self.render() { self.size() } else { format!("{} (render {})", self.size(), self.render()) }
    }

    pub fn status(&self) -> &'static str {
        self.state().status
    }

    pub fn fps(&self) -> f64 {
        self.state().fps
    }

    /// The mode in use: `frames` falls back to the screencast on a browser without begin frames.
    pub fn mode(&self) -> CaptureMode {
        if self.no_frames.load(Ordering::Relaxed) { CaptureMode::Screencast } else { self.options.mode }
    }

    /// Frames per second Chromium draws in the frames mode, and at most takes in the screencast.
    fn capture_fps(&self) -> f64 {
        capped_fps(self.fps(), self.options.max_fps).max(1.0)
    }

    pub fn current(&self) -> Arc<Frame> {
        self.history.current()
    }

    /// A session starts reading frames.
    pub fn acquire(&self) {
        let mut state = self.state();
        state.users += 1;
        state.idle_generation += 1;
        if !state.active {
            self.set_active(&mut state, true);
        }
    }

    /// The last session left: pause the capture once idle for `idle`.
    pub fn release(self: &Arc<Self>) {
        let generation = {
            let mut state = self.state();
            state.users = state.users.saturating_sub(1);
            if state.users > 0 {
                return;
            }
            state.idle_generation += 1;
            state.idle_generation
        };
        let capture = Arc::clone(self);
        let _ = thread::Builder::new().name("capture-idle".into()).spawn(move || {
            thread::sleep(capture.options.idle);
            let mut state = capture.state();
            if state.idle_generation == generation && state.users == 0 && state.active {
                capture.set_active(&mut state, false);
            }
        });
    }

    fn set_active(&self, state: &mut State, active: bool) {
        state.active = active;
        crate::info!("burn-in {}: {}", self.key(), if active { "resumed" } else { "paused, no sessions" });
        if let Some(control) = &state.control {
            state.status = if active { "capturing" } else { "paused" };
            let _ = control.send(Control::Active(active));
        }
    }

    /// A session needs more frames (the ffmpeg overlay at 50 fps): capture every n-th frame
    /// for it, or draw at its rate in the frames mode.
    pub fn raise_fps(&self, fps: f64) {
        let mode = self.mode();
        let mut state = self.state();
        if fps <= state.fps {
            return;
        }
        let max = self.options.max_fps;
        let (before, after) = (capped_fps(state.fps, max), capped_fps(fps, max));
        state.fps = fps;
        let same = match mode {
            CaptureMode::Screencast => every_nth_frame(before) == every_nth_frame(after),
            CaptureMode::Frames => before == after,
        };
        if same {
            return;
        }
        crate::info!("burn-in {}: capturing for {} fps", self.key(), Value::Number(fps));
        if state.active {
            // A new step needs a new screencast, a new rate a new frame grid.
            if let Some(control) = &state.control {
                let _ = control.send(Control::Active(true));
            }
        }
    }

    /// Kills the browser and removes its profile; the capture does not restart.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let mut state = self.state();
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(profile) = state.profile.take() {
            remove_profile(&profile);
        }
    }

    fn supervise(self: Arc<Self>) {
        while !self.stopped.load(Ordering::Relaxed) {
            if let Err(error) = self.run_browser() {
                if !self.stopped.load(Ordering::Relaxed) {
                    crate::error!("burn-in {}: {error}", self.key());
                }
            }
            if self.stopped.load(Ordering::Relaxed) {
                break;
            }
            // No graphics until the browser is back; playout continues.
            self.history.set(Vec::new(), Instant::now());
            self.state().status = "restarting";
            thread::sleep(RESTART_DELAY);
        }
    }

    fn browser_command(&self, browser: &str, profile: &Path, mode: CaptureMode) -> Command {
        let o = &self.options;
        let mut command = Command::new(browser);
        command
            .args([
                "--headless=new",
                "--disable-gpu",
                "--remote-debugging-pipe",
                "--hide-scrollbars",
                "--mute-audio",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-background-timer-throttling",
                "--disable-renderer-backgrounding",
                "--disable-backgrounding-occluded-windows",
                "--force-device-scale-factor=1",
                // Less background work next to the encoder (CPU-only hosts such as Ampere A1).
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-default-apps",
                "--disable-sync",
                "--metrics-recording-only",
                "--no-pings",
                "--renderer-process-limit=1",
                "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--window-size={},{}", o.width, o.height));
        if o.no_sandbox {
            command.arg("--no-sandbox");
        }
        if mode == CaptureMode::Frames {
            // Not --disable-new-content-rendering-timeout: with it every frame reports damage.
            command.args(["--enable-begin-frame-control", "--run-all-compositor-stages-before-draw"]);
        }
        // The page opens in the startup tab: a tab created later over DevTools
        // stays hidden in headless mode and never paints.
        command.args(&o.extra_args).arg(&o.url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        command
    }

    fn run_browser(self: &Arc<Self>) -> Result<(), String> {
        let browser = self.options.browser.clone().ok_or("no Chromium found (set VIZRS_BROWSER)")?;
        let profile = std::env::temp_dir().join(format!("virtrs-cg-{}-{}", std::process::id(), PROFILES.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&profile).map_err(|e| format!("profile {}: {e}", profile.display()))?;

        // Chromium reads commands on fd 3 and writes replies and events on fd 4.
        let (commands_read, commands_write) = std::io::pipe().map_err(|e| e.to_string())?;
        let (events_read, events_write) = std::io::pipe().map_err(|e| e.to_string())?;
        let mode = self.mode();
        let mut command = self.browser_command(&browser, &profile, mode);
        let fds = [(commands_read.as_raw_fd(), 3), (events_write.as_raw_fd(), 4)];
        // SAFETY: child_setup only makes async-signal-safe calls.
        unsafe {
            command.pre_exec(move || sys::child_setup(&fds));
        }
        let spawned = command.spawn();
        // The child has its copies; ours would keep the pipes open after it dies.
        drop(commands_read);
        drop(events_write);
        let child = match spawned {
            Ok(child) => child,
            Err(e) => {
                remove_profile(&profile);
                return Err(format!("start {browser}: {e}"));
            }
        };

        let (control, incoming) = mpsc::channel();
        {
            let mut state = self.state();
            if self.stopped.load(Ordering::Relaxed) {
                drop(state);
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                remove_profile(&profile);
                return Ok(());
            }
            state.child = Some(child);
            state.profile = Some(profile);
            state.control = Some(control.clone());
        }

        let (events, devtools_events) = mpsc::channel();
        let devtools = DevTools::start(events_read, commands_write, events);
        let forward = control.clone();
        let _ = thread::Builder::new().name("devtools-events".into()).spawn(move || {
            for event in devtools_events {
                if forward.send(Control::DevTools(event)).is_err() {
                    break;
                }
            }
        });

        let result = self.session(&devtools, &incoming, mode);

        let mut state = self.state();
        state.control = None;
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(profile) = state.profile.take() {
            remove_profile(&profile);
        }
        result
    }

    /// The startup tab once it shows the page (it starts as `about:blank`).
    fn page_target(&self, devtools: &DevTools) -> Result<String, String> {
        let deadline = Instant::now() + START_TIMEOUT;
        while Instant::now() < deadline {
            let targets = devtools.call("Target.getTargets", json::object([]), None, START_TIMEOUT)?;
            let page = targets.get("targetInfos").and_then(Value::as_array).and_then(|infos| {
                infos.iter().find(|info| {
                    info.get("type").and_then(Value::as_str) == Some("page")
                        && info.get("url").and_then(Value::as_str).is_some_and(|url| url.starts_with("http"))
                })
            });
            if let Some(id) = page.and_then(|info| info.get("targetId")).and_then(Value::as_str) {
                return Ok(id.to_owned());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err("page did not open".into())
    }

    fn session(self: &Arc<Self>, devtools: &Arc<DevTools>, incoming: &mpsc::Receiver<Control>, mode: CaptureMode) -> Result<(), String> {
        let target = self.page_target(devtools)?;
        let attached = devtools.call(
            "Target.attachToTarget",
            json::object([("targetId", target.into()), ("flatten", true.into())]),
            None,
            CALL_TIMEOUT,
        )?;
        let session = attached.get("sessionId").and_then(Value::as_str).ok_or("no sessionId")?.to_owned();
        let call = |method: &str, params: Value| devtools.call(method, params, Some(&session), CALL_TIMEOUT);
        let (width, height) = (u64::from(self.options.width), u64::from(self.options.height));

        call(
            "Emulation.setDeviceMetricsOverride",
            json::object([
                ("width", width.into()),
                ("height", height.into()),
                ("deviceScaleFactor", 1u64.into()),
                ("mobile", false.into()),
            ]),
        )?;
        // Transparency only through DevTools: `--default-background-color` crashes current renderers.
        let transparent = json::object([("r", 0u64.into()), ("g", 0u64.into()), ("b", 0u64.into()), ("a", 0u64.into())]);
        call("Emulation.setDefaultBackgroundColorOverride", json::object([("color", transparent)]))?;
        call("Page.enable", json::object([]))?;
        if mode == CaptureMode::Frames {
            return self.frames(devtools, &session, incoming);
        }

        // Not faster than the channel needs: half the PNG work at 24–30 fps.
        let screencast = |active: bool| -> Result<u32, String> {
            if active {
                let n = every_nth_frame(self.capture_fps());
                call("Page.stopScreencast", json::object([]))?;
                let params = json::object([
                    ("format", "png".into()),
                    ("maxWidth", width.into()),
                    ("maxHeight", height.into()),
                    ("everyNthFrame", u64::from(n).into()),
                ]);
                call("Page.startScreencast", params)?;
                Ok(n)
            } else {
                call("Page.stopScreencast", json::object([]))?;
                Ok(0)
            }
        };

        let active = self.state().active;
        let mut step = if active { screencast(true)? } else { 0 };
        {
            let mut state = self.state();
            state.status = if state.active { "capturing" } else { "paused" };
        }
        crate::info!("burn-in {}: capturing {}", self.key(), self.options.url);

        // A screenshot of the final state when the screencast went quiet.
        let shooting = Arc::new(AtomicBool::new(false));
        let mut settle_at = (step > 1).then(|| Instant::now() + SETTLE);
        loop {
            let wait = settle_at.map_or(Duration::from_secs(3600), |at| at.saturating_duration_since(Instant::now()));
            match incoming.recv_timeout(wait) {
                Err(RecvTimeoutError::Timeout) => {
                    settle_at = None;
                    // On its own thread, as the Node.js build awaited it: the loop keeps
                    // acking screencast frames meanwhile, one shot at a time.
                    if self.state().active && !shooting.swap(true, Ordering::Relaxed) {
                        let (capture, devtools, session, shooting) =
                            (Arc::clone(self), Arc::clone(devtools), session.clone(), Arc::clone(&shooting));
                        let _ = thread::Builder::new().name("screenshot".into()).spawn(move || {
                            let params = json::object([("format", "png".into())]);
                            match devtools.call("Page.captureScreenshot", params, Some(&session), CALL_TIMEOUT) {
                                Ok(shot) => {
                                    if let Some(data) = shot.get("data").and_then(Value::as_str) {
                                        capture.push_png(data, Instant::now());
                                    }
                                }
                                Err(error) => crate::warn!("burn-in {}: {error}", capture.key()),
                            }
                            shooting.store(false, Ordering::Relaxed);
                        });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err("browser exited".into()),
                Ok(Control::Active(on)) => {
                    match screencast(on) {
                        Ok(n) => step = n,
                        Err(error) => crate::warn!("burn-in {}: {error}", self.key()),
                    }
                    settle_at = (on && step > 1).then(|| Instant::now() + SETTLE);
                }
                Ok(Control::DevTools(Incoming::Closed)) => return Err("browser exited".into()),
                Ok(Control::DevTools(Incoming::Event(event))) => {
                    let method = event.get("method").and_then(Value::as_str).unwrap_or("");
                    let params = event.get("params").cloned().unwrap_or(Value::Null);
                    match method {
                        "Page.screencastFrame" if event.get("sessionId").and_then(Value::as_str) == Some(session.as_str()) => {
                            // Ack at once so Chromium renders the next frame while this one is decoded.
                            let ack = json::object([("sessionId", params.get("sessionId").cloned().unwrap_or(Value::Null))]);
                            let _ = devtools.send("Page.screencastFrameAck", ack, Some(&session));
                            if let Some(data) = params.get("data").and_then(Value::as_str) {
                                let timestamp = params.get("metadata").and_then(|m| m.get("timestamp")).and_then(Value::as_f64);
                                self.push_png(data, drawn_at(timestamp));
                            }
                            settle_at = (step > 1 && self.state().active).then(|| Instant::now() + SETTLE);
                        }
                        "Inspector.targetCrashed" | "Target.targetCrashed" | "Target.detachedFromTarget" => {
                            return Err(format!("page closed ({method})"));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// The frames mode: Chromium draws once per slot of the channel's frame
    /// grid, with the slot's time as frame time, so an animation advances
    /// exactly one frame per output frame. A picture is dated by its slot. A
    /// screenshot always redraws and reports damage: while the page moves
    /// every slot asks for one; once it stands still, a slot only probes for
    /// damage and a screenshot follows when there is some. Slots missed by a
    /// slow host are skipped (the picture before repeats), never made up.
    fn frames(&self, devtools: &DevTools, session: &str, incoming: &mpsc::Receiver<Control>) -> Result<(), String> {
        let begin = |time: f64, interval: f64, screenshot: bool| {
            let mut params = json::object([("frameTimeTicks", time.into()), ("interval", interval.into())]);
            if let (true, Value::Object(members)) = (screenshot, &mut params) {
                let png = json::object([("format", "png".into()), ("optimizeForSpeed", true.into())]);
                members.push(("screenshot".into(), png));
            }
            devtools.call("HeadlessExperimental.beginFrame", params, Some(session), CALL_TIMEOUT)
        };
        // Chromium's frame times are CLOCK_MONOTONIC in ms, as `Instant`.
        let (origin_ms, origin) = (sys::monotonic_ms(), Instant::now());
        let instant = |ms: f64| {
            let offset = Duration::from_secs_f64((ms - origin_ms).abs() / 1000.0);
            if ms >= origin_ms { origin.checked_add(offset) } else { origin.checked_sub(offset) }.unwrap_or(origin)
        };

        let mut frame_ms = 1000.0 / self.capture_fps();
        if let Err(error) = begin(sys::monotonic_ms(), frame_ms, false) {
            if error.contains("wasn't found") {
                self.no_frames.store(true, Ordering::Relaxed);
                crate::warn!(
                    "burn-in {}: no begin frames in this browser (needs chromium-headless-shell); using the screencast",
                    self.key()
                );
            }
            return Err(error);
        }
        let mut active = {
            let mut state = self.state();
            state.status = if state.active { "capturing" } else { "paused" };
            state.active
        };
        crate::info!("burn-in {}: capturing {} (frames)", self.key(), self.options.url);

        let mut slot = (sys::monotonic_ms() / frame_ms).ceil();
        let (mut moving, mut last_shot, mut last_png) = (false, f64::NEG_INFINITY, None::<String>);
        // Slots of the window drawn and missed (a host too slow for the rate).
        let (mut window, mut drawn, mut missed) = (Instant::now(), 0u64, 0u64);
        loop {
            let wait = if active { instant(slot * frame_ms).saturating_duration_since(Instant::now()) } else { Duration::from_secs(3600) };
            match incoming.recv_timeout(wait) {
                Err(RecvTimeoutError::Disconnected) | Ok(Control::DevTools(Incoming::Closed)) => return Err("browser exited".into()),
                Ok(Control::Active(on)) => {
                    active = on;
                    (window, drawn, missed) = (Instant::now(), 0, 0);
                    // A new grid at the current rate; the first slot sends a fresh picture.
                    frame_ms = 1000.0 / self.capture_fps();
                    slot = (sys::monotonic_ms() / frame_ms).ceil();
                    (moving, last_shot, last_png) = (false, f64::NEG_INFINITY, None);
                }
                Ok(Control::DevTools(Incoming::Event(event))) => {
                    let method = event.get("method").and_then(Value::as_str).unwrap_or("");
                    if matches!(method, "Inspector.targetCrashed" | "Target.targetCrashed" | "Target.detachedFromTarget") {
                        return Err(format!("page closed ({method})"));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let time = slot * frame_ms;
                    let check = time - last_shot >= CHECK_MS;
                    let mut result = begin(time, frame_ms, moving || check)?;
                    if result.get("screenshotData").is_some() {
                        last_shot = time;
                    } else if result.get("hasDamage").and_then(Value::as_bool) == Some(true) {
                        last_shot = time;
                        result = begin(time + 0.001, frame_ms, true)?;
                    }
                    let shot = result.get("screenshotData").and_then(Value::as_str);
                    moving = shot.is_some_and(|data| last_png.as_deref() != Some(data));
                    match shot {
                        Some(data) if moving => {
                            self.push_png(data, instant(time));
                            last_png = Some(data.to_owned());
                        }
                        // The page stood still; the frame after a screenshot reports damage, an empty frame takes it.
                        Some(_) => {
                            begin(time + 0.002, frame_ms, false)?;
                        }
                        None => {}
                    }
                    // Behind the grid: on with the slot of now; the missed ones repeat the picture.
                    let next = (slot + 1.0).max((sys::monotonic_ms() / frame_ms).floor());
                    (drawn, missed) = (drawn + 1, missed + (next - slot - 1.0) as u64);
                    slot = next;
                    if window.elapsed() >= MISSED_WINDOW {
                        if missed * 10 > drawn + missed {
                            crate::warn!(
                                "burn-in {}: {missed} of {} frames not drawn in time; lower max_fps or render_size",
                                self.key(),
                                drawn + missed
                            );
                        }
                        (window, drawn, missed) = (Instant::now(), 0, 0);
                    }
                }
            }
        }
    }

    /// A new picture; one of another size (the page resizing) is skipped.
    fn push_png(&self, data: &str, at: Instant) {
        let png = match base64::decode(data) {
            Ok(png) => png,
            Err(error) => return crate::warn!("burn-in {}: {error}", self.key()),
        };
        let o = &self.options;
        match png_size(&png) {
            Some(size) if size == (o.width, o.height) => self.history.set_png(png, at, &self.decode),
            Some((w, h)) => crate::warn!("burn-in {}: frame {w}x{h} skipped", self.key()),
            None => crate::warn!("burn-in {}: not a PNG", self.key()),
        }
    }
}

/// Width and height from the PNG header.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let header = png.get(..24).filter(|h| h.starts_with(b"\x89PNG\r\n\x1a\n") && &h[12..16] == b"IHDR")?;
    let number = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    Some((number(16), number(20)))
}

/// Visible rectangles of a capture's PNG at the channel size.
fn decoder(options: &CaptureOptions) -> Arc<Decode> {
    let (width, height, out_width, out_height) = (options.width, options.height, options.out_width, options.out_height);
    Arc::new(move |png: &[u8]| {
        let picture = decode_png(png)?;
        if (picture.width, picture.height) != (width, height) {
            return Err(format!("frame {}x{} skipped", picture.width, picture.height));
        }
        let boxes = visible_rects(&picture.rgba, width, height);
        Ok(if (width, height) != (out_width, out_height) {
            scale_rects(&picture.rgba, width, height, &boxes, out_width, out_height)
        } else {
            boxes.into_iter().map(|(x, y, w, h)| Rect { x, y, w, h, pixels: crop(&picture.rgba, width, (x, y, w, h)) }).collect()
        })
    })
}

impl Source for Capture {
    fn frame_at(&self, time: Instant) -> Arc<Frame> {
        self.history.frame_at(time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screencast_step_keeps_the_channel_rate() {
        for (fps, n) in [(0.0, 60), (1.0, 60), (24.0, 2), (25.0, 2), (29.97, 2), (30.0, 2), (50.0, 1), (60.0, 1), (100.0, 1)] {
            assert_eq!(every_nth_frame(fps), n, "{fps}");
        }
    }

    #[test]
    fn max_fps_lowers_the_screencast_rate() {
        assert_eq!(every_nth_frame(capped_fps(50.0, 25.0)), 2);
        assert_eq!(every_nth_frame(capped_fps(25.0, 50.0)), 2);
        assert_eq!(every_nth_frame(capped_fps(50.0, 0.0)), 1);
    }

    #[test]
    fn png_size_reads_the_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((1920, 1080)));
        assert_eq!(png_size(&png[..20]), None);
        assert_eq!(png_size(b"GIF89a, not a PNG at all"), None);
    }

    #[test]
    fn drawn_at_uses_the_timestamp_only_when_plausible() {
        let wall = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        let before = Instant::now();
        assert!(drawn_at(Some(wall - 0.2)) < before);
        assert!(drawn_at(Some(wall - 5.0)) >= before);
        assert!(drawn_at(Some(wall + 5.0)) >= before);
        assert!(drawn_at(None) >= before);
    }
}
