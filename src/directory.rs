//! The local node's get-data tree, and the role this process takes from it.
//!
//! The binary decides client or server. The server holds the address list when
//! this device is the rank-1 server-grade node, and otherwise stands by.

use std::fmt;

pub const CLIENT_ALIAS: &str = "pnet-lan-client";
pub const SERVER_ALIAS: &str = "pnet-lan-server";

/// Rank of the primary server-grade node.
pub const PRIMARY_SG_RANK: u8 = 1;

/// Which program is running. The names are the aliases registered with pNet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessKind {
    Client,
    Server,
}

impl ProcessKind {
    pub fn alias(self) -> &'static str {
        match self {
            ProcessKind::Client => CLIENT_ALIAS,
            ProcessKind::Server => SERVER_ALIAS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    Device,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Client,
    Record,
    Standby,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Client => "client",
            Role::Record => "record",
            Role::Standby => "standby",
        }
    }

    /// The address list is opened only by the record role.
    pub fn opens_store(self) -> bool {
        matches!(self, Role::Record)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRecord {
    pub id: [u8; 16],
    pub alias: String,
    pub approved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRecord {
    pub id: [u8; 16],
    pub alias: String,
    pub grade: Grade,
    pub sg_rank: u8,
    pub apps: Vec<AppRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactRecord {
    pub alias: String,
    pub id: [u8; 16],
    pub devices: Vec<DeviceRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory {
    pub local_app_id: [u8; 16],
    pub local_app_alias: String,
    pub local_app_approved: bool,
    pub token: [u8; 16],
    pub local_device: [u8; 16],
    pub owner_alias: String,
    pub owner_id: [u8; 16],
    pub own_devices: Vec<DeviceRecord>,
    pub contacts: Vec<ContactRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Truncated,
    BadUtf8,
    BadGrade,
    Trailing,
    Status,
    Rejected { code: Option<u8> },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Truncated => f.write_str("truncated get-data reply"),
            ParseError::BadUtf8 => f.write_str("get-data string is not utf-8"),
            ParseError::BadGrade => f.write_str("device grade is not device or server"),
            ParseError::Trailing => f.write_str("get-data reply has trailing bytes"),
            ParseError::Status => f.write_str("get-data status is not success"),
            ParseError::Rejected { code: Some(code) } => {
                write!(f, "node rejected get-data ({code:#04x})")
            }
            ParseError::Rejected { code: None } => f.write_str("node rejected get-data"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleError {
    LocalDeviceMissing,
}

impl fmt::Display for RoleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoleError::LocalDeviceMissing => {
                f.write_str("local device is not in the get-data tree")
            }
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Result<u8, ParseError> {
        let byte = *self.data.get(self.pos).ok_or(ParseError::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ParseError> {
        let end = self.pos.checked_add(N).ok_or(ParseError::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(ParseError::Truncated)?;
        let array = slice.try_into().map_err(|_| ParseError::Truncated)?;
        self.pos = end;
        Ok(array)
    }

    fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        let end = self.pos.checked_add(n).ok_or(ParseError::Truncated)?;
        if end > self.data.len() {
            return Err(ParseError::Truncated);
        }
        self.pos = end;
        Ok(())
    }

    fn str(&mut self) -> Result<String, ParseError> {
        let len = self.u8()? as usize;
        let end = self.pos.checked_add(len).ok_or(ParseError::Truncated)?;
        let bytes = self.data.get(self.pos..end).ok_or(ParseError::Truncated)?;
        let text = std::str::from_utf8(bytes).map_err(|_| ParseError::BadUtf8)?;
        self.pos = end;
        Ok(text.to_string())
    }
}

/// Parse a get-data reply, including its leading status byte.
pub fn parse_get_data(reply: &[u8]) -> Result<Directory, ParseError> {
    let mut cur = Cursor {
        data: reply,
        pos: 0,
    };
    let status = cur.u8()?;
    if status == 0x01 {
        return Err(ParseError::Rejected {
            code: reply.get(cur.pos).copied(),
        });
    }
    if status != 0x00 {
        return Err(ParseError::Status);
    }

    let local_app_id = cur.array()?;
    let local_app_alias = cur.str()?;
    cur.skip(4 + 2)?;
    let local_app_approved = cur.u8()? != 0;
    let token = cur.array()?;
    let local_device = cur.array()?;
    let owner_alias = cur.str()?;
    let owner_id = cur.array()?;

    let device_count = cur.u8()? as usize;
    let mut own_devices = Vec::with_capacity(device_count);
    for _ in 0..device_count {
        let mut device = read_device(&mut cur)?;
        device.apps = read_apps(&mut cur, true)?;
        own_devices.push(device);
    }

    let contact_count = cur.u8()? as usize;
    let mut contacts = Vec::with_capacity(contact_count);
    for _ in 0..contact_count {
        let alias = cur.str()?;
        let id = cur.array()?;
        let device_count = cur.u8()? as usize;
        let mut devices = Vec::with_capacity(device_count);
        for _ in 0..device_count {
            let mut device = read_device(&mut cur)?;
            device.apps = read_apps(&mut cur, false)?;
            devices.push(device);
        }
        contacts.push(ContactRecord { alias, id, devices });
    }

    if cur.pos != reply.len() {
        return Err(ParseError::Trailing);
    }

    Ok(Directory {
        local_app_id,
        local_app_alias,
        local_app_approved,
        token,
        local_device,
        owner_alias,
        owner_id,
        own_devices,
        contacts,
    })
}

fn read_device(cur: &mut Cursor<'_>) -> Result<DeviceRecord, ParseError> {
    let id = cur.array()?;
    let alias = cur.str()?;
    let grade = match cur.u8()? {
        0 => Grade::Device,
        1 => Grade::Server,
        _ => return Err(ParseError::BadGrade),
    };
    let sg_rank = cur.u8()?;
    let host_count = cur.u8()?;
    for _ in 0..host_count {
        let _host = cur.str()?;
    }
    // signing key, X25519 key, certificate signature, issued-at.
    cur.skip(32 + 32 + 64 + 8)?;
    let _cert_alias = cur.str()?;
    Ok(DeviceRecord {
        id,
        alias,
        grade,
        sg_rank,
        apps: Vec::new(),
    })
}

fn read_apps(cur: &mut Cursor<'_>, with_host: bool) -> Result<Vec<AppRecord>, ParseError> {
    let count = cur.u8()? as usize;
    let mut apps = Vec::with_capacity(count);
    for _ in 0..count {
        let id = cur.array()?;
        let alias = cur.str()?;
        let approved = if with_host {
            cur.skip(4 + 2)?;
            cur.u8()? != 0
        } else {
            true
        };
        apps.push(AppRecord {
            id,
            alias,
            approved,
        });
    }
    Ok(apps)
}

/// True when this device is the primary server-grade node (rank 1).
pub fn is_primary_server(device: &DeviceRecord) -> bool {
    device.grade == Grade::Server && device.sg_rank == PRIMARY_SG_RANK
}

/// The rank-1 server-grade node and its `pnet-lan-server` app, if that app is listed.
pub fn record_server(dir: &Directory) -> Option<([u8; 16], [u8; 16])> {
    let device = dir
        .own_devices
        .iter()
        .find(|device| is_primary_server(device))?;
    let app = device.apps.iter().find(|app| app.alias == SERVER_ALIAS)?;
    Some((device.id, app.id))
}

pub fn choose_role(dir: &Directory, kind: ProcessKind) -> Result<Role, RoleError> {
    let local = dir
        .own_devices
        .iter()
        .find(|device| device.id == dir.local_device)
        .ok_or(RoleError::LocalDeviceMissing)?;
    match kind {
        ProcessKind::Client => Ok(Role::Client),
        ProcessKind::Server => {
            if is_primary_server(local) {
                Ok(Role::Record)
            } else {
                Ok(Role::Standby)
            }
        }
    }
}

pub fn role_summary(role: Role) -> &'static str {
    match role {
        Role::Client => "This client holds this device's address and routes the others.",
        Role::Record => "This server holds the address list.",
        Role::Standby => "This server is standing by and will not open a store.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u8, grade: Grade, rank: u8) -> DeviceRecord {
        DeviceRecord {
            id: [id; 16],
            alias: format!("n{id}"),
            grade,
            sg_rank: rank,
            apps: Vec::new(),
        }
    }

    fn dir(local: DeviceRecord, others: Vec<DeviceRecord>) -> Directory {
        let local_device = local.id;
        let mut own_devices = vec![local];
        own_devices.extend(others);
        Directory {
            local_app_id: [9; 16],
            local_app_alias: SERVER_ALIAS.to_string(),
            local_app_approved: false,
            token: [2; 16],
            local_device,
            owner_alias: "owner".to_string(),
            owner_id: [4; 16],
            own_devices,
            contacts: Vec::new(),
        }
    }

    #[test]
    fn rank_one_server_holds_the_list() {
        let directory = dir(
            device(1, Grade::Server, 1),
            vec![device(2, Grade::Device, 0)],
        );
        let role = choose_role(&directory, ProcessKind::Server).unwrap();
        assert_eq!(role, Role::Record);
        assert!(role.opens_store());
    }

    #[test]
    fn other_nodes_stand_by() {
        for (grade, rank) in [
            (Grade::Server, 2u8),
            (Grade::Server, 0),
            (Grade::Device, 0),
            (Grade::Device, 1),
        ] {
            let directory = dir(device(3, grade, rank), vec![device(1, Grade::Server, 1)]);
            let role = choose_role(&directory, ProcessKind::Server).unwrap();
            assert_eq!(role, Role::Standby);
            assert!(!role.opens_store());
        }
    }

    #[test]
    fn a_client_stays_a_client_on_any_grade() {
        let directory = dir(device(1, Grade::Server, 1), Vec::new());
        assert_eq!(
            choose_role(&directory, ProcessKind::Client).unwrap(),
            Role::Client
        );
    }

    #[test]
    fn a_missing_local_device_is_an_error() {
        let mut directory = dir(device(1, Grade::Server, 1), Vec::new());
        directory.local_device = [8; 16];
        assert_eq!(
            choose_role(&directory, ProcessKind::Server),
            Err(RoleError::LocalDeviceMissing)
        );
    }

    fn push_str(buf: &mut Vec<u8>, text: &str) {
        buf.push(text.len() as u8);
        buf.extend_from_slice(text.as_bytes());
    }

    fn push_device(buf: &mut Vec<u8>, id: [u8; 16], grade: u8, rank: u8) {
        buf.extend_from_slice(&id);
        push_str(buf, "n64");
        buf.push(grade);
        buf.push(rank);
        buf.push(0);
        buf.extend_from_slice(&[0xA5; 32]);
        buf.extend_from_slice(&[0x5A; 32]);
        buf.extend_from_slice(&[0x11; 64]);
        buf.extend_from_slice(&42u64.to_le_bytes());
        push_str(buf, "cert");
    }

    #[test]
    fn get_data_names_the_local_rank() {
        let mut buf = vec![0x00];
        buf.extend_from_slice(&[7; 16]);
        push_str(&mut buf, SERVER_ALIAS);
        buf.extend_from_slice(&[127, 0, 0, 1]);
        buf.extend_from_slice(&8793u16.to_be_bytes());
        buf.push(1);
        buf.extend_from_slice(&[2; 16]);
        let local = [1; 16];
        buf.extend_from_slice(&local);
        push_str(&mut buf, "owner");
        buf.extend_from_slice(&[4; 16]);
        buf.push(1);
        push_device(&mut buf, local, 1, 1);
        buf.push(0);
        buf.push(0);
        let parsed = parse_get_data(&buf).unwrap();
        assert_eq!(parsed.local_device, local);
        assert!(is_primary_server(&parsed.own_devices[0]));
        assert_eq!(
            choose_role(&parsed, ProcessKind::Server).unwrap(),
            Role::Record
        );
    }
}
