//! Start one pNet LAN process against the local node.
//!
//! The client registers as `pnet-lan-client` and keeps its own token. The
//! server registers as `pnet-lan-server`. It is the record on the rank-1
//! server-grade node and stands by on every other node. A standby does not
//! open a store. The record role serves the address page. The client opens a
//! TUN interface and forwards TCP and UDP through pNet.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use crate::directory::{choose_role, parse_get_data, record_server, role_summary, ProcessKind};
use crate::fetch::{answer_fetch, decode_assignment, fetch_request};
use crate::forward::{self, client_app, device_of_client, prepare_inbound, prepare_outbound};
use crate::node_api::{
    decode_push, default_push_port, load_token, save_token, token_path, NodeClient,
    DEFAULT_PNET_ADDR,
};
use crate::page;
use crate::push::{
    ack_message, client_targets, decode_ack, needs_push, note_confirmation, push_message,
    pushed_updated_at, RETRY,
};
use crate::store::{self, Store};
use crate::tun::{self, claim_for};

pub fn main_for(kind: ProcessKind) -> ExitCode {
    match run(kind) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}: {err}", kind.alias());
            ExitCode::FAILURE
        }
    }
}

fn run(kind: ProcessKind) -> Result<(), String> {
    let name = kind.alias();
    let pnet_addr = pnet_addr()?;
    let push_port = push_port(kind)?;
    let token_path = token_path(kind);

    let push = std::net::UdpSocket::bind(("127.0.0.1", push_port))
        .map_err(|err| format!("bind 127.0.0.1:{push_port}: {err}"))?;
    let _ = push.local_addr();
    let client =
        NodeClient::connect(pnet_addr).map_err(|err| format!("connect to {pnet_addr}: {err}"))?;

    eprintln!("{name}: registering with {pnet_addr}");
    let token = match client.register(name, push_port) {
        Ok(token) => {
            save_token(&token_path, &token).map_err(|err| format!("save token: {err}"))?;
            token
        }
        Err(err) => match load_token(&token_path) {
            Ok(Some(token)) => {
                eprintln!("{name}: register failed ({err}); using the saved token");
                token
            }
            Ok(None) => return Err(format!("register failed: {err}")),
            Err(load_err) => {
                return Err(format!("register failed: {err}; saved token: {load_err}"));
            }
        },
    };

    let reply = client
        .get_data(&token)
        .map_err(|err| format!("get-data failed: {err}"))?;
    let dir = parse_get_data(&reply).map_err(|err| err.to_string())?;
    if dir.local_app_alias != name {
        return Err(format!(
            "this token is for '{}', not {name}",
            dir.local_app_alias
        ));
    }
    let role = choose_role(&dir, kind).map_err(|err| err.to_string())?;

    println!("{name}: {role}");
    println!("{}", role_summary(role));
    if !dir.local_app_approved {
        println!("The local node has not approved this app yet.");
    }
    println!(
        "token {}… at {}",
        token_prefix(&token),
        token_path.display()
    );
    println!("Push port {push_port}. Stop with Ctrl-C.");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if role.opens_store() {
        let path = store::record_dir();
        let mut list = Store::open(&path).map_err(|err| format!("open address list: {err}"))?;
        list.note_clients_from(&dir)
            .map_err(|err| format!("note clients: {err}"))?;
        println!(
            "Address list {} ({} devices, updated_at {}).",
            path.display(),
            list.assignments().len(),
            list.updated_at()
        );
        let list = Arc::new(Mutex::new(list));
        let shared_dir = Arc::new(Mutex::new(dir.clone()));
        let http_port = page::http_port()?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", http_port))
            .map_err(|err| format!("bind 127.0.0.1:{http_port}: {err}"))?;
        println!("Address page http://127.0.0.1:{http_port}/");
        let page_list = Arc::clone(&list);
        let page_dir = Arc::clone(&shared_dir);
        std::thread::spawn(move || {
            if let Err(err) = page::serve(listener, page_list, page_dir) {
                eprintln!("address page: {err}");
            }
        });
        serve_record(&client, &push, &token, &list, &shared_dir)?;
    }
    if kind == ProcessKind::Client {
        serve_client(client, &push, &token, &dir)?;
    }
    std::thread::park();
    Ok(())
}

fn request_list(client: &NodeClient, token: &[u8; 16], dir: &crate::directory::Directory) {
    let Some((device, app)) = record_server(dir) else {
        println!("The rank-1 server is not running pnet-lan-server yet.");
        return;
    };
    match client.send(token, &device, &app, &fetch_request()) {
        Ok(()) => println!("Asked the server for the address list."),
        Err(err) => println!("Could not ask for the address list: {err}"),
    }
}

fn serve_client(
    client: NodeClient,
    push: &std::net::UdpSocket,
    token: &[u8; 16],
    dir: &crate::directory::Directory,
) -> Result<(), String> {
    let interface =
        Arc::new(tun::Tun::open("pnetlan").map_err(|err| format!("open tun pnetlan: {err}"))?);
    println!("TUN {} is open (fd {}).", interface.name, interface.fd());
    let client = Arc::new(client);
    let rows = Arc::new(Mutex::new(Vec::new()));
    let directory = Arc::new(Mutex::new(dir.clone()));
    let local_device = dir.local_device;
    let reader = Arc::clone(&interface);
    let sender = Arc::clone(&client);
    let outbound_rows = Arc::clone(&rows);
    let outbound_dir = Arc::clone(&directory);
    let outbound_token = *token;
    std::thread::spawn(move || {
        forward_outbound(
            reader,
            sender,
            outbound_rows,
            outbound_dir,
            local_device,
            outbound_token,
        );
    });
    request_list(&client, token, dir);
    let mut buf = [0u8; 65535];
    loop {
        let n = match push.recv(&mut buf) {
            Ok(n) => n,
            Err(err) => return Err(format!("push socket: {err}")),
        };
        let Some((sender, payload)) = decode_push(&buf[..n]) else {
            continue;
        };
        refresh_directory(&client, token, &directory);
        let current = directory.lock().map_err(|_| "directory lock".to_string())?;
        let mut saved = rows.lock().map_err(|_| "address list lock".to_string())?;
        if forward::unwrap_packet(payload).is_some() {
            if let Some(device) = device_of_client(&current, &sender) {
                if let Some(rewritten) = prepare_inbound(payload, &local_device, &device, &saved) {
                    if let Err(err) = interface.write_packet(&rewritten) {
                        eprintln!("could not write the packet: {err}");
                    }
                }
            }
            continue;
        }
        if let Some((_updated_at, list)) = decode_assignment(payload) {
            let claim = claim_for(&dir.local_device, &list);
            tun::apply(&interface.name, &claim)?;
            *saved = list;
            println!("{}", tun::describe(&claim));
        }
        drop(saved);
        drop(current);
        let Some(updated_at) = pushed_updated_at(payload) else {
            continue;
        };
        let Some((device, app)) = record_server(dir) else {
            continue;
        };
        if let Err(err) = client.send(token, &device, &app, &ack_message(updated_at)) {
            eprintln!("could not confirm the address list: {err}");
        } else {
            println!("Confirmed address list {updated_at}.");
        }
    }
}

fn forward_outbound(
    interface: Arc<tun::Tun>,
    client: Arc<NodeClient>,
    rows: Arc<Mutex<Vec<crate::store::Assignment>>>,
    dir: Arc<Mutex<crate::directory::Directory>>,
    local_device: [u8; 16],
    token: [u8; 16],
) {
    let mut buf = [0u8; 4094];
    loop {
        let n = match interface.read_packet(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(err) => {
                eprintln!("tun read: {err}");
                return;
            }
        };
        let (directory, saved) = {
            let Ok(directory) = dir.lock() else {
                continue;
            };
            let Ok(saved) = rows.lock() else {
                continue;
            };
            (directory.clone(), saved.clone())
        };
        let Some((device, payload)) = prepare_outbound(&buf[..n], &local_device, &saved) else {
            continue;
        };
        let Some(app) = client_app(&directory, &device) else {
            continue;
        };
        if let Err(err) = client.send(&token, &device, &app, &payload) {
            eprintln!("could not forward the packet: {err}");
        }
    }
}

fn refresh_directory(
    client: &NodeClient,
    token: &[u8; 16],
    dir: &Mutex<crate::directory::Directory>,
) {
    if let Ok(reply) = client.get_data(token) {
        if let Ok(fresh) = parse_get_data(&reply) {
            if let Ok(mut current) = dir.lock() {
                *current = fresh;
            }
        }
    }
}

fn serve_record(
    client: &NodeClient,
    push: &std::net::UdpSocket,
    token: &[u8; 16],
    list: &Arc<Mutex<Store>>,
    dir: &Arc<Mutex<crate::directory::Directory>>,
) -> Result<(), String> {
    push.set_read_timeout(Some(RETRY))
        .map_err(|err| format!("push socket: {err}"))?;
    let mut buf = [0u8; 65535];
    loop {
        {
            let mut list = list.lock().map_err(|_| "address list lock".to_string())?;
            let mut current = dir.lock().map_err(|_| "directory lock".to_string())?;
            if let Ok(reply) = client.get_data(token) {
                if let Ok(fresh) = parse_get_data(&reply) {
                    *current = fresh;
                    let _ = list.note_clients_from(&current);
                }
            }
            let message = push_message(list.updated_at(), list.assignments());
            for (device, app) in client_targets(&current) {
                if !needs_push(&list, &device) {
                    continue;
                }
                if let Err(err) = client.send(token, &device, &app, &message) {
                    eprintln!("could not push the address list: {err}");
                }
            }
        }
        let n = match push.recv(&mut buf) {
            Ok(n) => n,
            Err(err) if is_timeout(&err) => continue,
            Err(err) => return Err(format!("push socket: {err}")),
        };
        let Some((sender, payload)) = decode_push(&buf[..n]) else {
            continue;
        };
        let now = unix_ms();
        let mut list = list.lock().map_err(|_| "address list lock".to_string())?;
        let current = dir.lock().map_err(|_| "directory lock".to_string())?;
        if let Some(acked) = decode_ack(payload) {
            note_confirmation(&mut list, &current, &sender, acked, now)
                .map_err(|err| format!("confirm: {err}"))?;
            continue;
        }
        let reply = answer_fetch(&mut list, &current, &sender, now, payload)
            .map_err(|err| format!("answer fetch: {err}"))?;
        let Some(reply) = reply else {
            continue;
        };
        let Some(device) = current.own_devices.iter().find_map(|device| {
            device
                .apps
                .iter()
                .any(|app| app.id == sender)
                .then_some(device.id)
        }) else {
            continue;
        };
        if let Err(err) = client.send(token, &device, &sender, &reply) {
            eprintln!("could not send the address list: {err}");
        }
    }
}

fn is_timeout(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn pnet_addr() -> Result<SocketAddr, String> {
    let text = env_or("PNET_ADDR", DEFAULT_PNET_ADDR);
    let addr: SocketAddr = text
        .parse()
        .map_err(|_| format!("PNET_ADDR '{text}' is not a socket address"))?;
    if !addr.is_ipv4() {
        return Err("PNET_ADDR must be IPv4".to_string());
    }
    Ok(addr)
}

fn push_port(kind: ProcessKind) -> Result<u16, String> {
    let default = default_push_port(kind);
    let text = env_or("PNET_LAN_PUSH_PORT", &default.to_string());
    let port: u16 = text
        .parse()
        .map_err(|_| format!("PNET_LAN_PUSH_PORT '{text}' is not a port"))?;
    if port == 0 {
        return Err("PNET_LAN_PUSH_PORT must not be 0".to_string());
    }
    Ok(port)
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => value,
        _ => default.to_string(),
    }
}

fn token_prefix(token: &[u8; 16]) -> String {
    token
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
