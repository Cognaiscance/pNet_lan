//! Start one pNet LAN process against the local node.
//!
//! The client registers as `pnet-lan-client` and keeps its own token. The
//! server registers as `pnet-lan-server`. It is the record on the rank-1
//! server-grade node and stands by on every other node. A standby does not
//! open a store. This step has no page and no TUN interface.

use std::net::SocketAddr;
use std::process::ExitCode;

use crate::directory::{choose_role, parse_get_data, role_summary, ProcessKind};
use crate::node_api::{
    default_push_port, load_token, save_token, token_path, NodeClient, DEFAULT_PNET_ADDR,
};
use crate::store::{self, Store};

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

    let _push = std::net::UdpSocket::bind(("127.0.0.1", push_port))
        .map_err(|err| format!("bind 127.0.0.1:{push_port}: {err}"))?;
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
    }
    std::thread::park();
    Ok(())
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
