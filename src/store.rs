//! Address list for the server that holds it.
//!
//! The directory is `~/.pnet/pnet-lan/`. This file is `list`. Only the record
//! role opens it. An address is an optional IPv4 or IPv6 label for one device. The
//! list has one `updated_at`. Each of this user's devices that has the client
//! installed has its own `received_at`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::IpAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::directory::{Directory, CLIENT_ALIAS};

const MAGIC: &[u8; 4] = b"LAN1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub device: [u8; 16],
    pub address: Option<IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub device: [u8; 16],
    pub received_at: Option<u64>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Corrupt(&'static str),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(err) => write!(f, "{err}"),
            StoreError::Corrupt(reason) => write!(f, "address list is corrupt: {reason}"),
        }
    }
}

pub struct Store {
    path: PathBuf,
    updated_at: u64,
    assignments: Vec<Assignment>,
    receipts: Vec<Receipt>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir).map_err(StoreError::Io)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        let path = dir.join("list");
        if !path.exists() {
            let store = Self {
                path,
                updated_at: 0,
                assignments: Vec::new(),
                receipts: Vec::new(),
            };
            store.write()?;
            return Ok(store);
        }
        let bytes = fs::read(&path).map_err(StoreError::Io)?;
        let (updated_at, assignments, receipts) = decode(&bytes)?;
        Ok(Self {
            path,
            updated_at,
            assignments,
            receipts,
        })
    }

    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn receipts(&self) -> &[Receipt] {
        &self.receipts
    }

    /// Remember an own device that has the client installed. A new row starts
    /// with no `received_at`. This does not change the assignment list.
    pub fn note_client(&mut self, device: [u8; 16]) -> Result<(), StoreError> {
        if self.receipts.iter().any(|row| row.device == device) {
            return Ok(());
        }
        self.receipts.push(Receipt {
            device,
            received_at: None,
        });
        self.write()
    }

    /// Drop receipt rows for devices that no longer have the client.
    pub fn retain_clients(&mut self, devices: &[[u8; 16]]) -> Result<(), StoreError> {
        let before = self.receipts.len();
        self.receipts.retain(|row| devices.contains(&row.device));
        if self.receipts.len() != before {
            self.write()?;
        }
        Ok(())
    }

    pub fn note_clients_from(&mut self, dir: &Directory) -> Result<(), StoreError> {
        let mut devices = Vec::new();
        for device in &dir.own_devices {
            let installed = device.apps.iter().any(|app| app.alias == CLIENT_ALIAS);
            if installed {
                devices.push(device.id);
                self.note_client(device.id)?;
            }
        }
        self.retain_clients(&devices)
    }

    /// Set or clear the address for any device, own or contact. The same
    /// value leaves `updated_at` where it is.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn assign(
        &mut self,
        device: [u8; 16],
        address: Option<IpAddr>,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        if let Some(row) = self.assignments.iter_mut().find(|row| row.device == device) {
            if row.address == address {
                return Ok(());
            }
            row.address = address;
        } else {
            self.assignments.push(Assignment { device, address });
        }
        self.updated_at = now_ms;
        self.write()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_received(&mut self, device: [u8; 16], at_ms: u64) -> Result<bool, StoreError> {
        let Some(row) = self.receipts.iter_mut().find(|row| row.device == device) else {
            return Ok(false);
        };
        if row.received_at == Some(at_ms) {
            return Ok(true);
        }
        row.received_at = Some(at_ms);
        self.write()?;
        Ok(true)
    }

    fn write(&self) -> Result<(), StoreError> {
        let bytes = encode(self.updated_at, &self.assignments, &self.receipts);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(StoreError::Io)?;
        file.write_all(&bytes).map_err(StoreError::Io)?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
            .map_err(StoreError::Io)?;
        Ok(())
    }
}

fn encode(updated_at: u64, assignments: &[Assignment], receipts: &[Receipt]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&updated_at.to_be_bytes());
    buf.extend_from_slice(&(assignments.len() as u32).to_be_bytes());
    for row in assignments {
        buf.extend_from_slice(&row.device);
        push_address(&mut buf, row.address);
    }
    buf.extend_from_slice(&(receipts.len() as u32).to_be_bytes());
    for row in receipts {
        buf.extend_from_slice(&row.device);
        match row.received_at {
            None => buf.push(0),
            Some(at) => {
                buf.push(1);
                buf.extend_from_slice(&at.to_be_bytes());
            }
        }
    }
    buf
}

fn decode(bytes: &[u8]) -> Result<(u64, Vec<Assignment>, Vec<Receipt>), StoreError> {
    if bytes.len() < 4 + 8 + 4 || &bytes[..4] != MAGIC {
        return Err(StoreError::Corrupt("bad header"));
    }
    let mut pos = 4;
    let updated_at = read_u64(bytes, &mut pos)?;
    let count = read_u32(bytes, &mut pos)? as usize;
    let mut assignments = Vec::with_capacity(count);
    for _ in 0..count {
        let device = read_id(bytes, &mut pos)?;
        let address =
            take_address(bytes, &mut pos).map_err(|_| StoreError::Corrupt("bad address"))?;
        assignments.push(Assignment { device, address });
    }
    let count = read_u32(bytes, &mut pos)? as usize;
    let mut receipts = Vec::with_capacity(count);
    for _ in 0..count {
        let device = read_id(bytes, &mut pos)?;
        let kind = read_u8(bytes, &mut pos)?;
        let received_at = match kind {
            0 => None,
            1 => Some(read_u64(bytes, &mut pos)?),
            _ => return Err(StoreError::Corrupt("bad receipt")),
        };
        receipts.push(Receipt {
            device,
            received_at,
        });
    }
    if pos != bytes.len() {
        return Err(StoreError::Corrupt("trailing bytes"));
    }
    Ok((updated_at, assignments, receipts))
}

/// `0` is no address, `4` is IPv4, `16` is IPv6.
pub fn push_address(buf: &mut Vec<u8>, address: Option<IpAddr>) {
    match address {
        None => buf.push(0),
        Some(IpAddr::V4(addr)) => {
            buf.push(4);
            buf.extend_from_slice(&addr.octets());
        }
        Some(IpAddr::V6(addr)) => {
            buf.push(16);
            buf.extend_from_slice(&addr.octets());
        }
    }
}

pub fn take_address(bytes: &[u8], pos: &mut usize) -> Result<Option<IpAddr>, ()> {
    let kind = *bytes.get(*pos).ok_or(())?;
    *pos += 1;
    match kind {
        0 => Ok(None),
        4 => {
            let end = pos.checked_add(4).ok_or(())?;
            let octets: [u8; 4] = bytes.get(*pos..end).ok_or(())?.try_into().map_err(|_| ())?;
            *pos = end;
            Ok(Some(IpAddr::V4(octets.into())))
        }
        16 => {
            let end = pos.checked_add(16).ok_or(())?;
            let octets: [u8; 16] = bytes.get(*pos..end).ok_or(())?.try_into().map_err(|_| ())?;
            *pos = end;
            Ok(Some(IpAddr::V6(octets.into())))
        }
        _ => Err(()),
    }
}

fn read_u8(bytes: &[u8], pos: &mut usize) -> Result<u8, StoreError> {
    let byte = *bytes.get(*pos).ok_or(StoreError::Corrupt("truncated"))?;
    *pos += 1;
    Ok(byte)
}

fn read_n<const N: usize>(bytes: &[u8], pos: &mut usize) -> Result<[u8; N], StoreError> {
    let end = pos.checked_add(N).ok_or(StoreError::Corrupt("truncated"))?;
    let slice = bytes
        .get(*pos..end)
        .ok_or(StoreError::Corrupt("truncated"))?;
    let array = slice
        .try_into()
        .map_err(|_| StoreError::Corrupt("truncated"))?;
    *pos = end;
    Ok(array)
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, StoreError> {
    Ok(u32::from_be_bytes(read_n(bytes, pos)?))
}

fn read_u64(bytes: &[u8], pos: &mut usize) -> Result<u64, StoreError> {
    Ok(u64::from_be_bytes(read_n(bytes, pos)?))
}

fn read_id(bytes: &[u8], pos: &mut usize) -> Result<[u8; 16], StoreError> {
    read_n(bytes, pos)
}

pub fn record_dir() -> PathBuf {
    if let Ok(path) = std::env::var("PNET_LAN_DIR") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".pnet/pnet-lan");
        }
    }
    PathBuf::from(".pnet/pnet-lan")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{AppRecord, DeviceRecord, Directory, Grade};
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pnet-lan-list-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn an_empty_list_reopens_empty() {
        let dir = temp_dir();
        let store = Store::open(&dir).unwrap();
        assert_eq!(store.updated_at(), 0);
        assert!(store.assignments().is_empty());
        assert!(store.receipts().is_empty());
        drop(store);
        let again = Store::open(&dir).unwrap();
        assert_eq!(again.updated_at(), 0);
        assert!(again.assignments().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn addresses_for_own_and_contact_devices_reopen() {
        let dir = temp_dir();
        let own = [1u8; 16];
        let contact = [2u8; 16];
        let v6 = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2));
        let mut store = Store::open(&dir).unwrap();
        store
            .assign(own, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))), 100)
            .unwrap();
        store.assign(contact, Some(v6), 200).unwrap();
        assert_eq!(store.updated_at(), 200);
        store
            .assign(own, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))), 300)
            .unwrap();
        assert_eq!(store.updated_at(), 200);
        drop(store);
        let again = Store::open(&dir).unwrap();
        assert_eq!(again.updated_at(), 200);
        assert_eq!(
            again.assignments(),
            &[
                Assignment {
                    device: own,
                    address: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))),
                },
                Assignment {
                    device: contact,
                    address: Some(v6),
                },
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn received_at_follows_installed_clients() {
        let dir = temp_dir();
        let laptop = [3u8; 16];
        let phone = [4u8; 16];
        let mut store = Store::open(&dir).unwrap();
        store.note_client(laptop).unwrap();
        assert!(store.set_received(laptop, 50).unwrap());
        assert!(!store.set_received(phone, 60).unwrap());
        assert_eq!(store.updated_at(), 0);
        store.retain_clients(&[]).unwrap();
        assert!(store.receipts().is_empty());
        drop(store);
        let again = Store::open(&dir).unwrap();
        assert!(again.receipts().is_empty());
        assert_eq!(again.updated_at(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_directory_selects_own_clients() {
        let dir = temp_dir();
        let mut store = Store::open(&dir).unwrap();
        let laptop = [5u8; 16];
        let directory = Directory {
            local_app_id: [9; 16],
            local_app_alias: "pnet-lan-server".into(),
            local_app_approved: true,
            token: [2; 16],
            local_device: [1; 16],
            owner_alias: "owner".into(),
            owner_id: [4; 16],
            own_devices: vec![DeviceRecord {
                id: laptop,
                alias: "laptop".into(),
                grade: Grade::Device,
                sg_rank: 0,
                apps: vec![AppRecord {
                    id: [8; 16],
                    alias: CLIENT_ALIAS.into(),
                    approved: false,
                }],
            }],
            contacts: Vec::new(),
        };
        store.note_clients_from(&directory).unwrap();
        assert_eq!(store.receipts().len(), 1);
        assert_eq!(store.receipts()[0].device, laptop);
        assert_eq!(store.receipts()[0].received_at, None);
        let _ = fs::remove_dir_all(&dir);
    }
}
