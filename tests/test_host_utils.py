import socket
import threading
import pytest

class TestEphemeralPortCommunication:
    def test_client_can_send_without_bind(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            sock.sendto(b"TEST", ("192.0.2.1", 7004))
            local_addr = sock.getsockname()
            assert local_addr[0] != ""
            assert local_addr[1] > 1024
        finally:
            sock.close()

    def test_ephemeral_port_is_unique_per_socket(self):
        sock1 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            sock1.sendto(b"TEST1", ("192.0.2.1", 7004))
            sock2.sendto(b"TEST2", ("192.0.2.1", 7004))
            port1 = sock1.getsockname()[1]
            port2 = sock2.getsockname()[1]
            assert port1 != port2
            assert port1 > 1024
            assert port2 > 1024
        finally:
            sock1.close()
            sock2.close()

    def test_client_can_receive_on_ephemeral_port(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)
        received_response = []

        def server_responder():
            _data, addr = server_sock.recvfrom(1024)
            server_sock.sendto(b"RESPONSE", addr)

        try:
            server_thread = threading.Thread(target=server_responder, daemon=True)
            server_thread.start()
            client_sock.sendto(b"REQUEST", server_addr)
            data, addr = client_sock.recvfrom(1024)
            received_response.append(data)
            assert received_response[0] == b"RESPONSE"
            assert addr == server_addr
            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

class TestHostAddressTracking:
    def test_host_learns_client_port_from_message(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        learned_client_addr = [None]

        def server_listener():
            _data, addr = server_sock.recvfrom(1024)
            learned_client_addr[0] = addr

        try:
            server_thread = threading.Thread(target=server_listener, daemon=True)
            server_thread.start()
            client_sock.sendto(b"HELLO", server_addr)
            server_thread.join(timeout=1)
            assert learned_client_addr[0] is not None
            assert learned_client_addr[0][0] == "127.0.0.1"
            assert learned_client_addr[0][1] > 1024
        finally:
            client_sock.close()
            server_sock.close()

    def test_host_can_reply_to_learned_address(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)

        def echo_server():
            data, client_addr = server_sock.recvfrom(1024)
            server_sock.sendto(b"ECHO: " + data, client_addr)

        try:
            server_thread = threading.Thread(target=echo_server, daemon=True)
            server_thread.start()
            client_sock.sendto(b"TEST", server_addr)
            data, addr = client_sock.recvfrom(1024)
            assert data == b"ECHO: TEST"
            assert addr == server_addr
            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

class TestHeartbeatWithEphemeralPorts:
    def test_heartbeat_pong_establishes_connection(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)
        learned_addr = [None]

        def server_handler():
            data, addr = server_sock.recvfrom(256)
            if data == b"PONG":
                learned_addr[0] = addr
                server_sock.sendto(b"PING", addr)

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()
            client_sock.sendto(b"PONG", server_addr)
            data, _addr = client_sock.recvfrom(256)
            assert data == b"PING"
            assert learned_addr[0] is not None
            assert learned_addr[0][1] > 1024
            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

    def test_heartbeat_bidirectional_on_ephemeral_port(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)
        messages = []

        def server_handler():
            data, client_addr = server_sock.recvfrom(256)
            messages.append(("server_recv", data))
            server_sock.sendto(b"PING", client_addr)
            data, _ = server_sock.recvfrom(256)
            messages.append(("server_recv", data))

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()
            client_sock.sendto(b"PONG", server_addr)
            messages.append(("client_send", b"PONG"))
            data, _ = client_sock.recvfrom(256)
            messages.append(("client_recv", data))
            client_sock.sendto(b"PONG", server_addr)
            messages.append(("client_send", b"PONG"))
            server_thread.join(timeout=1)
            assert ("client_send", b"PONG") in messages
            assert ("server_recv", b"PONG") in messages
            assert ("client_recv", b"PING") in messages
        finally:
            client_sock.close()
            server_sock.close()

class TestClipboardWithEphemeralPorts:
    def test_clipboard_keepalive_registers_client(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)
        registered_addr = [None]

        def server_handler():
            data, addr = server_sock.recvfrom(1024)
            if data == b"CLIPBOARD_KEEPALIVE":
                registered_addr[0] = addr
                server_sock.sendto(b"CLIPBOARD_UPDATE HOST test", addr)

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()
            client_sock.sendto(b"CLIPBOARD_KEEPALIVE", server_addr)
            data, _ = client_sock.recvfrom(1024)
            assert data == b"CLIPBOARD_UPDATE HOST test"
            assert registered_addr[0] is not None
            assert registered_addr[0][1] > 1024
            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

    def test_clipboard_update_bidirectional(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client_sock.settimeout(2)
        messages = []

        def server_handler():
            data, client_addr = server_sock.recvfrom(1024)
            messages.append(("server", data))
            server_sock.sendto(b"CLIPBOARD_UPDATE HOST from_host", client_addr)

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()
            client_sock.sendto(b"CLIPBOARD_UPDATE CLIENT from_client", server_addr)
            data, _ = client_sock.recvfrom(1024)
            messages.append(("client", data))
            server_thread.join(timeout=1)
            assert ("server", b"CLIPBOARD_UPDATE CLIENT from_client") in messages
            assert ("client", b"CLIPBOARD_UPDATE HOST from_host") in messages
        finally:
            client_sock.close()
            server_sock.close()

class TestNATTraversal:
    def test_single_socket_bidirectional_communication(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("127.0.0.1", 0))
        sock.settimeout(2)
        local_addr = sock.getsockname()
        echo_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        echo_sock.bind(("127.0.0.1", 0))
        echo_addr = echo_sock.getsockname()

        def echo_server():
            data, addr = echo_sock.recvfrom(1024)
            echo_sock.sendto(b"ECHO:" + data, addr)

        try:
            echo_thread = threading.Thread(target=echo_server, daemon=True)
            echo_thread.start()
            sock.sendto(b"TEST", echo_addr)
            data, _ = sock.recvfrom(1024)
            assert data == b"ECHO:TEST"
            assert sock.getsockname() == local_addr
            echo_thread.join(timeout=1)
        finally:
            sock.close()
            echo_sock.close()

    def test_no_bind_means_no_firewall_rule_needed(self):
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
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
            client_sock.sendto(b"REQUEST", server_addr)
            data, _ = client_sock.recvfrom(1024)
            assert data == b"REPLY"
            assert client_sock.getsockname()[1] > 1024
            server_thread.join(timeout=1)
        finally:
            client_sock.close()
            server_sock.close()

class TestAddressTracking:
    def test_address_updates_on_new_message(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        tracked_addresses = []

        def server_tracker():
            for _ in range(2):
                _data, addr = server_sock.recvfrom(1024)
                tracked_addresses.append(addr)

        try:
            server_thread = threading.Thread(target=server_tracker, daemon=True)
            server_thread.start()
            client1 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client1.sendto(b"MSG1", server_addr)
            client1.close()
            client2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client2.sendto(b"MSG2", server_addr)
            client2.close()
            server_thread.join(timeout=1)
            assert len(tracked_addresses) == 2
            assert tracked_addresses[0][1] != tracked_addresses[1][1]
        finally:
            server_sock.close()

    def test_host_remembers_last_client_port(self):
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_sock.bind(("127.0.0.1", 0))
        server_addr = server_sock.getsockname()
        server_sock.settimeout(2)
        current_client_addr = [None]

        def server_handler():
            _, addr = server_sock.recvfrom(1024)
            current_client_addr[0] = addr
            _, addr = server_sock.recvfrom(1024)
            current_client_addr[0] = addr
            if current_client_addr[0]:
                server_sock.sendto(b"REPLY", current_client_addr[0])

        try:
            server_thread = threading.Thread(target=server_handler, daemon=True)
            server_thread.start()
            client1 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client1.sendto(b"OLD", server_addr)
            client2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client2.settimeout(2)
            client2.sendto(b"NEW", server_addr)
            data, _ = client2.recvfrom(1024)
            assert data == b"REPLY"
            server_thread.join(timeout=1)
            client1.close()
            client2.close()
        finally:
            server_sock.close()

if __name__ == "__main__":
    pytest.main([__file__, "-v"])
