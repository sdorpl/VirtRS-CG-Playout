// Synchronization of VizRS pages with ErsatzTV script templates (DOKUMENTACJA §3).
// Queries VizRS for the current show (/api/show or seed file) and generates
// template files in the target directory (one .yml per page):
//   name: [Nazwa strony] (7303)
//   args:
//     - http://cg:7700/etv?channel=1&page=7303

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use crate::json::{self, Value};
use crate::net::{Url, connect, error, status_of};

/// A page fetched from VizRS.
#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub number: u64,
    pub name: String,
}

/// Generates the YAML content of an ErsatzTV script element for a VizRS page.
pub fn generate_template_yaml(name: &str, number: u64, channel: &str, cg_url: &str) -> String {
    let clean_cg_url = cg_url.trim_end_matches('/');
    format!(
        "# Generated from VizRS CG System page {number}\n\
         name: \"{name} ({number})\"\n\
         command: /vizrs/vizrs-etv-client\n\
         args:\n\
         \x20 - {clean_cg_url}/etv?channel={channel}&page={number}\n\
         z_index: 100\n\
         epg_entries: 3\n\
         start_seconds: 0\n\
         duration_seconds: 172800\n\
         pixel_format: rgba\n\
         format: packet\n"
    )
}

/// Parses pages from VizRS show JSON text.
pub fn parse_pages(json_text: &str) -> Result<Vec<PageInfo>, String> {
    let root = json::parse(json_text).map_err(|e| format!("invalid JSON: {e}"))?;
    let pages_val = root.get("pages").and_then(Value::as_array).ok_or_else(|| "missing or non-array 'pages' field".to_owned())?;

    let mut pages = Vec::new();
    for p in pages_val {
        let number = match p.get("number") {
            Some(Value::Number(n)) => *n as u64,
            Some(Value::String(s)) => s.parse().unwrap_or(0),
            _ => continue,
        };
        if number == 0 {
            continue;
        }
        let name = p.get("name").and_then(Value::as_str).unwrap_or("Strona").trim().to_owned();
        pages.push(PageInfo { number, name });
    }
    Ok(pages)
}

/// Queries VizRS HTTP API for the active show JSON.
pub fn fetch_show(api_url: &str, token: &str) -> io::Result<String> {
    let full_url = if api_url.ends_with("/api/show") { api_url.to_owned() } else { format!("{}/api/show", api_url.trim_end_matches('/')) };
    let url = Url::split(&full_url).ok_or_else(|| error(format!("bad URL: {full_url}")))?;
    let mut stream = connect(&url)?;

    let mut req = format!("GET {} HTTP/1.0\r\nHost: {}\r\nAccept: application/json\r\n", url.target(), url.netloc);
    if !token.is_empty() {
        req.push_str(&format!("X-Token: {token}\r\n"));
    }
    req.push_str("\r\n");

    stream.write_all(req.as_bytes())?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;

    let end = reply.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| error("no reply headers".into()))?;
    let head = String::from_utf8_lossy(&reply[..end]);
    let status = status_of(head.lines().next().unwrap_or(""));
    if status != "200" {
        return Err(error(format!("HTTP error {status} from VizRS: {}", String::from_utf8_lossy(&reply[end + 4..]))));
    }
    Ok(String::from_utf8_lossy(&reply[end + 4..]).into_owned())
}

/// Synchronizes templates into `output_dir` from VizRS show JSON text.
pub fn sync_templates(show_json: &str, output_dir: &Path, channel: &str, cg_url: &str, clean_old: bool) -> Result<usize, String> {
    let pages = parse_pages(show_json)?;
    fs::create_dir_all(output_dir).map_err(|e| format!("{}: {e}", output_dir.display()))?;

    if clean_old {
        if let Ok(entries) = fs::read_dir(output_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if file_name.starts_with("page-") && file_name.ends_with(".yml") {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }

    let mut count = 0;
    for page in &pages {
        let file_path = output_dir.join(format!("page-{}.yml", page.number));
        let content = generate_template_yaml(&page.name, page.number, channel, cg_url);
        fs::write(&file_path, content).map_err(|e| format!("{}: {e}", file_path.display()))?;
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_template_matches_user_requirement() {
        let yaml = generate_template_yaml("Ticker", 7303, "1", "http://cg:7700");
        assert!(yaml.contains("name: \"Ticker (7303)\""));
        assert!(yaml.contains("args:\n  - http://cg:7700/etv?channel=1&page=7303"));
    }

    #[test]
    fn parse_pages_from_show_json() {
        let json = r#"{
            "pages": [
                { "number": 1001, "name": "Belka – budżet" },
                { "number": 7303, "name": "Ticker pilny" }
            ]
        }"#;
        let pages = parse_pages(json).unwrap();
        assert_eq!(
            pages,
            vec![PageInfo { number: 1001, name: "Belka – budżet".into() }, PageInfo { number: 7303, name: "Ticker pilny".into() },]
        );
    }
}
