// VizRS TCP client (line protocol, default port 6100) and the per-channel
// logic of clip and ad events: ads are played without graphics, the pages
// taken out come back with the first regular clip (src/vizrs.js).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config::Config;
use crate::json::{self, Value};
use crate::template::{AdMatcher, ClipContext, js_string, render_commands};
use crate::time;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
// After an item's stop: no new session by then = the next item has no element.
const GAP_GRACE_MS: f64 = 2000.0;

/// `OUT` for all layers on air in a `STATE` reply, and their pages.
pub fn on_air(state_reply: &str) -> Option<(String, Vec<String>)> {
    let state = json::parse(state_reply.strip_prefix("OK")?.trim()).ok()?;
    let layers = state.get("layers").and_then(Value::as_array).unwrap_or(&[]);
    let names: Vec<&str> = layers.iter().filter_map(|l| l.get("layer")?.as_str()).collect();
    // VizRS reports the page as an object ({id, number, name}); older builds as a string.
    let pages = layers
        .iter()
        .filter_map(|l| match l.get("page")? {
            page @ Value::Object(_) => page.get("number"),
            page => Some(page),
        })
        .filter(|p| matches!(p, Value::String(s) if !s.is_empty()) || matches!(p, Value::Number(_)))
        .map(js_string)
        .collect();
    (!names.is_empty()).then(|| (format!("OUT {}", names.join(",")), pages))
}

/// Replies per command, and the pages taken out by `clear_first`.
#[derive(Debug, Default, PartialEq)]
pub struct Sent {
    pub replies: Vec<(String, String)>,
    pub cleared_pages: Vec<String>,
}

struct Connection {
    reader: BufReader<TcpStream>,
}

impl Connection {
    fn open(address: &str) -> Result<Connection, String> {
        let (host, port) = address.rsplit_once(':').unwrap_or(("", address));
        let host = if host.is_empty() { "127.0.0.1" } else { host.trim_start_matches('[').trim_end_matches(']') };
        let port: u16 = port.parse().map_err(|_| format!("connect to {address}: bad port"))?;
        let addrs = (host, port).to_socket_addrs().map_err(|e| format!("connect to {address}: {e}"))?;

        let mut last = format!("connect to {address}: no address");
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    stream.set_read_timeout(Some(REPLY_TIMEOUT)).map_err(|e| e.to_string())?;
                    stream.set_write_timeout(Some(REPLY_TIMEOUT)).map_err(|e| e.to_string())?;
                    return Ok(Connection { reader: BufReader::new(stream) });
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => last = format!("connect to {address}: timeout"),
                Err(e) => last = format!("connect to {address}: {e}"),
            }
        }
        Err(last)
    }

    /// One command line, one reply line.
    fn command(&mut self, line: &str) -> Result<String, String> {
        let verb = line.split(' ').next().unwrap_or("");
        self.reader.get_mut().write_all(format!("{line}\n").as_bytes()).map_err(|e| format!("{verb}: {e}"))?;
        let mut reply = Vec::new();
        match self.reader.read_until(b'\n', &mut reply) {
            Ok(0) => Err("connection closed".into()),
            Ok(_) if reply.last() != Some(&b'\n') => Err("connection closed".into()),
            Ok(_) => Ok(String::from_utf8_lossy(&reply).trim().to_owned()),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Err(format!("{verb}: no reply")),
            Err(e) => Err(format!("{verb}: {e}")),
        }
    }
}

/// Sends commands over one connection: replies per command and the pages taken
/// out by `clear_first` (all layers on air, from `STATE`).
pub fn send(address: &str, token: &str, commands: &[String], clear_first: bool) -> Result<Sent, String> {
    let mut connection = Connection::open(address)?;
    let mut sent = Sent::default();

    if !token.is_empty() {
        let reply = connection.command(&format!("AUTH {token}"))?;
        if !reply.starts_with("OK") {
            return Err(format!("AUTH: {reply}"));
        }
    }

    if clear_first {
        if let Some((out, pages)) = on_air(&connection.command("STATE")?) {
            let reply = connection.command(&out)?;
            if reply.starts_with("OK") {
                sent.cleared_pages = pages;
            }
            sent.replies.push((out, reply));
        }
    }

    for command in commands {
        let reply = connection.command(command)?;
        sent.replies.push((command.clone(), reply));
    }
    Ok(sent)
}

/// A batch for the channel's sender, in the order the events happened.
struct Batch {
    ad: bool,
    commands: Vec<String>,
}

struct State {
    last_key: Option<String>,
    active: usize,
    /// Bumped by every session start and end: a pending gap check of an older
    /// generation does nothing (the `clearTimeout` of the Node.js build).
    gap_generation: u64,
}

/// Clip events of one channel, sent in order.
pub struct ChannelEvents {
    config: Arc<Config>,
    ads: Arc<AdMatcher>,
    state: Mutex<State>,
    queue: Mutex<Sender<Batch>>,
}

impl ChannelEvents {
    pub fn new(config: Arc<Config>, ads: Arc<AdMatcher>) -> Arc<ChannelEvents> {
        let (queue, batches) = mpsc::channel::<Batch>();
        let sender_config = Arc::clone(&config);
        thread::Builder::new()
            .name("vizrs".into())
            .spawn(move || {
                // Pages taken out for the break, owned by the sender as in the Node.js queue.
                let mut cleared: Option<Vec<String>> = None;
                for batch in batches {
                    send_clip(&sender_config, &mut cleared, batch);
                }
            })
            .expect("vizrs sender thread");
        Arc::new(ChannelEvents {
            config,
            ads,
            state: Mutex::new(State { last_key: None, active: 0, gap_generation: 0 }),
            queue: Mutex::new(queue),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn enqueue(&self, batch: Batch) {
        let _ = self.queue.lock().unwrap_or_else(|e| e.into_inner()).send(batch);
    }

    /// A script session of the channel starts; call `ended` when it is over.
    pub fn session(&self, ctx: &ClipContext) {
        {
            let mut state = self.state();
            state.active += 1;
            state.gap_generation += 1;
        }
        self.clip(ctx);
    }

    /// A session ended. When no session follows by the item's stop (ErsatzTV
    /// runs ahead of the clock), the next item has no VizRS element: an ad.
    pub fn ended(self: &Arc<Self>, ctx: &ClipContext) {
        let generation = {
            let mut state = self.state();
            state.active = state.active.saturating_sub(1);
            state.gap_generation += 1;
            if !self.config.ad_without_element || state.active > 0 {
                return;
            }
            state.gap_generation
        };

        let key = format!("{}|{}", ctx.source, ctx.start);
        let stop = time::parse_iso(&ctx.stop).map_or(0.0, |stop| (stop - time::now_ms()).max(0) as f64);
        let delay = Duration::from_secs_f64((stop + self.config.gap_grace_ms.unwrap_or(GAP_GRACE_MS)).max(0.0) / 1000.0);
        let events = Arc::clone(self);
        let ctx = ctx.clone();
        let _ = thread::Builder::new().name("vizrs-gap".into()).spawn(move || {
            thread::sleep(delay);
            let gap = {
                let state = events.state();
                state.gap_generation == generation && state.active == 0 && state.last_key.as_deref() == Some(key.as_str())
            };
            if gap {
                events.ad(&ctx);
            }
        });
    }

    /// An item without the element: graphics out and `on_ad`, as for a matched ad.
    fn ad(&self, ctx: &ClipContext) {
        self.state().last_key = Some(format!("ad|{}", ctx.stop));
        let ad_ctx = ClipContext { title: String::new(), show_title: String::new(), source: String::new(), duration: 0.0, ..ctx.clone() };
        self.enqueue(Batch { ad: true, commands: render_commands(&self.config.on_ad, &ad_ctx) });
    }

    /// Events for a clip that starts (or is joined) now. ErsatzTV starts the
    /// script again when a new session joins the same item; that does not send
    /// the commands twice (and does not restart the animations).
    fn clip(&self, ctx: &ClipContext) {
        let key = format!("{}|{}", ctx.source, ctx.start);
        {
            let mut state = self.state();
            if state.last_key.as_deref() == Some(key.as_str()) {
                return;
            }
            state.last_key = Some(key);
        }
        let ad = self.ads.is_ad(ctx);
        let template = if ad { &self.config.on_ad } else { &self.config.on_clip };
        self.enqueue(Batch { ad, commands: render_commands(template, ctx) });
    }
}

fn send_clip(config: &Config, cleared: &mut Option<Vec<String>>, Batch { ad, mut commands }: Batch) {
    // Graphics taken out for the break come back before the clip's own commands.
    let restore = if ad { None } else { cleared.take() };
    if let Some(pages) = &restore {
        commands.insert(0, format!("TAKE {}", pages.join(",")));
    }
    if commands.is_empty() && !ad {
        return;
    }

    match send(&config.vizrs.address, &config.vizrs.token, &commands, ad) {
        Ok(sent) => {
            // Later ads of the same break only find `on_ad` graphics on air.
            if !sent.cleared_pages.is_empty() && cleared.is_none() {
                *cleared = Some(sent.cleared_pages);
            }
            for (command, reply) in &sent.replies {
                if reply.starts_with("OK") {
                    crate::debug!("VizRS {command}: {reply}");
                } else {
                    crate::warn!("VizRS {command}: {reply}");
                }
            }
        }
        Err(error) => {
            // Try again with the next regular clip.
            if restore.is_some() {
                *cleared = restore;
            }
            crate::error!("VizRS: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;

    /// VizRS stand-in for one connection: replies `OK {}` (or `reply(line)`), returns the lines.
    fn fake(reply: fn(&str) -> Option<String>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut received = Vec::new();
            for line in BufReader::new(stream).lines() {
                let line = line.unwrap();
                match reply(&line) {
                    Some(answer) => writeln!(writer, "{answer}").unwrap(),
                    None => break,
                }
                received.push(line);
            }
            received
        });
        (address, handle)
    }

    #[test]
    fn auth_state_out_and_commands() {
        let (address, server) = fake(|line| {
            Some(match line {
                "STATE" => r#"OK {"layers":[{"layer":"main","page":"1001"},{"layer":"bug","page":{"number":3001}}]}"#.into(),
                l if l.starts_with("TAKE 9") => "ERR no page".into(),
                _ => "OK {}".into(),
            })
        });
        let sent = send(&address, "secret", &["TAKE 9".into(), "UPDATE 1 a=b".into()], true).unwrap();
        assert_eq!(sent.cleared_pages, ["1001", "3001"]);
        assert_eq!(sent.replies[0], ("OUT main,bug".into(), "OK {}".into()));
        assert_eq!(sent.replies[1], ("TAKE 9".into(), "ERR no page".into()));
        assert_eq!(server.join().unwrap(), ["AUTH secret", "STATE", "OUT main,bug", "TAKE 9", "UPDATE 1 a=b"]);
    }

    #[test]
    fn rejected_auth_and_lost_connection() {
        let (address, server) = fake(|_| Some("ERR unauthorized".into()));
        assert_eq!(send(&address, "bad", &["TAKE 1".into()], false).unwrap_err(), "AUTH: ERR unauthorized");
        server.join().unwrap();

        let (address, server) = fake(|_| None);
        assert_eq!(send(&address, "", &["TAKE 1".into()], false).unwrap_err(), "connection closed");
        server.join().unwrap();

        assert!(send("127.0.0.1:9", "", &[], false).unwrap_err().starts_with("connect to 127.0.0.1:9: "));
    }

    #[test]
    fn silent_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let hold = thread::spawn(move || {
            // Accept, never reply, hang up after the client gave up.
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(REPLY_TIMEOUT + Duration::from_millis(500));
            drop(stream);
        });
        let started = std::time::Instant::now();
        assert_eq!(send(&address, "", &["TAKE 1".into()], false).unwrap_err(), "TAKE: no reply");
        assert!(started.elapsed() < REPLY_TIMEOUT + Duration::from_millis(400));
        hold.join().unwrap();
    }
}
