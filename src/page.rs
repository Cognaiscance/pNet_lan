//! The address page on the record server.
//!
//! It lists this user's devices, then each contact's devices. A row can be
//! given an IPv4 or IPv6 address, or cleared. Saving writes the list and
//! bumps `updated_at`, which is what makes the push run. The page listens on
//! loopback only.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::directory::Directory;
use crate::store::{Store, StoreError};

pub const DEFAULT_HTTP_PORT: u16 = 8794;

pub fn http_port() -> Result<u16, String> {
    let text = match std::env::var("PNET_LAN_HTTP_PORT") {
        Ok(value) if !value.is_empty() => value,
        _ => DEFAULT_HTTP_PORT.to_string(),
    };
    let port: u16 = text
        .parse()
        .map_err(|_| format!("PNET_LAN_HTTP_PORT '{text}' is not a port"))?;
    if port == 0 {
        return Err("PNET_LAN_HTTP_PORT must not be 0".to_string());
    }
    Ok(port)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub device: [u8; 16],
    pub heading: String,
    pub alias: String,
    pub address: Option<IpAddr>,
}

/// Own devices first, then each contact's devices. The address is the one
/// stored for that device, if any.
pub fn rows(dir: &Directory, store: &Store) -> Vec<Row> {
    let mut out = Vec::new();
    for device in &dir.own_devices {
        out.push(Row {
            device: device.id,
            heading: "Your devices".to_string(),
            alias: device.alias.clone(),
            address: stored(store, &device.id),
        });
    }
    for contact in &dir.contacts {
        for device in &contact.devices {
            out.push(Row {
                device: device.id,
                heading: contact.alias.clone(),
                alias: device.alias.clone(),
                address: stored(store, &device.id),
            });
        }
    }
    out
}

fn stored(store: &Store, device: &[u8; 16]) -> Option<IpAddr> {
    store
        .assignments()
        .iter()
        .find(|row| row.device == *device)
        .and_then(|row| row.address)
}

pub fn render(updated_at: u64, listed: &[Row], error: Option<&str>) -> String {
    let mut body = String::new();
    body.push_str(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>pNet LAN</title></head><body>",
    );
    body.push_str("<h1>Addresses</h1>");
    body.push_str(&format!("<p>Updated at {updated_at}.</p>"));
    if let Some(error) = error {
        body.push_str("<p>");
        body.push_str(&escape(error));
        body.push_str("</p>");
    }
    body.push_str("<form method=\"post\" action=\"/\">");
    let mut heading = None;
    for row in listed {
        if heading.as_deref() != Some(row.heading.as_str()) {
            body.push_str("<h2>");
            body.push_str(&escape(&row.heading));
            body.push_str("</h2>");
            heading = Some(row.heading.clone());
        }
        let value = row.address.map(|addr| addr.to_string()).unwrap_or_default();
        body.push_str("<label>");
        body.push_str(&escape(&row.alias));
        body.push_str(" <input name=\"");
        body.push_str(&hex_id(&row.device));
        body.push_str("\" value=\"");
        body.push_str(&escape(&value));
        body.push_str("\"></label><br>");
    }
    body.push_str("<button type=\"submit\">Save</button></form></body></html>");
    body
}

/// Apply a saved form. A bad address writes nothing. An unknown device is
/// ignored. An empty field clears a stored address.
pub fn apply_form(
    store: &mut Store,
    dir: &Directory,
    fields: &[(String, String)],
    now_ms: u64,
) -> Result<(), StoreError> {
    let known = rows(dir, store);
    let mut changes = Vec::new();
    for (name, value) in fields {
        let Some(device) = parse_id(name) else {
            continue;
        };
        if !known.iter().any(|row| row.device == device) {
            continue;
        }
        let address = if value.is_empty() {
            None
        } else {
            let Ok(address) = value.parse::<IpAddr>() else {
                return Err(StoreError::Corrupt("bad address"));
            };
            Some(address)
        };
        let current = stored(store, &device);
        if current != address {
            changes.push((device, address));
        }
    }
    for (device, address) in changes {
        store.assign(device, address, now_ms)?;
    }
    Ok(())
}

pub fn form_fields(body: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    if body.is_empty() {
        return fields;
    }
    for pair in body.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        fields.push((percent_decode(name), percent_decode(value)));
    }
    fields
}

/// One HTTP request against the shared list. `GET /` renders it. `POST /`
/// saves it. Anything else is 404.
pub fn handle(
    method: &str,
    path: &str,
    body: &str,
    store: &mut Store,
    dir: &Directory,
    now_ms: u64,
) -> (u16, String) {
    if path != "/" {
        return (404, "Not found".to_string());
    }
    match method {
        "GET" => (200, render(store.updated_at(), &rows(dir, store), None)),
        "POST" => match apply_form(store, dir, &form_fields(body), now_ms) {
            Ok(()) => (200, render(store.updated_at(), &rows(dir, store), None)),
            Err(_) => (
                400,
                render(
                    store.updated_at(),
                    &rows(dir, store),
                    Some("That address is not an IP address."),
                ),
            ),
        },
        _ => (405, "Method not allowed".to_string()),
    }
}

pub fn serve(
    listener: TcpListener,
    store: Arc<Mutex<Store>>,
    dir: Arc<Mutex<Directory>>,
) -> Result<(), String> {
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => return Err(format!("page accept: {err}")),
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        if let Ok((method, path, body)) = read_request(&mut stream) {
            let now = unix_ms();
            let (status, page) = {
                let mut store = store.lock().map_err(|_| "address list lock".to_string())?;
                let dir = dir.lock().map_err(|_| "directory lock".to_string())?;
                handle(&method, &path, &body, &mut store, &dir, now)
            };
            let _ = write_response(&mut stream, status, &page);
        }
    }
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") || buf.len() > 65_536 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let Some((head, rest)) = text.split_once("\r\n\r\n") else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no header",
        ));
    };
    let mut lines = head.lines();
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
        .min(65_536);
    let mut body = rest.to_string();
    while body.len() < length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.push_str(&String::from_utf8_lossy(&chunk[..n]));
    }
    body.truncate(length);
    Ok((method, path, body))
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let bytes = body.as_bytes();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(bytes)?;
    Ok(())
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_id(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut id = [0u8; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(id)
}

fn percent_decode(text: &str) -> String {
    let text = text.replace('+', " ");
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{ContactRecord, DeviceRecord, Grade};
    use crate::push::needs_push;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn device(id: u8, alias: &str) -> DeviceRecord {
        DeviceRecord {
            id: [id; 16],
            alias: alias.into(),
            grade: Grade::Device,
            sg_rank: 0,
            apps: Vec::new(),
        }
    }

    fn directory() -> Directory {
        Directory {
            local_app_id: [9; 16],
            local_app_alias: "pnet-lan-server".into(),
            local_app_approved: true,
            token: [2; 16],
            local_device: [1; 16],
            owner_alias: "william".into(),
            owner_id: [7; 16],
            own_devices: vec![device(1, "n64"), device(3, "laptop <home>")],
            contacts: vec![ContactRecord {
                alias: "chad".into(),
                id: [6; 16],
                devices: vec![device(4, "phone")],
            }],
        }
    }

    fn open() -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!(
            "pnet-lan-page-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        (dir, store)
    }

    #[test]
    fn the_page_lists_own_devices_then_each_contact() {
        let (dir, mut store) = open();
        store.note_client([3; 16]).unwrap();
        store.set_received([3; 16], 40).unwrap();
        store
            .assign([3; 16], Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))), 40)
            .unwrap();
        let directory = directory();
        let (status, html) = handle("GET", "/", "", &mut store, &directory, 50);
        assert_eq!(status, 200);
        let own = html.find("Your devices").unwrap();
        let chad = html.find(">chad<").unwrap();
        let laptop = html.find("laptop &lt;home&gt;").unwrap();
        let phone = html.find("phone").unwrap();
        assert!(own < laptop && laptop < chad && chad < phone);
        assert!(html.contains("10.0.0.2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_an_address_bumps_the_list_and_needs_a_push() {
        let (dir, mut store) = open();
        store.note_client([3; 16]).unwrap();
        store.set_received([3; 16], 40).unwrap();
        let directory = directory();
        let phone = hex_id(&[4; 16]);
        let body = format!("{phone}=fd00%3A%3A2");
        let (status, html) = handle("POST", "/", &body, &mut store, &directory, 90);
        assert_eq!(status, 200);
        assert!(html.contains("fd00::2"));
        assert_eq!(store.updated_at(), 90);
        assert_eq!(
            store.assignments()[0].address,
            Some(IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2)))
        );
        assert!(needs_push(&store, &[3; 16]));
        let again = format!("{phone}=fd00%3A%3A2");
        let _ = handle("POST", "/", &again, &mut store, &directory, 120);
        assert_eq!(store.updated_at(), 90);
        let clear = format!("{phone}=");
        handle("POST", "/", &clear, &mut store, &directory, 130);
        assert_eq!(store.assignments()[0].address, None);
        assert_eq!(store.updated_at(), 130);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_address_does_not_change_the_list() {
        let (dir, mut store) = open();
        let directory = directory();
        let laptop = hex_id(&[3; 16]);
        let (status, html) = handle(
            "POST",
            "/",
            &format!("{laptop}=not-an-ip"),
            &mut store,
            &directory,
            80,
        );
        assert_eq!(status, 400);
        assert!(html.contains("That address is not an IP address."));
        assert!(store.assignments().is_empty());
        assert_eq!(store.updated_at(), 0);
        let (status, _) = handle("GET", "/nope", "", &mut store, &directory, 80);
        assert_eq!(status, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loopback_post_saves_the_address() {
        let (dir, store) = open();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let store = Arc::new(Mutex::new(store));
        let directory = Arc::new(Mutex::new(directory()));
        let page_store = Arc::clone(&store);
        let page_dir = Arc::clone(&directory);
        std::thread::spawn(move || {
            let _ = serve(listener, page_store, page_dir);
        });
        let laptop = hex_id(&[3; 16]);
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{laptop}=10.1.0.8",
            format!("{laptop}=10.1.0.8").len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("200"));
        assert!(response.contains("10.1.0.8"));
        let saved = store.lock().unwrap();
        assert_eq!(
            saved.assignments()[0].address,
            Some(IpAddr::V4(Ipv4Addr::new(10, 1, 0, 8)))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
