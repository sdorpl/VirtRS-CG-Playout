// HTTP side of the sidecar (the routes of src/server.js):
//
//   POST /etv?channel=1[&render=720p]     ErsatzTV script element (etap 3–4)
//   GET  /ffmpeg?channel=1[&wait=2000]    ffmpeg overlay takes a session over (etap 6)
//   GET  /ffmpeg/frames?session=N         raw yuva420p frames for ffmpeg (etap 6)
//   GET  /health                          captures and sessions
//
// Routes not built yet answer 404: the clients then play the item without
// graphics, as with the sidecar down.

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::Config;
use crate::http::{self, Request};
use crate::json::{self, Value};

pub const MAX_BODY: usize = 4 * 1024 * 1024;

pub struct Server {
    pub config: Config,
    sessions: AtomicUsize,
}

impl Server {
    pub fn new(config: Config) -> Arc<Server> {
        Arc::new(Server { config, sessions: AtomicUsize::new(0) })
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
            _ => http::respond(&mut stream, 404, None, b""),
        };
        if let Err(error) = result {
            crate::debug!("http {} {}: {error}", request.method, request.path);
        }
    }
}
