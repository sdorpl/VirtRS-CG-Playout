// ffmpeg for ErsatzTV that overlays the VizRS graphics itself (binary
// vizrs-ffmpeg, set as the FFmpeg path in ErsatzTV; DOKUMENTACJA §1). Every
// call goes to the real ffmpeg unchanged, except a transcode that reads the
// ErsatzTV graphics canvas (`-i http://localhost:8409/internal/graphics/<channel>/<item>?...`):
//
// 1. the canvas is opened here and drained, so ErsatzTV still starts the VizRS
//    script element with the item's template data (commands, ads, EPG);
// 2. the sidecar takes that script session over (`GET /ffmpeg`): ErsatzTV gets
//    no more packets and its graphics engine waits at no cost;
// 3. ffmpeg reads raw yuva420p frames from the sidecar instead of the canvas
//    (`GET /ffmpeg/frames`), through a socket passed as `pipe:N`.
//
// ErsatzTV's own graphics elements of that item (text, image, watermarks in
// the canvas) are not drawn then. Whenever the sidecar does not answer, the
// original command runs: the item plays with ErsatzTV's graphics path.
//
// With VIZRS_FFMPEG_FPS every transcode to HLS runs at that rate (ErsatzTV
// takes the rate from the media): the video gets `fps=N` before the graphics
// overlay, so the graphics move at N frames per second, and the output `-r N`.
//
//     VIZRS_CG_URL       sidecar, e.g. http://cg:7700 (empty: no overlay)
//     VIZRS_FFMPEG       real ffmpeg (default /usr/local/bin/ffmpeg)
//     VIZRS_FFMPEG_FPS   channel frame rate, e.g. 50 (empty: the media's rate)
//
// A static binary, so it runs in the ErsatzTV image of any architecture
// without extra packages. A canvas over https goes to plain ffmpeg.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use regex::Regex;

use crate::http::percent_decode;
use crate::json::{self, Value};
use crate::net::{Url, connect, error, read_head, status_of};
use crate::sys;

const DEFAULT_FFMPEG: &str = "/usr/local/bin/ffmpeg";
const CLAIM_WAIT_MS: u32 = 2000;
const SCRIPT_END: Duration = Duration::from_millis(500);
const COPY: [(&str, &str); 4] = [("-c", "copy"), ("-c:v", "copy"), ("-vcodec", "copy"), ("-codec:v", "copy")];
// Options of the canvas input that do not apply to the raw frames.
const DROP_WITH_VALUE: [&str; 16] = [
    "-headers",
    "-protocol_whitelist",
    "-reconnect",
    "-reconnect_streamed",
    "-reconnect_delay_max",
    "-reconnect_on_network_error",
    "-rw_timeout",
    "-f",
    "-c:v",
    "-vcodec",
    "-codec:v",
    "-pix_fmt",
    "-video_size",
    "-s",
    "-framerate",
    "-r",
];
const FORWARDED: [i32; 4] = [sys::SIGTERM, sys::SIGINT, sys::SIGHUP, sys::SIGQUIT];

/// The overlay's environment.
pub struct Settings {
    pub ffmpeg: OsString,
    /// Sidecar URL without the trailing `/`; empty = no overlay.
    pub sidecar: String,
    /// Forced channel frame rate; empty = the media's.
    pub fps: String,
}

impl Settings {
    pub fn from_env() -> Settings {
        let var = |name| std::env::var(name).unwrap_or_default();
        Settings {
            ffmpeg: std::env::var_os("VIZRS_FFMPEG").unwrap_or_else(|| DEFAULT_FFMPEG.into()),
            sidecar: var("VIZRS_CG_URL").trim_end_matches('/').to_owned(),
            fps: var("VIZRS_FFMPEG_FPS").trim().to_owned(),
        }
    }
}

/// The canvas input of an ffmpeg command.
#[derive(Debug, PartialEq)]
pub struct Canvas {
    /// First option of the canvas input (after the previous `-i <input>`).
    pub start: usize,
    /// Index of its `-i`.
    pub index: usize,
    /// ErsatzTV channel from the canvas path.
    pub channel: String,
}

fn pair(args: &[OsString], i: usize, a: &str, b: &str) -> bool {
    args.get(i).is_some_and(|x| x == a) && args.get(i + 1).is_some_and(|y| y == b)
}

fn overlay_filter() -> &'static Regex {
    static OVERLAY: OnceLock<Regex> = OnceLock::new();
    // The first overlay of two labelled inputs: [video][graphics]overlay…
    OVERLAY.get_or_init(|| Regex::new(r"\[([^\]]+)\]\[([^\]]+)\](overlay\w*)").expect("overlay pattern"))
}

/// The filter with the graphics of input `input` (the canvas) overlaid at
/// `x`, `y`: their frames are the graphics area, not the whole channel. None
/// when that cannot be done safely: no overlay of that input, positional
/// overlay options, or the graphics resized on the way.
pub fn placed(filter: &str, input: usize, x: u32, y: u32) -> Option<String> {
    let inputs = [format!("[{input}]"), format!("[{input}:0]"), format!("[{input}:v]"), format!("[{input}:v:0]")];
    // The chain that prepares the graphics, `[1:0]format=yuva420p[v_s0]`, if any.
    let chain = filter.split(';').find(|chain| inputs.iter().any(|label| chain.starts_with(label.as_str())));
    let label = match chain {
        Some(chain) if chain.contains("scale") => return None,
        Some(chain) => chain.rsplit_once('[').filter(|(_, l)| l.ends_with(']'))?.1.trim_end_matches(']').to_owned(),
        None => inputs.iter().find(|label| filter.contains(label.as_str()))?.trim_matches(['[', ']']).to_owned(),
    };
    let pattern = format!(r"\[([^\]]+)\]\[{}\](overlay\w*)(=[^\[,;]*)?", regex::escape(&label));
    let found = Regex::new(&pattern).ok()?.captures(filter)?;
    let mut options = Vec::new();
    for option in found.get(3).map_or("", |m| &m.as_str()[1..]).split(':').filter(|o| !o.is_empty()) {
        let (name, _) = option.split_once('=')?;
        if name != "x" && name != "y" {
            options.push(option.to_owned());
        }
    }
    options.extend([format!("x={x}"), format!("y={y}")]);
    let whole = found.get(0)?;
    let replaced = format!("[{}][{label}]{}={}", &found[1], &found[2], options.join(":"));
    Some(format!("{}{replaced}{}", &filter[..whole.start()], &filter[whole.end()..]))
}

/// `args` with the canvas input (`-i` number `input`) overlaid at `x`, `y`.
pub fn with_position(args: &[OsString], input: usize, x: u32, y: u32) -> Option<Vec<OsString>> {
    let i = (0..args.len().saturating_sub(1)).find(|&i| args[i] == "-filter_complex")?;
    let filter = placed(args[i + 1].to_str()?, input, x, y)?;
    let mut out = args.to_vec();
    out[i + 1] = filter.into();
    Some(out)
}

/// Number of the input `-i` at `index` among the inputs of `args`.
pub fn input_number(args: &[OsString], index: usize) -> usize {
    (0..index).filter(|&i| args[i] == "-i").count()
}

/// A transcode to HLS at `fps`: `fps` before the overlay, `-r` on the output.
pub fn with_rate(args: Vec<OsString>, fps: &str) -> Vec<OsString> {
    let hls = (0..args.len()).find(|&i| pair(&args, i, "-f", "hls"));
    let copy = (0..args.len()).any(|i| COPY.iter().any(|(a, b)| pair(&args, i, a, b)));
    let (Some(hls), false, false) = (hls, fps.is_empty(), copy) else {
        return args;
    };

    let mut out = args;
    for i in 0..out.len() - 1 {
        if out[i] != "-filter_complex" {
            continue;
        }
        if let Some(filter) = out[i + 1].to_str() {
            let replaced = overlay_filter()
                .replacen(filter, 1, |m: &regex::Captures| format!("[{0}]fps={fps}[{0}_fps];[{0}_fps][{1}]{2}", &m[1], &m[2], &m[3]));
            out[i + 1] = replaced.into_owned().into();
        }
    }
    out.splice(hls..hls, ["-r".into(), fps.into()]);
    out
}

/// `/internal/graphics/<channel>/<item>`: the channel, still percent-encoded.
fn canvas_channel(path: &str) -> Option<&str> {
    let (channel, item) = path.strip_prefix("/internal/graphics/")?.split_once('/')?;
    let valid = !channel.is_empty() && !item.is_empty() && item.bytes().all(|b| b.is_ascii_digit());
    valid.then_some(channel)
}

/// `urllib.parse.unquote`: `%XX` only, `+` stays.
fn unquote(text: &str) -> String {
    percent_decode(&text.replace('+', "%2B"))
}

/// The canvas input of `args`, if any.
pub fn find_canvas(args: &[OsString]) -> Option<Canvas> {
    let mut start = 0;
    for i in 0..args.len().saturating_sub(1) {
        if args[i] != "-i" {
            continue;
        }
        let url = args[i + 1].to_str().and_then(Url::split);
        if let Some((url, channel)) = url.and_then(|u| canvas_channel(u.path).map(|c| (u, c))).filter(|(u, _)| u.scheme == "http") {
            // A region canvas only carries watermarks: no VizRS element on the item.
            let region = url.query.split('&').any(|p| p.split_once('=').is_some_and(|(k, v)| unquote(k) == "region" && !v.is_empty()));
            if region {
                return None;
            }
            return Some(Canvas { start, index: i, channel: unquote(channel) });
        }
        start = i + 2;
    }
    None
}

/// Headers of the `-headers` options (`name: value` lines).
pub fn parse_headers(options: &[OsString]) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for i in 0..options.len().saturating_sub(1) {
        if options[i] != "-headers" {
            continue;
        }
        for line in options[i + 1].to_string_lossy().split('\n') {
            let Some((name, value)) = line.trim_end_matches('\r').split_once(':') else { continue };
            let (name, value) = (name.trim(), value.trim());
            if name.is_empty() {
                continue;
            }
            match headers.iter_mut().find(|(n, _)| n == name) {
                Some(header) => header.1 = value.to_owned(),
                None => headers.push((name.to_owned(), value.to_owned())),
            }
        }
    }
    headers
}

/// What the sidecar gives the overlay (`GET /ffmpeg`), with the rate decided.
pub struct Frames {
    pub width: u32,
    pub height: u32,
    pub rate: String,
    /// Position of the frames in the channel (the graphics area); None = the whole channel.
    pub position: Option<(u32, u32)>,
}

/// The command with the canvas input replaced by raw frames from `pipe:<fd>`.
pub fn overlay_args(args: &[OsString], canvas: &Canvas, fd: i32, frames: &Frames, fps: &str) -> Vec<OsString> {
    let options = &args[canvas.start..canvas.index];
    let mut out: Vec<OsString> = args[..canvas.start].to_vec();
    let mut i = 0;
    while i < options.len() {
        if i + 1 < options.len() && DROP_WITH_VALUE.iter().any(|o| options[i] == *o) {
            i += 2;
            continue;
        }
        out.push(options[i].clone());
        i += 1;
    }
    let size = format!("{}x{}", frames.width, frames.height);
    let raw = ["-f", "rawvideo", "-pix_fmt", "yuva420p", "-video_size", &size, "-framerate", &frames.rate, "-i", &format!("pipe:{fd}")];
    out.extend(raw.iter().map(OsString::from));
    out.extend_from_slice(&args[canvas.index + 2..]);
    // Frames of a graphics area: overlaid at its place (checked by `with_position` beforehand).
    if let Some((x, y)) = frames.position.filter(|&p| p != (0, 0)) {
        out = with_position(&out, input_number(args, canvas.index), x, y).unwrap_or(out);
    }
    with_rate(out, fps)
}

/// `application/x-www-form-urlencoded` value, as `urllib.parse.urlencode`.
fn quote_plus(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => (b as char).to_string(),
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn log(message: impl std::fmt::Display) {
    let _ = writeln!(io::stderr(), "vizrs-ffmpeg: {message}");
}

/// Reads the ErsatzTV canvas and throws it away until closed.
#[derive(Clone, Default)]
struct Drain {
    socket: Arc<Mutex<Option<TcpStream>>>,
    closed: Arc<Mutex<bool>>,
}

impl Drain {
    fn start(url: String, headers: Vec<(String, String)>) -> Drain {
        let drain = Drain::default();
        let this = drain.clone();
        std::thread::spawn(move || {
            let _ = this.run(&url, &headers);
        });
        drain
    }

    fn run(&self, url: &str, headers: &[(String, String)]) -> io::Result<()> {
        let url = Url::split(url).ok_or_else(|| error("bad canvas URL".into()))?;
        let mut stream = connect(&url)?;
        {
            let closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
            if *closed {
                return Ok(());
            }
            *self.socket.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream.try_clone()?);
        }
        let mut request = format!("GET {} HTTP/1.1\r\nHost: {}\r\nAccept-Encoding: identity\r\n", url.target(), url.netloc);
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes())?;
        read_head(&mut stream, "canvas")?;
        stream.set_read_timeout(None)?;
        let mut buffer = vec![0u8; 1 << 16];
        while stream.read(&mut buffer)? > 0 {}
        Ok(())
    }

    fn close(&self) {
        let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        *closed = true;
        if let Some(socket) = self.socket.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

/// `GET path` on the sidecar: status and body.
fn sidecar_get(sidecar: &str, path: &str) -> io::Result<(String, Vec<u8>)> {
    let url = Url::split(sidecar).ok_or_else(|| error(format!("bad sidecar URL {sidecar}")))?;
    let mut stream = connect(&url)?;
    stream.write_all(format!("GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n", url.netloc).as_bytes())?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    let end = reply.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| error("sidecar: no reply".into()))?;
    let head = String::from_utf8_lossy(&reply[..end]);
    let status = status_of(head.lines().next().unwrap_or("")).to_owned();
    Ok((status, reply[end + 4..].to_vec()))
}

/// Socket positioned at the body of the sidecar's frame stream.
fn open_frames(sidecar: &str, path: &str) -> io::Result<TcpStream> {
    let url = Url::split(sidecar).ok_or_else(|| error(format!("bad sidecar URL {sidecar}")))?;
    let mut stream = connect(&url)?;
    stream.write_all(format!("GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n", url.netloc).as_bytes())?;
    let line = read_head(&mut stream, "frame stream")?;
    if status_of(&line) != "200" {
        return Err(error(format!("frame stream: {line}")));
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok(stream)
}

/// The sidecar's session for the canvas channel and its frame stream. With
/// `place` (the command can overlay the graphics anywhere) the frames are
/// those of the graphics area, if the channel has one.
fn take_over(settings: &Settings, channel: &str, header_rate: &str, place: bool) -> io::Result<(Frames, TcpStream)> {
    let area = if place { "&area=1" } else { "" };
    let path = format!("/ffmpeg?channel={}&wait={CLAIM_WAIT_MS}{area}", quote_plus(channel));
    let (status, body) = sidecar_get(&settings.sidecar, &path)?;
    if status != "200" {
        return Err(error(format!("no VizRS session on channel {channel} ({status})")));
    }
    let info = json::parse(&String::from_utf8_lossy(&body)).map_err(|e| error(e.to_string()))?;
    let field = |key| info.get(key).filter(|v| !v.is_null()).ok_or_else(|| error(format!("'{key}'")));
    let size = |key| field(key).and_then(|v| v.as_f64().map(|n| n as u32).ok_or_else(|| error(format!("'{key}'"))));
    let rate = match (settings.fps.as_str(), header_rate) {
        ("", "") => field("fps")?.to_string(),
        ("", rate) => rate.to_owned(),
        (fps, _) => fps.to_owned(),
    };
    let session = match field("session")? {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let position = if place { Some((size("x")?, size("y")?)) } else { None };
    let frames = Frames { width: size("width")?, height: size("height")?, rate, position };
    let path = format!("/ffmpeg/frames?session={}&fps={}{area}", quote_plus(&session), quote_plus(&frames.rate));
    Ok((frames, open_frames(&settings.sidecar, &path)?))
}

/// Replaces this process with the real ffmpeg (at the forced rate, if any).
fn plain(settings: &Settings, args: Vec<OsString>) -> ! {
    let error = Command::new(&settings.ffmpeg).args(with_rate(args, &settings.fps)).exec();
    log(format_args!("{}: {error}", settings.ffmpeg.to_string_lossy()));
    std::process::exit(127)
}

/// The overlay: `vizrs-ffmpeg <ffmpeg arguments>`; returns the exit code.
pub fn run(settings: &Settings, args: Vec<OsString>) -> i32 {
    // Not a canvas transcode: plain ffmpeg, at the forced rate when set.
    let Some(canvas) = find_canvas(&args).filter(|_| !settings.sidecar.is_empty()) else { plain(settings, args) };

    let headers = parse_headers(&args[canvas.start..canvas.index]);
    let rate = headers.iter().find(|(name, _)| name == "x-etv-frame-rate").map_or("", |(_, v)| v.as_str()).to_owned();
    let drain = Drain::start(args[canvas.index + 1].to_string_lossy().into_owned(), headers);

    let place = with_position(&args, input_number(&args, canvas.index), 2, 2).is_some();
    let (frames, socket) = match take_over(settings, &canvas.channel, &rate, place) {
        Ok(taken) => taken,
        Err(e) => {
            log(format_args!("{e}; ErsatzTV graphics instead"));
            drain.close();
            plain(settings, args)
        }
    };

    let fd = socket.as_raw_fd();
    // Forwarded to ffmpeg from here on; the child starts with them unblocked.
    sys::block_signals(&FORWARDED);
    let mut command = Command::new(&settings.ffmpeg);
    command.args(overlay_args(&args, &canvas, fd, &frames, &settings.fps));
    // SAFETY: only async-signal-safe calls in the forked child.
    unsafe {
        command.pre_exec(move || sys::ffmpeg_child_setup(fd));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            log(format_args!("{}: {e}", settings.ffmpeg.to_string_lossy()));
            drain.close();
            return 127;
        }
    };
    drop(socket);

    let pid = child.id();
    std::thread::spawn(move || {
        loop {
            sys::send_signal(pid, sys::wait_signal(&FORWARDED));
        }
    });

    let status = child.wait();
    // The sidecar ends the script when ffmpeg's frame stream closes; closing the
    // canvas before that makes ErsatzTV log a cancelled script element.
    std::thread::sleep(SCRIPT_END);
    drain.close();
    match status {
        Ok(status) => status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(1),
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn canvas_input() {
        let list = args(&["-i", "/m/a.mp4", "-headers", "h", "-i", "http://x:1/internal/graphics/2%2E1/28750?exp=1", "-f", "null", "-"]);
        assert_eq!(find_canvas(&list), Some(Canvas { start: 2, index: 4, channel: "2.1".into() }));
        assert_eq!(find_canvas(&args(&["-i", "http://x/internal/graphics/1/7?region=a"])), None);
        assert!(find_canvas(&args(&["-i", "http://x/internal/graphics/1/7?region="])).is_some());
        assert_eq!(find_canvas(&args(&["-i", "http://x/internal/graphics/1/a"])), None);
        assert_eq!(find_canvas(&args(&["-i", "file:///internal/graphics/1/7"])), None);
        assert_eq!(find_canvas(&args(&["-version"])), None);
    }

    #[test]
    fn headers() {
        let list = args(&["-headers", "a: 1\r\nx-etv-frame-rate:25/1\nbad\n: v\r\n", "-headers", "a:2"]);
        assert_eq!(parse_headers(&list), vec![("a".into(), "2".into()), ("x-etv-frame-rate".into(), "25/1".into())]);
    }

    #[test]
    fn rate_only_on_hls_transcodes() {
        let hls = args(&["-i", "a", "-filter_complex", "[0:0]x[v];[v][1:0]overlay_cuda=x=0[o];[o][2]overlay", "-f", "hls", "o.m3u8"]);
        assert_eq!(
            with_rate(hls.clone(), "50"),
            args(&[
                "-i",
                "a",
                "-filter_complex",
                "[0:0]x[v];[v]fps=50[v_fps];[v_fps][1:0]overlay_cuda=x=0[o];[o][2]overlay",
                "-r",
                "50",
                "-f",
                "hls",
                "o.m3u8"
            ])
        );
        assert_eq!(with_rate(hls.clone(), ""), hls);
        let copy = args(&["-i", "a", "-c:v", "copy", "-f", "hls", "o"]);
        assert_eq!(with_rate(copy.clone(), "50"), copy);
    }

    #[test]
    fn graphics_placed_at_the_area() {
        // ErsatzTV's software, NVIDIA and VAAPI graphs, the canvas as input 1.
        let cases = [
            ("[0:0]scale=8:4[v_m0];[1:0]format=yuva420p[v_s0];[v_m0][v_s0]overlay=x=0:y=0[v_o0]", "[v_m0][v_s0]overlay=x=10:y=940[v_o0]"),
            (
                "[1:0]format=yuva420p,hwupload_cuda[v_s0];[v_m0][v_s0]overlay_cuda=x=0:y=0[v_o0]",
                "[v_m0][v_s0]overlay_cuda=x=10:y=940[v_o0]",
            ),
            (
                "[1:0]format=yuva420p[v_s0];[v_m0][v_s0]overlay=format=0,format=nv12|vaapi,hwupload[v_o0]",
                "[v_m0][v_s0]overlay=format=0:x=10:y=940,format",
            ),
            ("[1:0]format=yuva420p,hwupload[v_s0];[v_m0][v_s0]overlay_vaapi[v_o0]", "[v_m0][v_s0]overlay_vaapi=x=10:y=940[v_o0]"),
            ("[0][1]overlay", "[0][1]overlay=x=10:y=940"),
            // A watermark (input 2) overlaid first: only the graphics move.
            (
                "[0:0][2:0]overlay=x=5:y=5[w];[1:0]format=yuva420p[g];[w][g]overlay[o]",
                "[0:0][2:0]overlay=x=5:y=5[w];[1:0]format=yuva420p[g];[w][g]overlay=x=10:y=940[o]",
            ),
        ];
        for (filter, expected) in cases {
            let placed = placed(filter, 1, 10, 940).unwrap();
            assert!(placed.contains(expected), "{placed}");
            assert_eq!(placed.matches("overlay").count(), filter.matches("overlay").count());
        }
        // Positional options, graphics scaled to the channel, no overlay of the canvas: not placed.
        assert_eq!(placed("[0][1]overlay=5:5", 1, 10, 940), None);
        assert_eq!(placed("[1:0]scale=1920:1080,format=yuva420p[g];[0][g]overlay", 1, 10, 940), None);
        assert_eq!(placed("[0]scale=8:4[v]", 1, 10, 940), None);
        assert_eq!(placed("[0][2]overlay", 1, 10, 940), None);

        let list = args(&["-i", "a", "-i", "http://x/internal/graphics/1/7", "-filter_complex", "[0][1]overlay[o]", "-f", "hls", "o"]);
        assert_eq!(input_number(&list, 2), 1);
        assert_eq!(with_position(&list, 1, 2, 4).unwrap()[5], "[0][1]overlay=x=2:y=4[o]");
        assert_eq!(with_position(&args(&["-i", "a", "-f", "null", "-"]), 1, 2, 4), None);
    }

    #[test]
    fn form_values() {
        assert_eq!(quote_plus("1 2/ä"), "1+2%2F%C3%A4");
        assert_eq!(unquote("a+b%2E"), "a+b.");
    }
}
