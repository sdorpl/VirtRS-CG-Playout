// ErsatzTV script graphics element -> VirtRS CG sidecar (binary
// vizrs-etv-client, etv/templates/graphics-elements/script/vizrs.yml).
//
// ErsatzTV starts this for every playout item: the template data (JSON) comes
// on stdin, ETV graphics packets go to stdout. It only forwards both ways, so
// the browser keeps running in the sidecar between items.
//
//     vizrs-etv-client http://cg:7700/etv?channel=1
//
// If the sidecar is not reachable it exits without output: the item plays
// without graphics. A static binary, so the ErsatzTV image needs no extra
// packages. Every session also brings the page elements of the folder in line
// with VizRS, in the background (src/sync.rs): this program runs where the
// ErsatzTV configuration is, and knows the element's channel from its URL.

use std::io::{self, Read, Write};
use std::thread;

use crate::net::{self, Url, connect, error, read_head, status_of};
use crate::sync::{self, SyncOptions, SyncReport};

pub const DEFAULT_URL: &str = "http://127.0.0.1:7700/etv";
const CHUNK: usize = 1 << 20;

fn log(message: impl std::fmt::Display) {
    let _ = writeln!(io::stderr(), "vizrs-etv: {message}");
}

/// `POST` of the template data; the stream positioned at the packets.
/// HTTP/1.0: the sidecar sends the packets as plain bytes until it closes.
fn open(url: &str, body: &[u8]) -> io::Result<std::net::TcpStream> {
    let url = Url::split(url).ok_or_else(|| error(format!("bad sidecar URL {url}")))?;
    let mut stream = connect(&url)?;
    let head = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        url.target(),
        url.netloc,
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    let line = read_head(&mut stream, "sidecar")?;
    let status = status_of(&line);
    if status != "200" {
        return Err(error(format!("sidecar replied {status}")));
    }
    // The stream lasts as long as the item: no read timeout.
    stream.set_read_timeout(None)?;
    Ok(stream)
}

/// Page list from the sidecar -> elements in the ErsatzTV folder, if it is here.
fn sync_pages(url: &str) -> io::Result<Option<SyncReport>> {
    let dir = sync::templates_dir(std::env::var("ETV_CONFIG_FOLDER").ok().as_deref());
    if !dir.is_dir() {
        return Ok(None);
    }
    let pages_url = sync::pages_url(url).ok_or_else(|| error(format!("bad sidecar URL {url}")))?;
    let pages = sync::parse_pages(&net::get(&pages_url, "")?).map_err(error)?;
    let element_url = sync::channel_url(url);
    sync::sync_templates(&pages, &dir, SyncOptions { element_url: &element_url, rewrite: false, prune: false }).map(Some)
}

/// The client: `vizrs-etv-client [url]`; always exits 0 (no graphics is no error).
pub fn run(url: &str) {
    let mut body = Vec::new();
    if let Err(e) = io::stdin().lock().read_to_end(&mut body) {
        log(e);
        return;
    }
    let sync_url = url.to_owned();
    let _ = thread::Builder::new().name("sync".into()).spawn(move || match sync_pages(&sync_url) {
        Ok(Some(report)) if report.changed() => log(format!("page elements: {report}")),
        Ok(_) => {}
        Err(e) => log(format!("page elements: {e}")),
    });
    let mut stream = match open(url, &body) {
        Ok(stream) => stream,
        Err(e) => {
            log(e);
            return;
        }
    };

    let mut out = io::stdout().lock();
    let mut buffer = vec![0u8; CHUNK];
    // Ends with the stream, or when ErsatzTV closes stdout.
    while let Ok(n @ 1..) = stream.read(&mut buffer) {
        if out.write_all(&buffer[..n]).and_then(|()| out.flush()).is_err() {
            break;
        }
    }
}
