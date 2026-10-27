//! The client's TUN interface.
//!
//! The interface claims only the address assigned to this device. Every other
//! assigned address is a host route through the interface. An empty assignment
//! leaves the interface with nothing to claim. Packets are not forwarded yet.

use std::fs::{File, OpenOptions};
use std::io;
use std::net::IpAddr;
use std::os::unix::io::AsRawFd;
use std::process::Command;

use crate::store::Assignment;

const TUNSETIFF: u64 = 0x4004_54ca;
const IFF_TUN: i16 = 0x0001;
const IFF_NO_PI: i16 = 0x1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub local: Option<IpAddr>,
    pub routes: Vec<IpAddr>,
}

/// This device's address, and host routes for every other assigned address.
pub fn claim_for(local_device: &[u8; 16], rows: &[Assignment]) -> Claim {
    let local = rows
        .iter()
        .find(|row| row.device == *local_device)
        .and_then(|row| row.address);
    let routes = rows
        .iter()
        .filter(|row| row.device != *local_device)
        .filter_map(|row| row.address)
        .filter(|address| Some(*address) != local)
        .collect();
    Claim { local, routes }
}

/// Commands that replace the interface address and routes with this claim.
pub fn commands(name: &str, claim: &Claim) -> Vec<Vec<String>> {
    let mut commands = vec![
        vec![
            "link".into(),
            "set".into(),
            "dev".into(),
            name.into(),
            "up".into(),
        ],
        vec!["addr".into(), "flush".into(), "dev".into(), name.into()],
        vec!["route".into(), "flush".into(), "dev".into(), name.into()],
        vec![
            "-6".into(),
            "route".into(),
            "flush".into(),
            "dev".into(),
            name.into(),
        ],
    ];
    if let Some(address) = claim.local {
        commands.push(addr_add(name, address));
    }
    for address in &claim.routes {
        commands.push(route_add(name, *address));
    }
    commands
}

fn addr_add(name: &str, address: IpAddr) -> Vec<String> {
    let mut command = ip_family(address);
    command.extend([
        "addr".into(),
        "add".into(),
        prefixed(address),
        "dev".into(),
        name.into(),
    ]);
    command
}

fn route_add(name: &str, address: IpAddr) -> Vec<String> {
    let mut command = ip_family(address);
    command.extend([
        "route".into(),
        "add".into(),
        prefixed(address),
        "dev".into(),
        name.into(),
    ]);
    command
}

fn ip_family(address: IpAddr) -> Vec<String> {
    match address {
        IpAddr::V4(_) => Vec::new(),
        IpAddr::V6(_) => vec!["-6".into()],
    }
}

fn prefixed(address: IpAddr) -> String {
    match address {
        IpAddr::V4(addr) => format!("{addr}/32"),
        IpAddr::V6(addr) => format!("{addr}/128"),
    }
}

pub struct Tun {
    file: File,
    pub name: String,
}

impl Tun {
    pub fn open(name: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")?;
        let mut req = [0u8; 32];
        let bytes = name.as_bytes();
        let len = bytes.len().min(15);
        req[..len].copy_from_slice(&bytes[..len]);
        let flags = (IFF_TUN | IFF_NO_PI).to_ne_bytes();
        req[16] = flags[0];
        req[17] = flags[1];
        let rc = unsafe { ioctl(file.as_raw_fd(), TUNSETIFF, req.as_mut_ptr()) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let end = req.iter().position(|byte| *byte == 0).unwrap_or(15);
        let name = String::from_utf8_lossy(&req[..end]).into_owned();
        Ok(Self { file, name })
    }

    pub fn fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}

/// Apply the claim with `ip`. A failed command returns its status text.
pub fn apply(name: &str, claim: &Claim) -> Result<(), String> {
    for command in commands(name, claim) {
        let output = Command::new("ip")
            .args(&command)
            .output()
            .map_err(|err| format!("ip {}: {err}", command.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let text = stderr.trim();
            if text.is_empty() {
                return Err(format!("ip {} failed", command.join(" ")));
            }
            // Flushing a device with no routes is not a failure.
            if command.iter().any(|word| word == "flush")
                && (text.is_empty()
                    || text.contains("Nothing to flush")
                    || text.contains("No such process"))
            {
                continue;
            }
            return Err(format!("ip {}: {text}", command.join(" ")));
        }
    }
    Ok(())
}

pub fn describe(claim: &Claim) -> String {
    match claim.local {
        Some(address) => format!(
            "Claimed {address}. {} other address{}.",
            claim.routes.len(),
            if claim.routes.len() == 1 { "" } else { "es" }
        ),
        None => "No address is assigned to this device.".to_string(),
    }
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, arg: *mut u8) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn row(id: u8, address: Option<IpAddr>) -> Assignment {
        Assignment {
            device: [id; 16],
            address,
        }
    }

    #[test]
    fn this_device_is_claimed_and_the_others_are_routes() {
        let local = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let other = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4));
        let v6 = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 8));
        let rows = [
            row(1, Some(local)),
            row(2, Some(other)),
            row(3, None),
            row(4, Some(v6)),
        ];
        let claim = claim_for(&[1; 16], &rows);
        assert_eq!(claim.local, Some(local));
        assert_eq!(claim.routes, vec![other, v6]);
        let lines: Vec<String> = commands("pnetlan", &claim)
            .into_iter()
            .map(|command| command.join(" "))
            .collect();
        assert!(lines
            .iter()
            .any(|line| line == "addr add 10.0.0.2/32 dev pnetlan"));
        assert!(lines
            .iter()
            .any(|line| line == "route add 10.0.0.4/32 dev pnetlan"));
        assert!(lines
            .iter()
            .any(|line| line == "-6 route add fd00::8/128 dev pnetlan"));
        assert!(!lines
            .iter()
            .any(|line| line.contains("route") && line.contains("10.0.0.2")));
    }

    #[test]
    fn an_empty_assignment_claims_nothing() {
        let rows = [row(1, None), row(2, None)];
        let claim = claim_for(&[1; 16], &rows);
        assert_eq!(
            claim,
            Claim {
                local: None,
                routes: Vec::new()
            }
        );
        let commands = commands("pnetlan", &claim);
        assert!(commands
            .iter()
            .all(|command| !command.iter().any(|word| word == "add")));
        assert_eq!(describe(&claim), "No address is assigned to this device.");
    }

    #[test]
    fn routes_remain_when_this_device_has_no_address() {
        let other = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let claim = claim_for(&[1; 16], &[row(1, None), row(2, Some(other))]);
        assert_eq!(claim.local, None);
        assert_eq!(claim.routes, vec![other]);
    }
}
