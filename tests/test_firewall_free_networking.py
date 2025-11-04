"""Tests for firewall-free networking with ephemeral ports.

This module tests the new outbound-only connection design where:
- Client uses ephemeral ports (no bind() needed)
- Host learns client's actual port from incoming messages
- No client firewall configuration required

Test Coverage:
--------------
- Ephemeral port communication (3 tests)
- Host address tracking (2 tests)
- Heartbeat with ephemeral ports (2 tests)
- Clipboard with ephemeral ports (2 tests)
- NAT traversal simulation (2 tests)
- Address tracking updates (2 tests)

Total: 13 tests covering the firewall-free networking design

Key Design Principles Tested:
-----------------------------
1. Client never binds to specific ports (uses OS-assigned ephemeral ports)
2. Host learns client's port from incoming messages
3. Bidirectional communication works on same ephemeral port
4. NAT-friendly: outbound connections work through firewalls/NAT
5. Host can update tracked address when client reconnects
"""

import socket
import threading

import pytest


class TestEphemeralPortCommunication:
    """Test that client can communicate without binding to specific ports."""

    def test_client_can_send_without_bind(self):
        """Verify client socket can send without binding to a specific port."""
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            # Client should be able to sendto without bind()
            # Use a non-routable address to avoid actual network traffic
            sock.sendto(b"TEST", ("192.0.2.1", 7004))

            # Verify socket got an ephemeral port assigned
            local_addr = sock.getsockname()
            assert local_addr[0] != ""
            # Ephemeral ports are typically > 1024
            assert local_addr[1] > 1024
        finally:
            sock.close()

    def test_ephemeral_port_is_unique_per_socket(self):
        """Verify each socket gets a unique ephemeral port."""
        sock1 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

        try:
            # Send from both to get ephemeral ports assigned
            sock1.sendto(b"TEST1", ("192.0.2.1", 7004))
            sock2.sendto(b"TEST2", ("192.0.2.1", 7004))

            port1 = sock1.getsockname()[1]
            port2 = sock2.getsockname()[1]

            # Ports should be different
            assert port1 != port2
            assert port1 > 1024
            assert port2 > 1024
        finally:
            sock1.close()
            sock2.close()

    def test_client_can_receive_on_ephemeral_port(self):
        """Verify client can receive responses on ephemeral port."""
        # Create a simple server
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))  # Bind to any available port
        server_addr = server_sock.getsockname()

        # Create client with ephemeral port
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        received_response = []

        def server_responder():
            """Server that responds to client's source address."""
            _data, addr = server_sock.recvfrom(1024)
            # Respond to the client's ephemeral port
            server_sock.sendto(b"RESPONSE", addr)

        try:
            # Start server thread
            server_thread = threading.Thread(target=server_responder, daemon=True)
            server_thread.start()

            # Client sends to server
            client_sock.sendto(b"REQUEST", server_addr)

            # Client receives on same socket (ephemeral port)
            data, addr = client_sock.recvfrom(1024)
            received_response.append(data)

            assert received_response[0] == b"RESPONSE"
            assert addr == server_addr

            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()


class TestHostAddressTracking:
    """Test that host correctly tracks client's ephemeral port."""

    def test_host_learns_client_port_from_message(self):
        """Verify host can extract and remember client's source address."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()

        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

        learned_client_addr = [None]

        def server_listener():
            """Server learns client address from incoming message."""
            _data, addr = server_sock.recvfrom(1024)
            learned_client_addr[0] = addr

        try:
            server_thread = threading.Thread(target=server_listener, daemon=True)
            server_thread.start()

            # Client sends from ephemeral port
            client_sock.sendto(b"HELLO", server_addr)

            server_thread.join(timeout=1)

            # Server should have learned client's ephemeral port
            assert learned_client_addr[0] is not None
            assert learned_client_addr[0][0] == "127.0.0.1"
            assert learned_client_addr[0][1] > 1024  # Ephemeral port range
        finally:
            client_sock.close()
            server_sock.close()

    def test_host_can_reply_to_learned_address(self):
        """Verify host can send replies to client's ephemeral port."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()

        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        def echo_server():
            """Server that echoes back to learned client address."""
            data, client_addr = server_sock.recvfrom(1024)
            # Send reply to learned address
            server_sock.sendto(b"ECHO: " + data, client_addr)

        try:
            server_thread = threading.Thread(target=echo_server, daemon=True)
            server_thread.start()

            # Client sends message
            client_sock.sendto(b"TEST", server_addr)

            # Client receives reply on ephemeral port
            data, addr = client_sock.recvfrom(1024)

            assert data == b"ECHO: TEST"
            assert addr == server_addr

            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()


class TestHeartbeatWithEphemeralPorts:
    """Test heartbeat protocol using ephemeral ports."""

    def test_heartbeat_pong_establishes_connection(self):
        """Verify client's initial PONG establishes the connection path."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)

        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        learned_addr = [None]

        def server_handler():
            """Server learns client address from initial PONG."""
            data, addr = server_sock.recvfrom(256)
            if data == b"PONG":
                learned_addr[0] = addr
                # Send PING to learned address
                server_sock.sendto(b"PING", addr)

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()

            # Client sends initial PONG to establish connection
            client_sock.sendto(b"PONG", server_addr)

            # Client receives PING on ephemeral port
            data, _addr = client_sock.recvfrom(256)

            assert data == b"PING"
            assert learned_addr[0] is not None
            assert learned_addr[0][1] > 1024  # Ephemeral port

            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

    def test_heartbeat_bidirectional_on_ephemeral_port(self):
        """Verify full PING/PONG cycle works with ephemeral ports."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)

        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        messages = []

        def server_handler():
            """Server handles PONG and sends PING."""
            # Receive initial PONG
            data, client_addr = server_sock.recvfrom(256)
            messages.append(("server_recv", data))

            # Send PING to learned address
            server_sock.sendto(b"PING", client_addr)

            # Receive PONG reply
            data, _ = server_sock.recvfrom(256)
            messages.append(("server_recv", data))

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()

            # Client: Initial PONG
            client_sock.sendto(b"PONG", server_addr)
            messages.append(("client_send", b"PONG"))

            # Client: Receive PING
            data, _ = client_sock.recvfrom(256)
            messages.append(("client_recv", data))

            # Client: Reply with PONG
            client_sock.sendto(b"PONG", server_addr)
            messages.append(("client_send", b"PONG"))

            server_thread.join(timeout=1)

            # Verify message sequence
            assert ("client_send", b"PONG") in messages
            assert ("server_recv", b"PONG") in messages
            assert ("client_recv", b"PING") in messages

        finally:
            client_sock.close()
            server_sock.close()


class TestClipboardWithEphemeralPorts:
    """Test clipboard sync using ephemeral ports."""

    def test_clipboard_keepalive_registers_client(self):
        """Verify client keepalive message registers ephemeral port."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)

        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        registered_addr = [None]

        def server_handler():
            """Server registers client from keepalive."""
            data, addr = server_sock.recvfrom(1024)
            if data == b"CLIPBOARD_KEEPALIVE":
                registered_addr[0] = addr
                # Send clipboard update to registered address
                server_sock.sendto(b"CLIPBOARD_UPDATE HOST test", addr)

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()

            # Client sends keepalive
            client_sock.sendto(b"CLIPBOARD_KEEPALIVE", server_addr)

            # Client receives update on ephemeral port
            data, _ = client_sock.recvfrom(1024)

            assert data == b"CLIPBOARD_UPDATE HOST test"
            assert registered_addr[0] is not None
            assert registered_addr[0][1] > 1024  # Ephemeral port

            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

    def test_clipboard_update_bidirectional(self):
        """Verify clipboard updates work in both directions with ephemeral ports."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)

        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        messages = []

        def server_handler():
            """Server handles clipboard messages."""
            # Receive client update and learn address
            data, client_addr = server_sock.recvfrom(1024)
            messages.append(("server", data))

            # Send update to learned address
            server_sock.sendto(b"CLIPBOARD_UPDATE HOST from_host", client_addr)

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()

            # Client sends update from ephemeral port
            client_sock.sendto(b"CLIPBOARD_UPDATE CLIENT from_client", server_addr)

            # Client receives update on same ephemeral port
            data, _ = client_sock.recvfrom(1024)
            messages.append(("client", data))

            server_thread.join(timeout=1)

            assert ("server", b"CLIPBOARD_UPDATE CLIENT from_client") in messages
            assert ("client", b"CLIPBOARD_UPDATE HOST from_host") in messages

        finally:
            client_sock.close()
            server_sock.close()


class TestNATTraversal:
    """Test that the design works through NAT (simulated)."""

    def test_single_socket_bidirectional_communication(self):
        """Verify same socket can both send and receive (NAT-friendly)."""
        # This simulates how NAT works: same socket for both directions
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("127.0.0.1", 0))
        sock.settimeout(2)
        local_addr = sock.getsockname()

        # Create echo server
        echo_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        echo_sock.bind(("127.0.0.1", 0))
        echo_addr = echo_sock.getsockname()

        def echo_server():
            data, addr = echo_sock.recvfrom(1024)
            echo_sock.sendto(b"ECHO:" + data, addr)

        try:
            echo_thread = threading.Thread(target=echo_server, daemon=True)
            echo_thread.start()

            # Send from socket
            sock.sendto(b"TEST", echo_addr)

            # Receive reply on same socket
            data, _addr = sock.recvfrom(1024)

            assert data == b"ECHO:TEST"
            # Both send and receive used same local port
            assert sock.getsockname() == local_addr

            echo_thread.join(timeout=1)
        finally:
            sock.close()
            echo_sock.close()

    def test_no_bind_means_no_firewall_rule_needed(self):
        """Verify socket without bind() works (no inbound firewall rule needed)."""
        # Client socket without bind()
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

        # Server socket with bind()
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)

        client_sock.settimeout(2)

        def server_echo():
            _data, addr = server_sock.recvfrom(1024)
            server_sock.sendto(b"REPLY", addr)

        try:
            server_thread = threading.Thread(target=server_echo, daemon=True)
            server_thread.start()

            # Client initiates - no bind() needed
            client_sock.sendto(b"REQUEST", server_addr)

            # Client can receive reply (firewall allows responses to outbound)
            data, _ = client_sock.recvfrom(1024)

            assert data == b"REPLY"
            # Client port was automatically assigned (ephemeral)
            assert client_sock.getsockname()[1] > 1024

            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()


class TestAddressTracking:
    """Test host's ability to track and update client addresses."""

    def test_address_updates_on_new_message(self):
        """Verify host updates tracked address when client port changes."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()

        tracked_addresses = []

        def server_tracker():
            """Track multiple client addresses."""
            for _ in range(2):
                _data, addr = server_sock.recvfrom(1024)
                tracked_addresses.append(addr)

        try:
            server_thread = threading.Thread(target=server_tracker, daemon=True)
            server_thread.start()

            # First client socket
            client1 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client1.sendto(b"MSG1", server_addr)
            client1.close()

            # Second client socket (different ephemeral port)
            client2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client2.sendto(b"MSG2", server_addr)
            client2.close()

            server_thread.join(timeout=1)

            # Server should have seen two different ports
            assert len(tracked_addresses) == 2
            assert tracked_addresses[0][1] != tracked_addresses[1][1]

        finally:
            server_sock.close()

    def test_host_remembers_last_client_port(self):
        """Verify host tracks most recent client port for responses."""
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)

        current_client_addr = [None]

        def server_handler():
            """Server tracks latest client address."""
            # First message
            _, addr = server_sock.recvfrom(1024)
            current_client_addr[0] = addr

            # Second message updates address
            _, addr = server_sock.recvfrom(1024)
            current_client_addr[0] = addr

            # Reply to most recent address
            if current_client_addr[0]:
                server_sock.sendto(b"REPLY", current_client_addr[0])

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()

            # First client
            client1 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client1.sendto(b"OLD", server_addr)

            # Second client (most recent)
            client2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client2.settimeout(2)
            client2.sendto(b"NEW", server_addr)

            # Server should reply to client2 (most recent)
            data, _ = client2.recvfrom(1024)

            assert data == b"REPLY"

            server_thread.join(timeout=1)
            client1.close()
            client2.close()
        finally:
            server_sock.close()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
