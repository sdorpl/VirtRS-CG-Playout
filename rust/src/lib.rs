// VirtRS CG Playout sidecar: VizRS graphics for ErsatzTV channels.
// Migration from the Node.js sidecar in progress (docs/plany/PLAN-rust.md).

pub mod log;

pub mod config;
pub mod frames;
pub mod http;
pub mod json;
pub mod packets;
pub mod png;
pub mod rects;
pub mod scale;
pub mod server;
pub mod stream;
pub mod template;
pub mod time;
pub mod vizrs;
pub mod yuva;
