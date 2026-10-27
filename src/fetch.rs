//! A client asks for the address list. The server answers only this user's clients.
//!
//! The request is version 1, type `FETCH` (`0x01`). The reply is version 1,
//! type `LIST` (`0x02`), then `updated_at` and each device's optional IPv4 or
//! IPv6 address. A later fetch with an earlier time does not move `received_at`
//! backward.

use crate::directory::{Directory, CLIENT_ALIAS};
use crate::store::{push_address, take_address, Assignment, Store, StoreError};

pub const VERSION: u8 = 1;
pub const FETCH: u8 = 0x01;
pub const LIST: u8 = 0x02;

pub fn fetch_request() -> Vec<u8> {
    vec![VERSION, FETCH]
}

pub fn encode_list(updated_at: u64, rows: &[Assignment]) -> Vec<u8> {
    let mut buf = vec![VERSION, LIST];
    buf.extend_from_slice(&updated_at.to_be_bytes());
    let count = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    buf.extend_from_slice(&count.to_be_bytes());
    for row in rows.iter().take(count as usize) {
        buf.extend_from_slice(&row.device);
        push_address(&mut buf, row.address);
    }
    buf
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn decode_list(payload: &[u8]) -> Option<(u64, Vec<Assignment>)> {
    if payload.len() < 2 + 8 + 2 || payload[0] != VERSION || payload[1] != LIST {
        return None;
    }
    let mut pos = 2;
    let updated_at = u64::from_be_bytes(payload.get(pos..pos + 8)?.try_into().ok()?);
    pos += 8;
    let count = u16::from_be_bytes(payload.get(pos..pos + 2)?.try_into().ok()?) as usize;
    pos += 2;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let device: [u8; 16] = payload.get(pos..pos + 16)?.try_into().ok()?;
        pos += 16;
        let address = take_address(payload, &mut pos).ok()?;
        rows.push(Assignment { device, address });
    }
    if pos != payload.len() {
        return None;
    }
    Some((updated_at, rows))
}

/// Own device that runs `pnet-lan-client` for this app id.
pub fn own_client_device(dir: &Directory, app: &[u8; 16]) -> Option<[u8; 16]> {
    dir.own_devices.iter().find_map(|device| {
        device
            .apps
            .iter()
            .any(|candidate| candidate.id == *app && candidate.alias == CLIENT_ALIAS)
            .then_some(device.id)
    })
}

/// Answer a fetch from one of this user's clients. A contact gets no reply.
/// `received_at` moves only forward.
pub fn answer_fetch(
    store: &mut Store,
    dir: &Directory,
    sender_app: &[u8; 16],
    now_ms: u64,
    request: &[u8],
) -> Result<Option<Vec<u8>>, StoreError> {
    if request != fetch_request() {
        return Ok(None);
    }
    let Some(device) = own_client_device(dir, sender_app) else {
        return Ok(None);
    };
    store.note_client(device)?;
    let current = store
        .receipts()
        .iter()
        .find(|row| row.device == device)
        .and_then(|row| row.received_at);
    if current.is_none_or(|at| now_ms > at) {
        store.set_received(device, now_ms)?;
    }
    Ok(Some(encode_list(store.updated_at(), store.assignments())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{AppRecord, ContactRecord, DeviceRecord, Grade};
    use crate::store::Receipt;
    use std::net::{IpAddr, Ipv4Addr};

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
            own_devices: vec![device(3, vec![app(8, CLIENT_ALIAS)])],
            contacts: vec![ContactRecord {
                alias: "chad".into(),
                id: [6; 16],
                devices: vec![device(4, vec![app(5, CLIENT_ALIAS)])],
            }],
        }
    }

    #[test]
    fn an_own_client_gets_the_list_and_stamps_receipt() {
        let dir = std::env::temp_dir().join(format!("pnet-lan-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        let laptop = [3u8; 16];
        let v6 = IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        store
            .assign(laptop, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))), 10)
            .unwrap();
        store.assign([9; 16], Some(v6), 11).unwrap();
        let directory = directory();
        let reply = answer_fetch(&mut store, &directory, &[8; 16], 40, &fetch_request())
            .unwrap()
            .unwrap();
        let (updated, rows) = decode_list(&reply).unwrap();
        assert_eq!(updated, 11);
        assert_eq!(
            rows[0].address,
            Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)))
        );
        assert_eq!(rows[1].address, Some(v6));
        assert_eq!(
            store.receipts(),
            &[Receipt {
                device: laptop,
                received_at: Some(40),
            }]
        );
        let again = answer_fetch(&mut store, &directory, &[8; 16], 30, &fetch_request())
            .unwrap()
            .unwrap();
        assert!(decode_list(&again).is_some());
        assert_eq!(store.receipts()[0].received_at, Some(40));
        answer_fetch(&mut store, &directory, &[8; 16], 50, &fetch_request()).unwrap();
        assert_eq!(store.receipts()[0].received_at, Some(50));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_contact_client_is_not_answered() {
        let dir = std::env::temp_dir().join(format!("pnet-lan-fetch-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        let directory = directory();
        let reply = answer_fetch(&mut store, &directory, &[5; 16], 40, &fetch_request()).unwrap();
        assert!(reply.is_none());
        assert!(store.receipts().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
