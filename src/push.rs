//! The server pushes the address list to this user's clients and retries.
//!
//! A push is version 1, type `PUSH` (`0x03`), then the same body as a fetch
//! reply. The client confirms with `ACK` (`0x04`) and the `updated_at` it
//! received. The server sets `received_at` only when that matches the current
//! list, and only forward. A client that never confirms is tried again. One
//! that is not in the directory catches up with a fetch when it returns.

use crate::directory::{Directory, CLIENT_ALIAS};
use crate::fetch::{encode_typed, own_client_device, VERSION};
use crate::store::{Assignment, Store, StoreError};

pub const PUSH: u8 = 0x03;
pub const ACK: u8 = 0x04;

/// How long the server waits before sending the list again.
pub const RETRY: std::time::Duration = std::time::Duration::from_secs(1);

pub fn push_message(updated_at: u64, rows: &[Assignment]) -> Vec<u8> {
    encode_typed(PUSH, updated_at, rows)
}

pub fn ack_message(updated_at: u64) -> Vec<u8> {
    let mut buf = vec![VERSION, ACK];
    buf.extend_from_slice(&updated_at.to_be_bytes());
    buf
}

pub fn pushed_updated_at(payload: &[u8]) -> Option<u64> {
    if payload.len() < 10 || payload[0] != VERSION || payload[1] != PUSH {
        return None;
    }
    Some(u64::from_be_bytes(payload[2..10].try_into().ok()?))
}

pub fn decode_ack(payload: &[u8]) -> Option<u64> {
    if payload.len() != 10 || payload[0] != VERSION || payload[1] != ACK {
        return None;
    }
    Some(u64::from_be_bytes(payload[2..10].try_into().ok()?))
}

/// Own devices that have `pnet-lan-client` installed, with that app id.
pub fn client_targets(dir: &Directory) -> Vec<([u8; 16], [u8; 16])> {
    let mut targets = Vec::new();
    for device in &dir.own_devices {
        for app in &device.apps {
            if app.alias == CLIENT_ALIAS {
                targets.push((device.id, app.id));
            }
        }
    }
    targets
}

/// True when this installed client has not confirmed the current list.
pub fn needs_push(store: &Store, device: &[u8; 16]) -> bool {
    store.receipts().iter().any(|row| {
        row.device == *device && row.received_at.is_none_or(|at| at < store.updated_at())
    })
}

/// Record a confirmation from one of this user's clients.
///
/// An older `updated_at` does not count and does not move `received_at`.
pub fn note_confirmation(
    store: &mut Store,
    dir: &Directory,
    sender_app: &[u8; 16],
    acked_updated_at: u64,
    now_ms: u64,
) -> Result<bool, StoreError> {
    if acked_updated_at != store.updated_at() {
        return Ok(false);
    }
    let Some(device) = own_client_device(dir, sender_app) else {
        return Ok(false);
    };
    store.note_client(device)?;
    let current = store
        .receipts()
        .iter()
        .find(|row| row.device == device)
        .and_then(|row| row.received_at);
    let stamp = now_ms.max(store.updated_at());
    if current.is_none_or(|at| stamp > at) {
        store.set_received(device, stamp)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{AppRecord, ContactRecord, DeviceRecord, Grade};
    use std::net::IpAddr;

    fn app(id: u8, alias: &str) -> AppRecord {
        AppRecord {
            id: [id; 16],
            alias: alias.into(),
            approved: true,
        }
    }

    fn device(id: u8, apps: Vec<AppRecord>) -> DeviceRecord {
        DeviceRecord {
            id: [id; 16],
            alias: format!("n{id}"),
            grade: Grade::Device,
            sg_rank: 0,
            apps,
        }
    }

    fn directory() -> Directory {
        Directory {
            local_app_id: [9; 16],
            local_app_alias: "pnet-lan-server".into(),
            local_app_approved: true,
            token: [2; 16],
            local_device: [1; 16],
            owner_alias: "owner".into(),
            owner_id: [7; 16],
            own_devices: vec![
                device(3, vec![app(8, CLIENT_ALIAS)]),
                device(4, vec![app(7, CLIENT_ALIAS)]),
            ],
            contacts: vec![ContactRecord {
                alias: "chad".into(),
                id: [6; 16],
                devices: vec![device(5, vec![app(5, CLIENT_ALIAS)])],
            }],
        }
    }

    #[test]
    fn a_change_is_pushed_until_each_own_client_confirms() {
        let dir = std::env::temp_dir().join(format!(
            "pnet-lan-push-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        let directory = directory();
        store.note_clients_from(&directory).unwrap();
        store
            .assign(
                [3; 16],
                Some(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2))),
                100,
            )
            .unwrap();

        let targets = client_targets(&directory);
        assert_eq!(targets, vec![([3; 16], [8; 16]), ([4; 16], [7; 16])]);
        assert!(needs_push(&store, &[3; 16]));
        assert!(needs_push(&store, &[4; 16]));

        let message = push_message(store.updated_at(), store.assignments());
        assert_eq!(pushed_updated_at(&message), Some(100));
        let ack = ack_message(100);
        assert_eq!(decode_ack(&ack), Some(100));

        assert!(!note_confirmation(&mut store, &directory, &[8; 16], 50, 120).unwrap());
        assert_eq!(store.receipts()[0].received_at, None);
        assert!(needs_push(&store, &[3; 16]));

        assert!(note_confirmation(&mut store, &directory, &[8; 16], 100, 130).unwrap());
        assert_eq!(store.receipts()[0].received_at, Some(130));
        assert!(!needs_push(&store, &[3; 16]));
        assert!(needs_push(&store, &[4; 16]));

        assert!(note_confirmation(&mut store, &directory, &[8; 16], 100, 90).unwrap());
        assert_eq!(store.receipts()[0].received_at, Some(130));

        assert!(!note_confirmation(&mut store, &directory, &[5; 16], 100, 140).unwrap());
        assert!(store.receipts().iter().all(|row| row.device != [5; 16]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
