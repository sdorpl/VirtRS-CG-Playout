// HTTP side of the sidecar (the routes of src/server.js):
//
//   POST /etv?channel=1[&render=720p]     body: ErsatzTV template data (JSON from
//                                         the script element's stdin); reply:
//                                         endless ETV packet stream
//   GET  /ffmpeg?channel=1[&wait=2000]    ffmpeg overlay takes a session over (etap 6)
//   GET  /ffmpeg/frames?session=N         raw yuva420p frames for ffmpeg (etap 6)
//   GET  /health                          captures and sessions
//
// Routes not built yet answer 404: the clients then play the item without
// graphics, as with the sidecar down. Burn-in (etap 4–5) is not built yet
// either: every session streams transparent frames.

use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::config::Config;
use crate::http::{self, Request};
use crate::json::{self, Value};
use crate::stream::{self, Control, Empty, Options};
use crate::template::{AdMatcher, ClipContext, js_string};
use crate::vizrs::ChannelEvents;

pub const MAX_BODY: usize = 4 * 1024 * 1024;

pub struct Server {
    pub config: Arc<Config>,
    ads: Arc<AdMatcher>,
    channels: Mutex<HashMap<String, Arc<ChannelEvents>>>,
    sessions: AtomicUsize,
    empty: Empty,
}

impl Server {
    pub fn new(config: Config) -> Arc<Server> {
        let (ads, warnings) = AdMatcher::new(&config.ad_match);
        for warning in warnings {
            crate::warn!("{warning}");
        }
        if !config.output_url.is_empty() {
            crate::warn!("burn-in is not in this build yet (docs/plany/PLAN-rust.md, etap 4–5): transparent frames");
        }
        Arc::new(Server {
            config: Arc::new(config),
            ads: Arc::new(ads),
            channels: Mutex::new(HashMap::new()),
            sessions: AtomicUsize::new(0),
            empty: Empty::new(),
        })
    }

    fn events_for(&self, channel: &str) -> Arc<ChannelEvents> {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let events =
            channels.entry(channel.to_owned()).or_insert_with(|| ChannelEvents::new(Arc::clone(&self.config), Arc::clone(&self.ads)));
        Arc::clone(events)
    }

    /// `/health`: script sessions, captures, channels served by the ffmpeg overlay.
    pub fn health(&self) -> Value {
        json::object([
            ("sessions", (self.sessions.load(Ordering::Relaxed) as u64).into()),
            ("captures", Value::Array(Vec::new())),
            ("overlay", Value::Array(Vec::new())),
        ])
    }

    pub fn handle(&self, request: Request, mut stream: TcpStream) {
        let result = match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/health") => http::respond(&mut stream, 200, Some("application/json"), self.health().to_string().as_bytes()),
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

        let events = self.events_for(&channel);
        events.session(&ctx);

        let control = Control::new(false);
        watch_close(&stream, &control);
        if let Ok(mut out) = http::Stream::start(stream, request) {
            self.sessions.fetch_add(1, Ordering::Relaxed);
            stream::stream_frames(&self.empty, &mut out, &Options { fps, lead_seconds: self.config.lead_seconds }, &control);
            self.sessions.fetch_sub(1, Ordering::Relaxed);
            let _ = out.finish();
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
