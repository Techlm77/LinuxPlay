//! PIN pairing: SPAKE2 over the PIN, then HMAC key confirmation that binds
//! both TLS certificate fingerprints. See `linuxplay_proto::signal`.

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use linuxplay_proto::signal::pairing_mac_input;
use rand::Rng;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::tls::Fingerprint;

type HmacSha256 = Hmac<Sha256>;

const ID_CLIENT: &[u8] = b"linuxplay-client";
const ID_HOST: &[u8] = b"linuxplay-host";

/// A fresh six-digit PIN.
pub fn new_pin() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000u32))
}

fn mac(key: &[u8], role: &str, host_fp: &Fingerprint, client_fp: &Fingerprint) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC takes any key length");
    m.update(&pairing_mac_input(role, host_fp, client_fp));
    m
}

pub struct ClientPairing(Spake2<Ed25519Group>);

/// Client: start pairing with the PIN the user typed. Returns the message
/// for `PairStart`.
pub fn client_start(pin: &str) -> (ClientPairing, String) {
    let (state, msg) = Spake2::<Ed25519Group>::start_a(
        &Password::new(pin.trim().as_bytes()),
        &Identity::new(ID_CLIENT),
        &Identity::new(ID_HOST),
    );
    (ClientPairing(state), B64.encode(msg))
}

impl ClientPairing {
    /// Check the host's `PairReply` and produce the MAC for `PairFinish`.
    /// Fails if the PIN was wrong or someone sits in the middle.
    pub fn finish(
        self,
        host_spake: &str,
        host_mac: &str,
        host_fp: &Fingerprint,
        client_fp: &Fingerprint,
    ) -> Result<String> {
        let msg = B64.decode(host_spake)?;
        let key = self.0.finish(&msg).map_err(|e| anyhow!("pairing failed: {e:?}"))?;
        let tag = hex::decode(host_mac)?;
        if mac(&key, "host", host_fp, client_fp).verify_slice(&tag).is_err() {
            bail!("wrong PIN, or the connection is being intercepted");
        }
        Ok(hex::encode(mac(&key, "client", host_fp, client_fp).finalize().into_bytes()))
    }
}

pub struct HostPairing {
    key: Vec<u8>,
    host_fp: Fingerprint,
    client_fp: Fingerprint,
}

/// Host: answer `PairStart`. Returns the state plus the SPAKE2 message and
/// MAC for `PairReply`.
pub fn host_respond(
    pin: &str,
    client_spake: &str,
    host_fp: &Fingerprint,
    client_fp: &Fingerprint,
) -> Result<(HostPairing, String, String)> {
    let (state, msg) = Spake2::<Ed25519Group>::start_b(
        &Password::new(pin.trim().as_bytes()),
        &Identity::new(ID_CLIENT),
        &Identity::new(ID_HOST),
    );
    let inbound = B64.decode(client_spake)?;
    let key = state.finish(&inbound).map_err(|e| anyhow!("pairing failed: {e:?}"))?;
    let host_mac = hex::encode(mac(&key, "host", host_fp, client_fp).finalize().into_bytes());
    let pairing = HostPairing { key, host_fp: *host_fp, client_fp: *client_fp };
    Ok((pairing, B64.encode(msg), host_mac))
}

impl HostPairing {
    /// Check the client's `PairFinish` MAC.
    pub fn verify(&self, client_mac: &str) -> Result<()> {
        let tag = hex::decode(client_mac)?;
        mac(&self.key, "client", &self.host_fp, &self.client_fp)
            .verify_slice(&tag)
            .map_err(|_| anyhow!("wrong PIN"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: Fingerprint = [1; 32];
    const CLIENT: Fingerprint = [2; 32];

    fn run(client_pin: &str, host_pin: &str, host_fp_seen_by_client: Fingerprint) -> Result<()> {
        let (client, start) = client_start(client_pin);
        let (host, spake, host_mac) = host_respond(host_pin, &start, &HOST, &CLIENT)?;
        let client_mac = client.finish(&spake, &host_mac, &host_fp_seen_by_client, &CLIENT)?;
        host.verify(&client_mac)
    }

    #[test]
    fn same_pin_pairs() {
        run("123456", "123456", HOST).unwrap();
    }

    #[test]
    fn wrong_pin_fails() {
        assert!(run("123456", "654321", HOST).is_err());
    }

    #[test]
    fn swapped_certificate_fails() {
        // A man in the middle shows the client a different host certificate.
        assert!(run("123456", "123456", [9; 32]).is_err());
    }
}
