//! Forward TCP and UDP through pNet, and rewrite the addresses.
//!
//! The sender removes the source and destination before the packet is sent to
//! the device that destination names. The receiver writes the source from the
//! address it assigned to the sender, and the destination from the address it
//! assigned to itself. If this list has no address for the other device, the
//! packet is dropped. The two lists do not have to use the same numbers.

use std::net::IpAddr;

use crate::directory::{Directory, CLIENT_ALIAS};
use crate::store::Assignment;

pub const PACKET: u8 = 0x10;
const MAX_PAYLOAD: usize = 4096;

pub fn wrap_packet(packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() + 2 > MAX_PAYLOAD {
        return None;
    }
    let mut out = Vec::with_capacity(packet.len() + 2);
    out.push(1);
    out.push(PACKET);
    out.extend_from_slice(packet);
    Some(out)
}

pub fn unwrap_packet(payload: &[u8]) -> Option<&[u8]> {
    if payload.len() < 2 || payload[0] != 1 || payload[1] != PACKET {
        return None;
    }
    Some(&payload[2..])
}

/// The device to send to, and the packet with its addresses cleared.
pub fn prepare_outbound(
    packet: &[u8],
    local_device: &[u8; 16],
    rows: &[Assignment],
) -> Option<([u8; 16], Vec<u8>)> {
    let view = view(packet)?;
    let dest = view.dest?;
    let device = rows
        .iter()
        .find(|row| row.address == Some(dest) && row.device != *local_device)
        .map(|row| row.device)?;
    let mut cleared = view.bytes.to_vec();
    clear_addresses(&mut cleared)?;
    let payload = wrap_packet(&cleared)?;
    Some((device, payload))
}

/// A packet for the local interface, using this user's addresses.
pub fn prepare_inbound(
    payload: &[u8],
    local_device: &[u8; 16],
    sender_device: &[u8; 16],
    rows: &[Assignment],
) -> Option<Vec<u8>> {
    if sender_device == local_device {
        return None;
    }
    let packet = unwrap_packet(payload)?;
    let view = view(packet)?;
    let source = address_of(rows, sender_device)?;
    let dest = address_of(rows, local_device)?;
    if !same_family(view.version, source) || !same_family(view.version, dest) {
        return None;
    }
    let mut rewritten = view.bytes.to_vec();
    write_addresses(&mut rewritten, source, dest)?;
    Some(rewritten)
}

pub fn client_app(dir: &Directory, device: &[u8; 16]) -> Option<[u8; 16]> {
    device_records(dir).find_map(|record| {
        if record.id != *device {
            return None;
        }
        record
            .apps
            .iter()
            .find(|app| app.alias == CLIENT_ALIAS)
            .map(|app| app.id)
    })
}

pub fn device_of_client(dir: &Directory, app: &[u8; 16]) -> Option<[u8; 16]> {
    device_records(dir).find_map(|record| {
        record
            .apps
            .iter()
            .any(|candidate| candidate.id == *app && candidate.alias == CLIENT_ALIAS)
            .then_some(record.id)
    })
}

fn device_records(dir: &Directory) -> impl Iterator<Item = &crate::directory::DeviceRecord> {
    dir.own_devices.iter().chain(
        dir.contacts
            .iter()
            .flat_map(|contact| contact.devices.iter()),
    )
}

fn address_of(rows: &[Assignment], device: &[u8; 16]) -> Option<IpAddr> {
    rows.iter()
        .find(|row| row.device == *device)
        .and_then(|row| row.address)
}

struct View<'a> {
    bytes: &'a [u8],
    version: u8,
    dest: Option<IpAddr>,
}

fn view(packet: &[u8]) -> Option<View<'_>> {
    if packet.is_empty() {
        return None;
    }
    match packet[0] >> 4 {
        4 => view_v4(packet),
        6 => view_v6(packet),
        _ => None,
    }
}

fn view_v4(packet: &[u8]) -> Option<View<'_>> {
    if packet.len() < 20 {
        return None;
    }
    let ihl = (packet[0] & 0x0f) as usize * 4;
    if ihl < 20 || packet.len() < ihl {
        return None;
    }
    let total = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if total < ihl || packet.len() < total {
        return None;
    }
    let frag = u16::from_be_bytes([packet[6], packet[7]]);
    if frag & 0x3fff != 0 {
        return None;
    }
    let protocol = packet[9];
    if !is_tcp_udp(protocol) {
        return None;
    }
    let header_end = ihl + transport_header_len(protocol, &packet[ihl..total])?;
    if total < header_end {
        return None;
    }
    let dest = IpAddr::from(<[u8; 4]>::try_from(&packet[16..20]).ok()?);
    Some(View {
        bytes: &packet[..total],
        version: 4,
        dest: Some(dest),
    })
}

fn view_v6(packet: &[u8]) -> Option<View<'_>> {
    if packet.len() < 40 {
        return None;
    }
    let payload_len = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    let total = 40 + payload_len;
    if packet.len() < total {
        return None;
    }
    let protocol = packet[6];
    if !is_tcp_udp(protocol) {
        return None;
    }
    let header_end = 40 + transport_header_len(protocol, &packet[40..total])?;
    if total < header_end {
        return None;
    }
    let dest = IpAddr::from(<[u8; 16]>::try_from(&packet[24..40]).ok()?);
    Some(View {
        bytes: &packet[..total],
        version: 6,
        dest: Some(dest),
    })
}

fn is_tcp_udp(protocol: u8) -> bool {
    protocol == 6 || protocol == 17
}

fn transport_header_len(protocol: u8, segment: &[u8]) -> Option<usize> {
    match protocol {
        6 => {
            if segment.len() < 20 {
                return None;
            }
            let len = (segment[12] >> 4) as usize * 4;
            if len < 20 || segment.len() < len {
                return None;
            }
            Some(len)
        }
        17 => {
            if segment.len() < 8 {
                return None;
            }
            Some(8)
        }
        _ => None,
    }
}

fn same_family(version: u8, address: IpAddr) -> bool {
    matches!((version, address), (4, IpAddr::V4(_)) | (6, IpAddr::V6(_)))
}

fn clear_addresses(packet: &mut [u8]) -> Option<()> {
    match packet.first()? >> 4 {
        4 => {
            packet[12..20].fill(0);
            fix_checksums(packet)
        }
        6 => {
            packet[8..40].fill(0);
            fix_checksums(packet)
        }
        _ => None,
    }
}

fn write_addresses(packet: &mut [u8], source: IpAddr, dest: IpAddr) -> Option<()> {
    match (packet.first()? >> 4, source, dest) {
        (4, IpAddr::V4(source), IpAddr::V4(dest)) => {
            packet[12..16].copy_from_slice(&source.octets());
            packet[16..20].copy_from_slice(&dest.octets());
            fix_checksums(packet)
        }
        (6, IpAddr::V6(source), IpAddr::V6(dest)) => {
            packet[8..24].copy_from_slice(&source.octets());
            packet[24..40].copy_from_slice(&dest.octets());
            fix_checksums(packet)
        }
        _ => None,
    }
}

fn fix_checksums(packet: &mut [u8]) -> Option<()> {
    let version = packet.first()? >> 4;
    let (protocol, transport_at) = match version {
        4 => {
            let ihl = (packet[0] & 0x0f) as usize * 4;
            write_checksum(&mut packet[..ihl], 10);
            (packet[9], ihl)
        }
        6 => (packet[6], 40),
        _ => return None,
    };
    let checksum_at = match protocol {
        6 => transport_at + 16,
        17 => transport_at + 6,
        _ => return None,
    };
    if packet.len() < checksum_at + 2 {
        return None;
    }
    packet[checksum_at] = 0;
    packet[checksum_at + 1] = 0;
    let sum = transport_checksum(packet, version, protocol, transport_at);
    let stored = if protocol == 17 && sum == 0 {
        0xffff
    } else {
        sum
    };
    packet[checksum_at..checksum_at + 2].copy_from_slice(&stored.to_be_bytes());
    Some(())
}

fn write_checksum(header: &mut [u8], at: usize) {
    header[at] = 0;
    header[at + 1] = 0;
    let sum = internet_checksum(header);
    header[at..at + 2].copy_from_slice(&sum.to_be_bytes());
}

fn transport_checksum(packet: &[u8], version: u8, protocol: u8, transport_at: usize) -> u16 {
    let segment = &packet[transport_at..];
    let mut pseudo = Vec::new();
    match version {
        4 => {
            pseudo.extend_from_slice(&packet[12..20]);
            pseudo.push(0);
            pseudo.push(protocol);
            pseudo.extend_from_slice(&(segment.len() as u16).to_be_bytes());
        }
        _ => {
            pseudo.extend_from_slice(&packet[8..40]);
            pseudo.extend_from_slice(&(segment.len() as u32).to_be_bytes());
            pseudo.extend_from_slice(&[0, 0, 0, protocol]);
        }
    }
    internet_checksum_parts(&[&pseudo, segment])
}

fn internet_checksum(data: &[u8]) -> u16 {
    internet_checksum_parts(&[data])
}

fn internet_checksum_parts(parts: &[&[u8]]) -> u16 {
    let mut sum = 0u32;
    let mut high = true;
    for part in parts {
        for byte in *part {
            if high {
                sum += u32::from(*byte) << 8;
            } else {
                sum += u32::from(*byte);
            }
            high = !high;
        }
    }
    if !high {
        sum += 0;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{AppRecord, ContactRecord, DeviceRecord, Grade};
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn row(id: u8, address: Option<IpAddr>) -> Assignment {
        Assignment {
            device: [id; 16],
            address,
        }
    }

    fn udp_v4(src: [u8; 4], dest: [u8; 4]) -> Vec<u8> {
        let mut packet = vec![
            0x45, 0, 0, 32, 0, 0, 0, 0, 64, 17, 0, 0, src[0], src[1], src[2], src[3], dest[0],
            dest[1], dest[2], dest[3], 0, 1, 0, 2, 0, 12, 0, 0, 1, 2, 3, 4,
        ];
        fix_checksums(&mut packet).unwrap();
        packet
    }

    #[test]
    fn an_outbound_packet_names_the_device_and_hides_the_addresses() {
        let packet = udp_v4([10, 0, 0, 2], [10, 0, 0, 4]);
        let rows = [
            row(2, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)))),
            row(4, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4)))),
        ];
        let (device, payload) = prepare_outbound(&packet, &[2; 16], &rows).unwrap();
        assert_eq!(device, [4; 16]);
        let cleared = unwrap_packet(&payload).unwrap();
        assert_eq!(&cleared[12..20], &[0; 8]);
        assert!(prepare_outbound(&packet, &[2; 16], &[row(4, None)]).is_none());
    }

    #[test]
    fn the_receiver_writes_its_own_addresses() {
        let packet = udp_v4([10, 0, 0, 2], [10, 0, 0, 4]);
        let sent = prepare_outbound(
            &packet,
            &[2; 16],
            &[
                row(2, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)))),
                row(4, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4)))),
            ],
        )
        .unwrap()
        .1;
        let local = [1u8; 16];
        let sender = [2u8; 16];
        let rows = [
            row(sender[0], Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 2)))),
            row(local[0], Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 9)))),
        ];
        let inbound = prepare_inbound(&sent, &local, &sender, &rows).unwrap();
        assert_eq!(&inbound[12..16], &[192, 168, 50, 2]);
        assert_eq!(&inbound[16..20], &[192, 168, 50, 9]);
        assert_eq!(internet_checksum(&inbound[..20]), 0);
        assert!(prepare_inbound(&sent, &local, &sender, &[rows[1].clone()]).is_none());
    }

    #[test]
    fn tcp_and_ipv6_follow_the_same_rewrite() {
        let mut v6 = vec![0u8; 60];
        v6[0] = 0x60;
        v6[5] = 20;
        v6[6] = 6;
        v6[7] = 64;
        v6[8] = 0xfd;
        v6[23] = 2;
        v6[24] = 0xfd;
        v6[39] = 4;
        v6[52] = 0x50;
        fix_checksums(&mut v6).unwrap();
        let src = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2));
        let dest = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 4));
        let (device, payload) =
            prepare_outbound(&v6, &[2; 16], &[row(2, Some(src)), row(4, Some(dest))]).unwrap();
        assert_eq!(device, [4; 16]);
        let other = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let mine = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 9));
        let inbound = prepare_inbound(
            &payload,
            &[1; 16],
            &[2; 16],
            &[row(2, Some(other)), row(1, Some(mine))],
        )
        .unwrap();
        assert_eq!(&inbound[8..24], &Ipv6Addr::LOCALHOST.octets());
        assert_eq!(inbound[6], 6);
    }

    #[test]
    fn other_protocols_are_dropped() {
        let mut packet = udp_v4([10, 0, 0, 2], [10, 0, 0, 4]);
        packet[9] = 1;
        let rows = [row(4, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4))))];
        assert!(prepare_outbound(&packet, &[2; 16], &rows).is_none());
    }

    #[test]
    fn the_client_app_is_found_on_own_and_contact_devices() {
        let directory = Directory {
            local_app_id: [9; 16],
            local_app_alias: "pnet-lan-server".into(),
            local_app_approved: true,
            token: [1; 16],
            local_device: [1; 16],
            owner_alias: "owner".into(),
            owner_id: [7; 16],
            own_devices: vec![DeviceRecord {
                id: [3; 16],
                alias: "laptop".into(),
                grade: Grade::Device,
                sg_rank: 0,
                apps: vec![AppRecord {
                    id: [8; 16],
                    alias: CLIENT_ALIAS.into(),
                    approved: true,
                }],
            }],
            contacts: vec![ContactRecord {
                alias: "chad".into(),
                id: [6; 16],
                devices: vec![DeviceRecord {
                    id: [4; 16],
                    alias: "phone".into(),
                    grade: Grade::Device,
                    sg_rank: 0,
                    apps: vec![AppRecord {
                        id: [5; 16],
                        alias: CLIENT_ALIAS.into(),
                        approved: true,
                    }],
                }],
            }],
        };
        assert_eq!(client_app(&directory, &[4; 16]), Some([5; 16]));
        assert_eq!(device_of_client(&directory, &[8; 16]), Some([3; 16]));
        assert_eq!(device_of_client(&directory, &[1; 16]), None);
    }
}
