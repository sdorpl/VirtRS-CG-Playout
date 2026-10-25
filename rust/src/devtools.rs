// Minimal Chrome DevTools Protocol client over `--remote-debugging-pipe`:
// JSON messages ended by a NUL byte, Chromium reads fd 3 and writes fd 4.
// Flat sessions (`sessionId` on every message), as in vizrs_capture.rs of
// VizRS ffPlayout System, without a WebSocket.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::json::{self, Value};

/// A DevTools event (a message with `method`), or the end of the pipe.
pub enum Incoming {
    Event(Value),
    Closed,
}

type Reply = Result<Value, String>;

pub struct DevTools {
    writer: Mutex<Box<dyn Write + Send>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>>,
}

impl DevTools {
    /// Reads messages on its own thread: replies go to their call, events to `events`.
    pub fn start(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static, events: Sender<Incoming>) -> Arc<DevTools> {
        let pending: Arc<Mutex<HashMap<u64, Sender<Reply>>>> = Arc::default();
        let replies = Arc::clone(&pending);
        let _ = thread::Builder::new().name("devtools".into()).spawn(move || {
            let mut reader = BufReader::with_capacity(1 << 20, reader);
            let mut message = Vec::new();
            loop {
                message.clear();
                match reader.read_until(0, &mut message) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if message.last() == Some(&0) {
                    message.pop();
                }
                let Ok(value) =
                    std::str::from_utf8(&message).map_err(|e| e.to_string()).and_then(|t| json::parse(t).map_err(|e| e.to_string()))
                else {
                    continue;
                };
                match value.get("id").and_then(Value::as_f64) {
                    Some(id) => {
                        let waiting = replies.lock().unwrap_or_else(|e| e.into_inner()).remove(&(id as u64));
                        if let Some(waiting) = waiting {
                            let reply = match value.get("error") {
                                Some(error) => Err(error.get("message").and_then(Value::as_str).unwrap_or("error").to_owned()),
                                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = waiting.send(reply);
                        }
                    }
                    // The reader of the events is gone: nobody listens any more.
                    None if value.get("method").is_some() && events.send(Incoming::Event(value)).is_err() => break,
                    None => {}
                }
            }
            // Calls still waiting fail now instead of at their timeout.
            replies.lock().unwrap_or_else(|e| e.into_inner()).clear();
            let _ = events.send(Incoming::Closed);
        });
        Arc::new(DevTools { writer: Mutex::new(Box::new(writer)), next_id: AtomicU64::new(0), pending })
    }

    fn write(&self, id: u64, method: &str, params: Value, session: Option<&str>) -> Result<(), String> {
        let mut message = json::object([("id", id.into()), ("method", method.into()), ("params", params)]);
        if let (Some(session), Value::Object(members)) = (session, &mut message) {
            members.push(("sessionId".into(), session.into()));
        }
        let mut bytes = message.to_string().into_bytes();
        bytes.push(0);
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        writer.write_all(&bytes).and_then(|()| writer.flush()).map_err(|e| format!("{method}: DevTools closed ({e})"))
    }

    /// Sends without waiting for the reply.
    pub fn send(&self, method: &str, params: Value, session: Option<&str>) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.write(id, method, params, session)
    }

    /// Sends and waits for the reply (`result`), or fails with the DevTools error.
    pub fn call(&self, method: &str, params: Value, session: Option<&str>, timeout: Duration) -> Reply {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (reply, result) = mpsc::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, reply);
        if let Err(error) = self.write(id, method, params, session) {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(error);
        }
        match result.recv_timeout(timeout) {
            Ok(reply) => reply.map_err(|e| format!("{method}: {e}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Err(format!("{method}: no reply"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!("{method}: DevTools closed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_events_and_end_of_pipe() {
        let (to_client, from_browser) = std::io::pipe().unwrap();
        let (from_client, to_browser) = std::io::pipe().unwrap();
        let (events, incoming) = mpsc::channel();
        let devtools = DevTools::start(to_client, to_browser, events);

        // Browser stand-in: answers the first call, sends an event, closes.
        let browser = thread::spawn(move || {
            let mut reader = BufReader::new(from_client);
            let mut message = Vec::new();
            reader.read_until(0, &mut message).unwrap();
            message.pop();
            let call = json::parse(std::str::from_utf8(&message).unwrap()).unwrap();
            let mut writer = from_browser;
            let id = call.get("id").unwrap().to_string();
            writer.write_all(br#"{"method":"Page.screencastFrame","params":{"data":"x"}}"#).unwrap();
            writer.write_all(b"\0").unwrap();
            writer.write_all(format!(r#"{{"id":{id},"result":{{"ok":true}}}}"#).as_bytes()).unwrap();
            writer.write_all(b"\0").unwrap();
            call
        });

        let result = devtools.call("Target.getTargets", json::object([]), Some("s1"), Duration::from_secs(5)).unwrap();
        assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
        let call = browser.join().unwrap();
        assert_eq!(call.get("sessionId").and_then(Value::as_str), Some("s1"));
        assert!(
            matches!(incoming.recv().unwrap(), Incoming::Event(e) if e.get("method").and_then(Value::as_str) == Some("Page.screencastFrame"))
        );
        assert!(matches!(incoming.recv().unwrap(), Incoming::Closed));
        assert!(devtools.call("Page.enable", json::object([]), None, Duration::from_secs(1)).is_err());
    }
}
