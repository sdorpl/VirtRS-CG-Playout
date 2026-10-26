// VizRS TCP client (line protocol, default port 6100) and the per-channel
// logic of clip and ad events: ads are played without graphics, the pages
// taken out come back with the first regular clip. A page element of its own
// (`page=`/`take=`) goes on air `page_in_seconds` after its item's start and
// out `page_out_seconds` before its end.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config::Config;
use crate::json::{self, Value};
use crate::template::{AdMatcher, ClipContext, ShowMatcher, js_string, render_commands};
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
    /// The item of the latest session (or the ad after it), for the gap check.
    last_item: Option<String>,
    /// Page elements scheduled, with their item's stop (epoch ms): the next
    /// pieces of the same item do not schedule them again.
    pages: Vec<(String, i64)>,
    active: usize,
    /// Bumped by every session start and end: a pending gap check of an older
    /// generation does nothing (the `clearTimeout` of the Node.js build).
    gap_generation: u64,
}

/// Clip events of one channel, sent in order.
pub struct ChannelEvents {
    config: Arc<Config>,
    ads: Arc<AdMatcher>,
    shows: Arc<ShowMatcher>,
    state: Mutex<State>,
    queue: Mutex<Sender<Batch>>,
}

impl ChannelEvents {
    pub fn new(config: Arc<Config>, ads: Arc<AdMatcher>, shows: Arc<ShowMatcher>) -> Arc<ChannelEvents> {
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
            shows,
            state: Mutex::new(State { last_key: None, last_item: None, pages: Vec::new(), active: 0, gap_generation: 0 }),
            queue: Mutex::new(queue),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sender(&self) -> Sender<Batch> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn enqueue(&self, batch: Batch) {
        let _ = self.sender().send(batch);
    }

    /// A script session of the channel starts; call `ended` when it is over.
    /// `custom_commands` overrides on_clip for this specific session (e.g. from ?take=ID&fields=...);
    /// with `page` (the pages of `?page=`/`?take=`) they are timed to the item and taken out before its end.
    pub fn session(&self, ctx: &ClipContext, custom_commands: Option<&[String]>, page: Option<&str>) {
        {
            let mut state = self.state();
            state.active += 1;
            state.gap_generation += 1;
            state.last_item = Some(format!("{}|{}", ctx.source, ctx.start));
        }
        self.clip(ctx, custom_commands, page);
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
                state.gap_generation == generation && state.active == 0 && state.last_item.as_deref() == Some(key.as_str())
            };
            if gap {
                events.ad(&ctx);
            }
        });
    }

    /// An item without the element: graphics out and `on_ad`, as for a matched ad.
    fn ad(&self, ctx: &ClipContext) {
        {
            let mut state = self.state();
            state.last_key = Some(format!("ad|{}", ctx.stop));
            state.last_item = state.last_key.clone();
        }
        let ad_ctx = ClipContext { title: String::new(), show_title: String::new(), source: String::new(), duration: 0.0, ..ctx.clone() };
        let show = self.shows.find(&ctx.show_title);
        let on_ad = show.and_then(|s| s.on_ad.as_deref()).unwrap_or(&self.config.on_ad);
        self.enqueue(Batch { ad: true, commands: render_commands(on_ad, &ad_ctx) });
    }

    /// Events for a clip that starts (or is joined) now. ErsatzTV starts the
    /// script again when a new session joins the same item; that does not send
    /// the commands twice (and does not restart the animations).
    fn clip(&self, ctx: &ClipContext, custom_commands: Option<&[String]>, page: Option<&str>) {
        let key = match custom_commands {
            Some(cmds) => format!("{}|{}|{}", ctx.source, ctx.start, cmds.join(";")),
            None => format!("{}|{}", ctx.source, ctx.start),
        };
        if let (Some(page), Some(cmds)) = (page, custom_commands) {
            return self.page(ctx, key, cmds, page);
        }
        {
            let mut state = self.state();
            if state.last_key.as_deref() == Some(key.as_str()) {
                return;
            }
            state.last_key = Some(key);
        }
        let show = self.shows.find(&ctx.show_title);
        let ad = self.ads_for(ctx);
        let commands = if ad {
            let on_ad = show.and_then(|s| s.on_ad.as_deref()).unwrap_or(&self.config.on_ad);
            render_commands(on_ad, ctx)
        } else if let Some(custom) = custom_commands {
            custom.iter().flat_map(|c| render_commands(c, ctx)).collect()
        } else {
            let on_clip = show.and_then(|s| s.on_clip.as_deref()).unwrap_or(&self.config.on_clip);
            render_commands(on_clip, ctx)
        };
        self.enqueue(Batch { ad, commands });
    }

    /// A page element of its own: `TAKE` `page_in_seconds` after the item's
    /// start, `OUT` `page_out_seconds` before its stop (joined late: at once).
    /// Several page elements on one item are scheduled once each.
    fn page(&self, ctx: &ClipContext, key: String, commands: &[String], page: &str) {
        let ad = self.ads_for(ctx);
        let stop = time::parse_iso(&ctx.stop);
        {
            let mut state = self.state();
            let now = time::now_ms();
            state.pages.retain(|(_, stop)| *stop > now);
            if state.pages.iter().any(|(k, _)| *k == key) {
                return;
            }
            state.pages.push((key, stop.unwrap_or(i64::MAX)));
        }
        if ad {
            return self.clip(ctx, None, None);
        }

        let commands: Vec<String> = commands.iter().flat_map(|c| render_commands(c, ctx)).collect();
        let seconds = |s: f64| (s.max(0.0) * 1000.0) as i64;
        let (Some(start), Some(stop)) = (time::parse_iso(&ctx.start), stop) else {
            // No schedule times: on air at once, out with the next ad.
            return self.enqueue(Batch { ad: false, commands });
        };
        let take_at = start + seconds(self.config.page_in_seconds);
        let out_at = stop - seconds(self.config.page_out_seconds);
        let now = time::now_ms();
        if take_at >= out_at || now >= out_at {
            crate::debug!("VizRS page {page}: item too short or ending, not taken");
            return;
        }

        let queue = self.sender();
        let out = format!("OUT {page}");
        let _ = thread::Builder::new().name("vizrs-page".into()).spawn(move || {
            let wait_until = |at: i64| thread::sleep(Duration::from_millis((at - time::now_ms()).max(0) as u64));
            wait_until(take_at);
            if queue.send(Batch { ad: false, commands }).is_err() {
                return;
            }
            wait_until(out_at);
            let _ = queue.send(Batch { ad: false, commands: vec![out] });
        });
    }

    /// Whether the item is an ad, by the show's rule or the global one.
    fn ads_for(&self, ctx: &ClipContext) -> bool {
        match self.shows.find(&ctx.show_title).and_then(|s| s.ad_matcher.as_ref()) {
            Some(matcher) => matcher.is_ad(ctx),
            None => self.ads.is_ad(ctx),
        }
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

    /// VizRS stand-in accepting multiple connections: replies `OK {}`, receives lines until `expected` lines arrive.
    fn fake_multi(expected: usize) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let handle = thread::spawn(move || {
            let mut received = Vec::new();
            while received.len() < expected {
                if let Ok((stream, _)) = listener.accept() {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { break };
                        if line == "STATE" {
                            let _ = writeln!(writer, "OK {{\"layers\":[]}}");
                        } else {
                            let _ = writeln!(writer, "OK {{}}");
                        }
                        received.push(line);
                        if received.len() >= expected {
                            break;
                        }
                    }
                }
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

    #[test]
    fn channel_events_per_show_overrides() {
        use crate::config::{AdMatch, ShowOverride};

        // 3 commands + 1 STATE query on ad
        let (address, server) = fake_multi(4);
        let mut config = Config::default();
        config.vizrs.address = address;
        config.on_clip = "TAKE global {title}".into();
        config.on_ad = "TAKE ad_global".into();
        config.shows = vec![ShowOverride {
            show_title: "Wiadomości".into(),
            on_clip: Some("TAKE news {title}".into()),
            on_ad: Some("TAKE ad_news".into()),
            ad_match: Some(AdMatch { path: "ad_news".into(), title: "".into() }),
        }];

        let config = Arc::new(config);
        let (ads, _) = AdMatcher::new(&config.ad_match);
        let (shows, _) = ShowMatcher::new(&config.shows);
        let events = ChannelEvents::new(config, Arc::new(ads), Arc::new(shows));

        // Clip matching show "Wiadomości"
        let ctx_news = ClipContext {
            title: "Wydanie".into(),
            show_title: "Wiadomości".into(),
            source: "/m/news1.mp4".into(),
            start: "2026-10-25T12:00:00Z".into(),
            ..ClipContext::default()
        };
        events.clip(&ctx_news, None, None);

        // Clip not matching any show (uses global)
        let ctx_other = ClipContext {
            title: "Film".into(),
            show_title: "Kino".into(),
            source: "/m/movie.mp4".into(),
            start: "2026-10-25T13:00:00Z".into(),
            ..ClipContext::default()
        };
        events.clip(&ctx_other, None, None);

        // Ad matching show's ad_match
        let ctx_news_ad = ClipContext {
            title: "Blok".into(),
            show_title: "Wiadomości".into(),
            source: "/m/ad_news_1.mp4".into(),
            start: "2026-10-25T12:15:00Z".into(),
            ..ClipContext::default()
        };
        events.clip(&ctx_news_ad, None, None);

        thread::sleep(Duration::from_millis(300));
        let received = server.join().unwrap();
        assert!(received.contains(&"TAKE news Wydanie".to_string()));
        assert!(received.contains(&"TAKE global Film".to_string()));
        assert!(received.contains(&"TAKE ad_news".to_string()));
    }

    #[test]
    fn page_elements_in_after_start_out_before_stop() {
        let (address, server) = fake_multi(4);
        let mut config = Config::default();
        config.vizrs.address = address;
        config.page_in_seconds = 0.3;
        config.page_out_seconds = 0.3;
        let config = Arc::new(config);
        let (ads, _) = AdMatcher::new(&config.ad_match);
        let (shows, _) = ShowMatcher::new(&config.shows);
        let events = ChannelEvents::new(config, Arc::new(ads), Arc::new(shows));

        let now = time::now_ms();
        let item = |source: &str, length: i64| ClipContext {
            source: source.into(),
            start: time::iso_time(now),
            stop: time::iso_time(now + length),
            ..ClipContext::default()
        };
        let take = |page: &str| vec![format!("TAKE {page}")];
        let clip = item("/m/a.mp4", 1200);
        events.clip(&clip, Some(&take("3001")), Some("3001"));
        events.clip(&clip, Some(&take("7303")), Some("7303"));
        // The next piece of the same item: scheduled already.
        events.clip(&clip, Some(&take("3001")), Some("3001"));
        // Shorter than in + out: never on air.
        events.clip(&item("/m/b.mp4", 500), Some(&take("5001")), Some("5001"));

        thread::sleep(Duration::from_millis(150));
        assert!(!server.is_finished(), "nothing before page_in_seconds");
        let received = server.join().unwrap();
        let mut takes = received[..2].to_vec();
        takes.sort();
        assert_eq!(takes, ["TAKE 3001", "TAKE 7303"]);
        let mut outs = received[2..].to_vec();
        outs.sort();
        assert_eq!(outs, ["OUT 3001", "OUT 7303"]);
        assert!(time::now_ms() - now >= 900, "out page_out_seconds before the stop");
    }
}
