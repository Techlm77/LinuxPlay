//! A STUN server for binding requests only: it tells a device which public
//! address its packets arrive from. Stateless, and an answer is barely
//! larger than the question, so it is of no use for amplifying traffic.

use linuxplay_proto::stun;
use tokio::net::UdpSocket;
use tracing::warn;

/// Answer binding requests on `socket` until the task is dropped.
pub async fn serve(socket: UdpSocket) {
    let mut packet = [0u8; 576];
    loop {
        match socket.recv_from(&mut packet).await {
            Ok((len, from)) => {
                if let Some(id) = stun::parse_binding_request(&packet[..len]) {
                    // A lost answer is asked for again; nothing to do on error.
                    let _ = socket.send_to(&stun::binding_response(&id, from), from).await;
                }
            }
            Err(e) => {
                warn!("STUN socket: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_binding_request_is_answered_with_the_senders_address() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        tokio::spawn(serve(server));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let id = [7u8; 12];
        client.send_to(b"not stun at all", server_addr).await.unwrap();
        client.send_to(&stun::binding_request(&id), server_addr).await.unwrap();
        let mut reply = [0u8; 128];
        let (len, _) = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_from(&mut reply))
            .await
            .expect("an answer")
            .unwrap();
        assert_eq!(stun::parse_binding_response(&reply[..len], &id), Some(client.local_addr().unwrap()));
    }
}
