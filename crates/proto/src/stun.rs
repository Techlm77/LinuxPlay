//! The one STUN exchange LinuxPlay uses (RFC 8489): a device asks "what
//! address do my packets come from?" and the server answers. A device behind
//! a router learns its public address this way, which the other side needs
//! to reach it directly.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
const HEADER_LEN: usize = 20;
const FAMILY_V4: u8 = 1;
const FAMILY_V6: u8 = 2;

/// Identifies one request and its answer.
pub type TransactionId = [u8; 12];

/// A binding request.
pub fn binding_request(id: &TransactionId) -> Vec<u8> {
    let mut msg = Vec::with_capacity(HEADER_LEN);
    msg.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE);
    msg.extend_from_slice(id);
    msg
}

/// The transaction id of `packet` if it is a binding request.
pub fn parse_binding_request(packet: &[u8]) -> Option<TransactionId> {
    let (kind, id, _) = parse(packet)?;
    (kind == BINDING_REQUEST).then_some(id)
}

/// The answer to a binding request that arrived from `from`.
pub fn binding_response(id: &TransactionId, from: SocketAddr) -> Vec<u8> {
    let key = xor_key(id);
    let port = from.port() ^ u16::from_be_bytes([MAGIC_COOKIE[0], MAGIC_COOKIE[1]]);
    let (family, address): (u8, Vec<u8>) = match from.ip().to_canonical() {
        IpAddr::V4(ip) => (FAMILY_V4, ip.octets().iter().zip(key).map(|(a, k)| a ^ k).collect()),
        IpAddr::V6(ip) => (FAMILY_V6, ip.octets().iter().zip(key).map(|(a, k)| a ^ k).collect()),
    };
    let mut attribute = vec![0, family];
    attribute.extend_from_slice(&port.to_be_bytes());
    attribute.extend_from_slice(&address);

    let mut msg = Vec::with_capacity(HEADER_LEN + 4 + attribute.len());
    msg.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    msg.extend_from_slice(&((4 + attribute.len()) as u16).to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE);
    msg.extend_from_slice(id);
    msg.extend_from_slice(&XOR_MAPPED_ADDRESS.to_be_bytes());
    msg.extend_from_slice(&(attribute.len() as u16).to_be_bytes());
    msg.extend_from_slice(&attribute);
    msg
}

/// The address a binding response reports, if `packet` answers request `id`.
pub fn parse_binding_response(packet: &[u8], id: &TransactionId) -> Option<SocketAddr> {
    let (kind, got, mut attributes) = parse(packet)?;
    if kind != BINDING_SUCCESS || got != *id {
        return None;
    }
    let key = xor_key(id);
    while attributes.len() >= 4 {
        let kind = u16::from_be_bytes([attributes[0], attributes[1]]);
        let len = u16::from_be_bytes([attributes[2], attributes[3]]) as usize;
        let value = attributes.get(4..4 + len)?;
        if kind == XOR_MAPPED_ADDRESS {
            let port = u16::from_be_bytes([*value.get(2)?, *value.get(3)?]) ^ u16::from_be_bytes([MAGIC_COOKIE[0], MAGIC_COOKIE[1]]);
            let unmask = |n: usize| -> Option<Vec<u8>> { Some(value.get(4..4 + n)?.iter().zip(key).map(|(a, k)| a ^ k).collect()) };
            let ip = match *value.get(1)? {
                FAMILY_V4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(unmask(4)?).ok()?)),
                FAMILY_V6 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(unmask(16)?).ok()?)),
                _ => return None,
            };
            return Some(SocketAddr::new(ip, port));
        }
        // Attributes are padded to a multiple of four bytes.
        attributes = attributes.get(4 + len.div_ceil(4) * 4..)?;
    }
    None
}

/// The bytes addresses are masked with: the magic cookie, then the id.
fn xor_key(id: &TransactionId) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..4].copy_from_slice(&MAGIC_COOKIE);
    key[4..].copy_from_slice(id);
    key
}

/// Split a STUN message into its type, transaction id and attributes.
fn parse(packet: &[u8]) -> Option<(u16, TransactionId, &[u8])> {
    let header = packet.get(..HEADER_LEN)?;
    // The two top bits of a STUN message are zero, and the cookie is fixed.
    if header[0] & 0xc0 != 0 || header[4..8] != MAGIC_COOKIE {
        return None;
    }
    let len = u16::from_be_bytes([header[2], header[3]]) as usize;
    let attributes = packet.get(HEADER_LEN..HEADER_LEN + len)?;
    Some((u16::from_be_bytes([header[0], header[1]]), header[8..20].try_into().ok()?, attributes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: TransactionId = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    #[test]
    fn a_request_and_its_answer_round_trip() {
        let request = binding_request(&ID);
        assert_eq!(parse_binding_request(&request), Some(ID));
        for from in ["203.0.113.7:54321", "[2001:db8::1234]:443", "192.168.1.30:47801"] {
            let from: SocketAddr = from.parse().unwrap();
            let response = binding_response(&ID, from);
            assert_eq!(parse_binding_response(&response, &ID), Some(from));
            // An answer to someone else's request is not ours.
            assert_eq!(parse_binding_response(&response, &[9; 12]), None);
            // And a response is not a request.
            assert_eq!(parse_binding_request(&response), None);
        }
    }

    #[test]
    fn the_wire_format_matches_the_rfc() {
        // RFC 5769's IPv4 sample: 192.0.2.1:32853 encodes as port a147, address e112a643.
        let id = [0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae];
        let response = binding_response(&id, "192.0.2.1:32853".parse().unwrap());
        assert_eq!(&response[..8], &[0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42]);
        assert_eq!(&response[20..], &[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43]);
    }

    #[test]
    fn junk_is_not_stun() {
        assert_eq!(parse_binding_request(&[]), None);
        assert_eq!(parse_binding_request(&[0u8; 19]), None);
        assert_eq!(parse_binding_request(&[0xff; 64]), None);
        // A length that runs past the packet.
        let mut truncated = binding_response(&ID, "10.0.0.1:1".parse().unwrap());
        truncated.truncate(24);
        assert_eq!(parse_binding_response(&truncated, &ID), None);
        // Every prefix of a real response must parse or fail, never panic.
        let full = binding_response(&ID, "[2001:db8::1]:9".parse().unwrap());
        for cut in 0..full.len() {
            let _ = parse_binding_response(&full[..cut], &ID);
        }
    }
}
