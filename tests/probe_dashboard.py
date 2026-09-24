"""Exercise the real dashboard's ReportConfig task against a running agent."""

import argparse
import base64
import hashlib
import http.cookiejar
import json
import ntpath
import os
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dashboard", default="http://127.0.0.1:18538")
    parser.add_argument("--server-id", type=int)
    parser.add_argument("--agent-temp-dir")
    parser.add_argument("--uuid", required=True)
    parser.add_argument("--apply-report-delay", type=int)
    parser.add_argument("--check-geoip", action="store_true")
    parser.add_argument("--check-monitor", action="store_true")
    parser.add_argument("--dump-monitor", action="store_true")
    parser.add_argument("--check-exec", action="store_true")
    parser.add_argument("--check-fs", action="store_true")
    parser.add_argument("--check-transfer", action="store_true")
    parser.add_argument("--check-transfer-large", action="store_true")
    parser.add_argument("--check-monitors", action="store_true")
    parser.add_argument("--https-monitor-url")
    parser.add_argument("--check-command", action="store_true")
    parser.add_argument("--check-nat", action="store_true")
    parser.add_argument("--check-terminal", action="store_true")
    parser.add_argument("--reload-during-terminal", action="store_true")
    parser.add_argument("--check-file-manager", action="store_true")
    parser.add_argument("--force-update", action="store_true")
    args = parser.parse_args()
    if args.reload_during_terminal:
        args.check_terminal = True
    if args.check_transfer_large:
        args.check_transfer = True
    if args.https_monitor_url:
        args.check_monitors = True

    cookies = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))
    login = urllib.request.Request(
        args.dashboard + "/api/v1/login",
        data=json.dumps({"username": "admin", "password": "admin"}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with opener.open(login, timeout=10) as response:
        envelope = json.load(response)
    assert envelope["success"], envelope.get("error")

    auth = {"Authorization": "Bearer " + envelope["data"]["token"]}

    request = urllib.request.Request(args.dashboard + "/api/v1/server", headers=auth)
    with opener.open(request, timeout=10) as response:
        inventory = json.load(response)
    server = next(item for item in inventory["data"] if item["uuid"] == args.uuid)
    agent_windows = server.get("host", {}).get("platform", "").startswith("Microsoft Windows")
    agent_join = ntpath.join if agent_windows else os.path.join
    if agent_windows and (args.check_fs or args.check_transfer or args.check_file_manager) and not args.agent_temp_dir:
        parser.error("--agent-temp-dir is required for Windows filesystem checks")
    agent_temp_dir = args.agent_temp_dir or "/tmp"
    if args.server_id is None:
        args.server_id = server["id"]
    else:
        assert args.server_id == server["id"], "server ID does not match UUID"

    def get_config():
        request = urllib.request.Request(
            args.dashboard + f"/api/v1/server/config/{args.server_id}",
            headers=auth,
        )
        with opener.open(request, timeout=15) as response:
            answer = json.load(response)
        if not answer.get("success"):
            raise RuntimeError(answer.get("error", "dashboard returned no success flag"))
        if not answer.get("data"):
            raise RuntimeError("dashboard returned no agent config")
        return json.loads(answer["data"])

    config = get_config()
    assert config["uuid"] == args.uuid, "dashboard received a different agent identity"
    assert config["server"], "dashboard received an empty agent server address"
    print("ReportConfig task round-trip passed for UUID", config["uuid"])

    if args.dump_monitor:
        print(json.dumps({"host": server.get("host"), "state": server.get("state")}, sort_keys=True))

    if args.check_monitor:
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        baseline = json.loads(subprocess.check_output(
            ["go", "run", "../tests/monitor_baseline.go"],
            cwd=os.path.join(root, "upstream-agent"), timeout=90))
        host = server["host"]
        fields = {"platform": "Platform", "platform_version": "PlatformVersion",
                  "cpu": "CPU", "mem_total": "MemTotal", "disk_total": "DiskTotal",
                  "swap_total": "SwapTotal", "arch": "Arch",
                  "virtualization": "Virtualization", "boot_time": "BootTime"}
        for rust_key, go_key in fields.items():
            assert host[rust_key] == baseline["host"][go_key], (
                rust_key, host[rust_key], baseline["host"][go_key])
        rust_state = server["state"]
        go_state = baseline["state"]
        assert abs(rust_state["disk_used"] - go_state["DiskUsed"]) < 256 * 1024 * 1024, (
            rust_state["disk_used"], go_state["DiskUsed"])
        print("Host fields and disk usage match upstream Go monitor in WSL")

    if args.check_geoip:
        assert server["geoip"]["ip"]["ipv4_addr"] == "127.0.0.1", server.get("geoip")
        print("GeoIP report reached dashboard for UUID", args.uuid)

    if args.check_exec or args.check_fs or args.check_transfer:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        scopes = []
        if args.check_exec:
            scopes.append("nezha:server:exec")
        if args.check_fs or args.check_transfer:
            scopes.extend(("nezha:server:read", "nezha:server:write", "nezha:server:delete"))
        def create_probe_token(name):
            request = urllib.request.Request(
                args.dashboard + "/api/v1/api-tokens",
                data=json.dumps({"name": name, "scopes": scopes}).encode(),
                headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
            )
            with opener.open(request, timeout=10) as response:
                answer = json.load(response)
            assert answer["success"], answer.get("error")
            return answer["data"]["token"], answer["data"]["id"]

        token, token_id = create_probe_token("rust-agent-local-probe")
        token_ids = [token_id]
        try:
            def call_tool(name, arguments, expect_error=False):
                request = urllib.request.Request(
                    args.dashboard + "/mcp",
                    data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                     "params": {"name": name, "arguments": {
                                         "server_id": args.server_id, **arguments}}}).encode(),
                    headers={"Authorization": "Bearer " + token,
                             "Content-Type": "application/json"},
                )
                with opener.open(request, timeout=40) as response:
                    answer = json.load(response)
                assert "result" in answer, answer
                if expect_error:
                    assert answer["result"].get("isError"), answer
                    return answer["result"]["content"][0]["text"]
                assert not answer["result"].get("isError"), answer
                return answer["result"]["structuredContent"]

            if args.check_exec:
                command = ({"cmd": "cmd.exe", "args": ["/C", "echo compat-exec"]}
                           if agent_windows else
                           {"cmd": "/bin/sh", "args": ["-c", "printf compat-exec"]})
                result = call_tool("server.exec", command)
                assert result["exit_code"] == 0 and result["stdout"].strip() == "compat-exec", result
                print("MCP server.exec round-trip passed for UUID", args.uuid)

            if args.check_fs:
                root = agent_join(agent_temp_dir, f"nezha-rust-fs-probe-{args.uuid}-{time.time_ns()}")
                nested = agent_join(root, "nested")
                path = agent_join(nested, "probe.txt")
                hidden = agent_join(nested, ".hidden")
                payload = b"compat-fs-probe-123"
                digest = hashlib.sha256(payload).hexdigest()
                try:
                    absent = call_tool("fs.delete", {"path": agent_join(root, "missing", "parent", "child"),
                                                     "recursive": True})
                    assert absent["deleted_count"] == 0, absent
                    empty = agent_join(root, "empty")
                    marker = agent_join(empty, "marker")
                    call_tool("fs.write", {"path": marker, "content": "x", "create_dirs": True})
                    assert call_tool("fs.delete", {"path": marker})["deleted_count"] == 1
                    assert call_tool("fs.delete", {"path": empty})["deleted_count"] == 1
                    token, next_token_id = create_probe_token("rust-agent-fs-delete-probe")
                    token_ids.append(next_token_id)
                    written = call_tool("fs.write", {"path": path, "mode": "0640",
                        "content": base64.b64encode(payload).decode(),
                        "encoding": "base64", "create_dirs": True})
                    assert written["size"] == len(payload) and written["sha256"] == digest, written
                    listed = call_tool("fs.list", {"path": nested})
                    assert [entry["name"] for entry in listed["entries"]] == ["probe.txt"], listed
                    assert listed["entries"][0]["mode"] == ("0666" if agent_windows else "0640"), listed
                    read = call_tool("fs.read", {"path": path, "offset": 7,
                                                  "length": 8, "encoding": "base64"})
                    assert base64.b64decode(read["content"]) == payload[7:15], read
                    assert read["sha256"] == hashlib.sha256(payload[7:15]).hexdigest(), read
                    assert read["truncated"], read
                    conflict = call_tool("fs.write", {"path": path, "content": "rejected",
                        "if_match_sha256": "0" * 64}, expect_error=True)
                    assert "sha256 mismatch" in conflict, conflict
                    updated = call_tool("fs.write", {"path": path, "content": "updated",
                                                      "if_match_sha256": digest})
                    assert updated["size"] == 7, updated
                    call_tool("fs.write", {"path": hidden, "content": "hidden"})
                    visible = call_tool("fs.list", {"path": nested})
                    assert visible["total"] == 1 and [entry["name"] for entry in visible["entries"]] == ["probe.txt"], visible
                    all_entries = call_tool("fs.list", {"path": nested,
                                                        "show_hidden": True})
                    assert all_entries["total"] == 2 and sorted(entry["name"] for entry in all_entries["entries"]) == [".hidden", "probe.txt"], all_entries
                    token, next_token_id = create_probe_token("rust-agent-fs-errors-probe")
                    token_ids.append(next_token_id)
                    missing = call_tool("fs.read", {"path": agent_join(nested, "missing.txt")}, expect_error=True)
                    assert "file or directory does not exist" in missing, missing
                    invalid_encoding = call_tool("fs.read", {"path": path, "encoding": "rot13"}, expect_error=True)
                    assert "unknown encoding" in invalid_encoding, invalid_encoding
                    invalid_mode = call_tool("fs.write", {"path": agent_join(nested, "invalid.txt"),
                                                          "content": "x", "mode": "invalid"}, expect_error=True)
                    assert "invalid mode" in invalid_mode, invalid_mode
                    nonrecursive = call_tool("fs.delete", {"path": root}, expect_error=True)
                    assert "internal agent error" in nonrecursive, nonrecursive
                    print("MCP fs.list/read/write round-trips passed for UUID", args.uuid)
                finally:
                    deleted = call_tool("fs.delete", {"path": root, "recursive": True})
                    assert deleted["deleted_count"] == 4, deleted
                print("MCP fs.delete round-trip passed for UUID", args.uuid)
            if args.check_transfer:
                if args.check_exec or args.check_fs:
                    token, transfer_token_id = create_probe_token("rust-agent-transfer-probe")
                    token_ids.append(transfer_token_id)
                root = agent_join(agent_temp_dir, f"nezha-rust-transfer-{args.uuid}-{time.time_ns()}")
                nested = agent_join(root, "nested")
                path = agent_join(nested, "data.bin")
                empty = agent_join(nested, "empty.bin")
                payload = bytes(range(256)) * 8193
                digest = hashlib.sha256(payload).hexdigest()

                def upload_url(target, **options):
                    result = call_tool("fs.upload_url", {"path": target, **options})
                    assert result["method"] == "POST", result
                    return result["url"]

                def upload_body(url, body, sha=None):
                    if sha is not None:
                        url += "?sha256=" + sha
                    request = urllib.request.Request(url, data=body, method="POST",
                                                     headers={"Content-Type": "application/octet-stream"})
                    with opener.open(request, timeout=80) as response:
                        return json.load(response)

                def download_body(target):
                    result = call_tool("fs.download_url", {"path": target})
                    assert result["method"] == "GET", result
                    with opener.open(result["url"], timeout=80) as response:
                        body = response.read()
                    return result["url"], body

                try:
                    url = upload_url(path, mode="0640", create_dirs=True)
                    result = upload_body(url, payload, digest)
                    assert result["size"] == len(payload) and result["sha256"] == digest, result
                    listed = call_tool("fs.list", {"path": nested})
                    assert next(item for item in listed["entries"] if item["name"] == "data.bin")["mode"] == ("0666" if agent_windows else "0640"), listed
                    try:
                        opener.open(urllib.request.Request(url, data=b"again", method="POST"), timeout=10)
                    except urllib.error.HTTPError as error:
                        assert error.code == 401, error.code
                    else:
                        raise AssertionError("upload token replay was accepted")
                    url, actual = download_body(path)
                    assert actual == payload, (len(actual), len(payload))
                    try:
                        opener.open(url, timeout=10)
                    except urllib.error.HTTPError as error:
                        assert error.code == 401, error.code
                    else:
                        raise AssertionError("download token replay was accepted")

                    bad_url = upload_url(path, if_match_sha256="0" * 64)
                    try:
                        upload_body(bad_url, b"rejected")
                    except urllib.error.HTTPError as error:
                        assert error.code == 502 and b"sha256 mismatch" in error.read(), error
                    else:
                        raise AssertionError("stale if_match was accepted")

                    bad_url = upload_url(path, if_match_sha256=digest)
                    try:
                        upload_body(bad_url, b"wrong-hash", "0" * 64)
                    except urllib.error.HTTPError as error:
                        assert error.code == 502 and b"sha256 mismatch" in error.read(), error
                    else:
                        raise AssertionError("wrong upload hash was accepted")
                    _, actual = download_body(path)
                    assert actual == payload, "rejected upload changed existing file"

                    empty_digest = hashlib.sha256(b"").hexdigest()
                    started = time.monotonic()
                    result = upload_body(upload_url(empty), b"", empty_digest)
                    assert time.monotonic() - started < 15, "empty upload waited for keepalive"
                    assert result["size"] == 0 and result["sha256"] == empty_digest, result
                    _, actual = download_body(empty)
                    assert actual == b"", actual
                    if args.check_transfer_large:
                        large = agent_join(nested, "limit.bin")
                        large_payload = bytes(range(256)) * (100 * 1024 * 1024 // 256)
                        large_digest = hashlib.sha256(large_payload).hexdigest()
                        result = upload_body(upload_url(large), large_payload, large_digest)
                        assert result["size"] == len(large_payload) and result["sha256"] == large_digest, result
                        _, actual = download_body(large)
                        assert hashlib.sha256(actual).hexdigest() == large_digest, "100MiB download hash mismatch"
                        assert len(actual) == len(large_payload), (len(actual), len(large_payload))
                        print("MCP transfer 100MiB upload/download boundary passed")
                    print("MCP transfer HTTP upload/download, hash, empty file and error paths passed")
                finally:
                    deleted = call_tool("fs.delete", {"path": root, "recursive": True})
                    assert deleted["deleted_count"] >= 4, deleted
        finally:
            for token_id in token_ids:
                request = urllib.request.Request(
                    args.dashboard + f"/api/v1/api-tokens/{token_id}", method="DELETE",
                    headers={**auth, "X-CSRF-Token": csrf},
                )
                opener.open(request, timeout=10).close()

    if args.check_monitors:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        cases = [(1, "http://127.0.0.1:18538/api/v1/service"),
                 (2, "127.0.0.1"), (3, "127.0.0.1:18538")]
        if args.https_monitor_url:
            cases.append((1, args.https_monitor_url))
        created = []
        try:
            for index, (kind, target) in enumerate(cases):
                request = urllib.request.Request(
                    args.dashboard + "/api/v1/service",
                    data=json.dumps({"name": f"rust-agent-probe-{index}", "type": kind,
                                     "target": target, "duration": 5, "cover": 1,
                                     "skip_servers": {str(args.server_id): True}}).encode(),
                    headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
                )
                with opener.open(request, timeout=10) as response:
                    answer = json.load(response)
                assert answer["success"], answer
                created.append(answer["data"])
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                request = urllib.request.Request(args.dashboard + "/api/v1/service", headers=auth)
                with opener.open(request, timeout=10) as response:
                    answer = json.load(response)
                assert answer["success"], answer
                stats = answer["data"].get("services") or {}
                if all(stats.get(str(service_id), {}).get("total_up", 0) > 0 for service_id in created):
                    print("HTTP, ICMP and TCP monitor results reached dashboard for UUID", args.uuid)
                    break
                time.sleep(1)
            else:
                raise AssertionError(f"monitor results did not all succeed: {stats}")
        finally:
            if created:
                request = urllib.request.Request(
                    args.dashboard + "/api/v1/batch-delete/service",
                    data=json.dumps(created).encode(),
                    headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
                )
                with opener.open(request, timeout=10) as response:
                    answer = json.load(response)
                assert answer["success"], answer

    if args.check_command:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        request = urllib.request.Request(
            args.dashboard + "/api/v1/cron",
            data=json.dumps({"name": "rust-agent-command-probe", "task_type": 1,
                             "command": "echo compat-command" if server["host"]["platform"].startswith("Microsoft Windows") else "printf compat-command", "cover": 0,
                             "servers": [args.server_id]}).encode(),
            headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
        )
        with opener.open(request, timeout=10) as response:
            answer = json.load(response)
        assert answer["success"], answer
        cron_id = answer["data"]
        try:
            request = urllib.request.Request(
                args.dashboard + f"/api/v1/cron/{cron_id}/manual", data=b"{}",
                headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
            )
            with opener.open(request, timeout=10) as response:
                answer = json.load(response)
            assert answer["success"], answer
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                request = urllib.request.Request(args.dashboard + "/api/v1/cron", headers=auth)
                with opener.open(request, timeout=10) as response:
                    answer = json.load(response)
                assert answer["success"], answer
                cron = next(item for item in answer["data"] if item["id"] == cron_id)
                if cron.get("last_result"):
                    print("Legacy command task result reached dashboard for UUID", args.uuid)
                    break
                time.sleep(1)
            else:
                raise AssertionError(f"command task did not succeed: {cron}")
        finally:
            request = urllib.request.Request(
                args.dashboard + "/api/v1/batch-delete/cron", data=json.dumps([cron_id]).encode(),
                headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
            )
            with opener.open(request, timeout=10) as response:
                answer = json.load(response)
            assert answer["success"], answer

    if args.check_nat:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        domain = f"rust-nat-probe-{args.uuid}.local"
        request = urllib.request.Request(
            args.dashboard + "/api/v1/nat",
            data=json.dumps({"name": "rust-agent-nat-probe", "enabled": True,
                             "server_id": args.server_id, "host": "127.0.0.1:18540",
                             "domain": domain}).encode(),
            headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
        )
        with opener.open(request, timeout=10) as response:
            answer = json.load(response)
        assert answer["success"], answer
        nat_id = answer["data"]
        try:
            request = urllib.request.Request(
                args.dashboard + "/", headers={"Host": domain},
            )
            with urllib.request.urlopen(request, timeout=20) as response:
                body = response.read()
                assert response.status == 200, response.status
            assert b"Nezha Agent Rust rewrite" in body or b"Directory listing" in body, body[:200]
            print("NAT HTTP request reached local backend through dashboard for UUID", args.uuid)
        finally:
            request = urllib.request.Request(
                args.dashboard + "/api/v1/batch-delete/nat", data=json.dumps([nat_id]).encode(),
                headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
            )
            with opener.open(request, timeout=10) as response:
                answer = json.load(response)
            assert answer["success"], answer

    if args.check_terminal:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        request = urllib.request.Request(
            args.dashboard + "/api/v1/terminal",
            data=json.dumps({"server_id": args.server_id}).encode(),
            headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
        )
        with opener.open(request, timeout=10) as response:
            answer = json.load(response)
        assert answer["success"], answer
        session_id = answer["data"]["session_id"]
        parsed = urllib.parse.urlparse(args.dashboard)

        def recv_exact(connection, size):
            result = bytearray()
            while len(result) < size:
                chunk = connection.recv(size - len(result))
                if not chunk:
                    raise ConnectionError("terminal websocket closed")
                result.extend(chunk)
            return bytes(result)

        def send_frame(connection, opcode, data):
            mask = os.urandom(4)
            length = len(data)
            if length < 126:
                header = bytes((0x80 | opcode, 0x80 | length))
            elif length < 65536:
                header = bytes((0x80 | opcode, 0xfe)) + length.to_bytes(2, "big")
            else:
                header = bytes((0x80 | opcode, 0xff)) + length.to_bytes(8, "big")
            connection.sendall(header + mask +
                               bytes(value ^ mask[index % 4] for index, value in enumerate(data)))

        with socket.create_connection((parsed.hostname, parsed.port), timeout=15) as connection:
            connection.settimeout(15)
            key = base64.b64encode(os.urandom(16)).decode()
            handshake = (f"GET /api/v1/ws/terminal/{session_id} HTTP/1.1\r\n"
                         f"Host: {parsed.hostname}:{parsed.port}\r\n"
                         f"Authorization: {auth['Authorization']}\r\n"
                         "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                         f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
            connection.sendall(handshake.encode())
            header = bytearray()
            while not header.endswith(b"\r\n\r\n"):
                header.extend(recv_exact(connection, 1))
                assert len(header) < 8192, "oversize websocket handshake"
            assert header.startswith(b"HTTP/1.1 101"), header.decode(errors="replace")
            send_frame(connection, 2, b'\x01{"Cols":80,"Rows":24}')
            command = (b"set /a 19*23\r\nmode con\r\n" if agent_windows else
                       b"echo terminal-$((19*23))\nstty size\n")
            send_frame(connection, 1, command)
            output = bytearray()
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                first, second = recv_exact(connection, 2)
                opcode = first & 0x0f
                length = second & 0x7f
                if length == 126:
                    length = int.from_bytes(recv_exact(connection, 2), "big")
                elif length == 127:
                    length = int.from_bytes(recv_exact(connection, 8), "big")
                assert length <= 1024 * 1024, "oversize terminal frame"
                mask = recv_exact(connection, 4) if second & 0x80 else None
                payload = recv_exact(connection, length)
                if mask:
                    payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
                if opcode == 9:
                    send_frame(connection, 10, payload)
                elif opcode == 2:
                    output.extend(payload)
                    if agent_windows and b"\x1b[6n" in payload:
                        send_frame(connection, 1, b"\x1b[1;1R")
                    computed = b"437" in output if agent_windows else b"terminal-437" in output
                    resized = (b"24" in output and b"80" in output) if agent_windows else b"24 80" in output
                    if computed and resized:
                        if args.reload_during_terminal:
                            config["report_delay"] = 3
                            request = urllib.request.Request(
                                args.dashboard + "/api/v1/server/config",
                                data=json.dumps({"servers": [args.server_id], "config": json.dumps(config)}).encode(),
                                headers={**auth, "Content-Type": "application/json",
                                         "X-CSRF-Token": csrf},
                            )
                            with opener.open(request, timeout=15) as response:
                                answer = json.load(response)
                            assert answer["success"], answer
                            connection.settimeout(20)
                            for _ in range(100):
                                try:
                                    first, second = recv_exact(connection, 2)
                                except ConnectionError:
                                    break
                                length = second & 0x7f
                                if length == 126:
                                    length = int.from_bytes(recv_exact(connection, 2), "big")
                                elif length == 127:
                                    length = int.from_bytes(recv_exact(connection, 8), "big")
                                assert length <= 1024 * 1024
                                payload = recv_exact(connection, length)
                                if first & 0x0f == 8:
                                    send_frame(connection, 8, payload)
                                    break
                                if first & 0x0f == 9:
                                    send_frame(connection, 10, payload)
                            else:
                                raise AssertionError("terminal did not close on agent reload")
                            print("Active terminal closed during agent reload for UUID", args.uuid)
                        else:
                            send_frame(connection, 1, b"exit\r\n" if agent_windows else b"exit\n")
                        print("PTY terminal output reached dashboard WebSocket for UUID", args.uuid)
                        break
                elif opcode == 8:
                    raise ConnectionError(f"terminal closed before output: {output!r}")
            else:
                raise AssertionError(f"terminal output not observed: {output!r}")

    if args.check_file_manager:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        request = urllib.request.Request(
            args.dashboard + f"/api/v1/file?id={args.server_id}", data=b"{}",
            headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
        )
        with opener.open(request, timeout=10) as response:
            answer = json.load(response)
        assert answer["success"], answer
        session_id = answer["data"]["session_id"]
        parsed = urllib.parse.urlparse(args.dashboard)

        def recv_exact(connection, size):
            result = bytearray()
            while len(result) < size:
                chunk = connection.recv(size - len(result))
                if not chunk:
                    raise ConnectionError("file manager websocket closed")
                result.extend(chunk)
            return bytes(result)

        def send_binary(connection, data):
            mask = os.urandom(4)
            size = len(data)
            header = bytes((0x82, 0x80 | size)) if size < 126 else (
                bytes((0x82, 0xfe)) + size.to_bytes(2, "big") if size < 65536 else
                bytes((0x82, 0xff)) + size.to_bytes(8, "big"))
            connection.sendall(header + mask +
                               bytes(value ^ mask[index % 4] for index, value in enumerate(data)))

        def recv_binary(connection):
            while True:
                first, second = recv_exact(connection, 2)
                size = second & 0x7f
                if size == 126:
                    size = int.from_bytes(recv_exact(connection, 2), "big")
                elif size == 127:
                    size = int.from_bytes(recv_exact(connection, 8), "big")
                assert size <= 4 * 1024 * 1024, size
                mask = recv_exact(connection, 4) if second & 0x80 else None
                payload = recv_exact(connection, size)
                if mask:
                    payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
                if first & 0x0f == 9:
                    connection.sendall(b"\x8a\x80" + b"\0\0\0\0")
                elif first & 0x0f == 2 and payload:
                    return payload
                elif first & 0x0f == 8:
                    raise ConnectionError("file manager websocket closed")

        local_temp_dir = None
        if agent_windows:
            local_temp_dir = subprocess.check_output(
                ["wslpath", "-u", agent_temp_dir], text=True).strip()
        with tempfile.TemporaryDirectory(prefix="nezha-rust-fm-", dir=local_temp_dir) as directory:
            source = os.path.join(directory, "source.bin")
            target = os.path.join(directory, "uploaded.bin")
            oversend_target = os.path.join(directory, "oversend.bin")
            remote_directory = (subprocess.check_output(["wslpath", "-w", directory], text=True).strip()
                                if agent_windows else directory)
            remote_source = agent_join(remote_directory, "source.bin")
            remote_target = agent_join(remote_directory, "uploaded.bin")
            remote_oversend_target = agent_join(remote_directory, "oversend.bin")
            payload = b"file-manager-compat-" * 1024
            with open(source, "wb") as output:
                output.write(payload)
            with socket.create_connection((parsed.hostname, parsed.port), timeout=15) as connection:
                connection.settimeout(15)
                key = base64.b64encode(os.urandom(16)).decode()
                handshake = (f"GET /api/v1/ws/file/{session_id} HTTP/1.1\r\n"
                             f"Host: {parsed.hostname}:{parsed.port}\r\n"
                             f"Authorization: {auth['Authorization']}\r\n"
                             "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                             f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
                connection.sendall(handshake.encode())
                header = bytearray()
                while not header.endswith(b"\r\n\r\n"):
                    header.extend(recv_exact(connection, 1))
                    assert len(header) < 8192
                assert header.startswith(b"HTTP/1.1 101"), header.decode(errors="replace")

                send_binary(connection, b"\0" + remote_directory.encode())
                listing = recv_binary(connection)
                assert listing.startswith(b"NZFN"), listing[:40]
                path_size = int.from_bytes(listing[4:8], "big")
                assert listing[8:8 + path_size].decode() == remote_directory, listing
                assert b"source.bin" in listing[8 + path_size:], listing

                send_binary(connection, b"\x01" + remote_source.encode())
                file_header = recv_binary(connection)
                assert file_header[:4] == b"NZTD", file_header[:40]
                size = int.from_bytes(file_header[4:12], "big")
                downloaded = bytearray()
                while len(downloaded) < size:
                    downloaded.extend(recv_binary(connection))
                assert bytes(downloaded) == payload, (len(downloaded), len(payload))

                send_binary(connection, b"\x02" + len(payload).to_bytes(8, "big") + remote_target.encode())
                send_binary(connection, payload)
                assert recv_binary(connection) == b"NZUP"
                send_binary(connection, b"\x02" + (4).to_bytes(8, "big") + remote_oversend_target.encode())
                send_binary(connection, b"body-plus-extra")
                assert recv_binary(connection) == b"NZUP"
            with open(target, "rb") as input_file:
                assert input_file.read() == payload
            with open(oversend_target, "rb") as input_file:
                assert input_file.read() == b"body-plus-extra"
        print("Legacy file manager list/download/upload passed through dashboard WebSocket")

    if args.apply_report_delay is not None:
        config["report_delay"] = args.apply_report_delay
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        request = urllib.request.Request(
            args.dashboard + "/api/v1/server/config",
            data=json.dumps({"servers": [args.server_id], "config": json.dumps(config)}).encode(),
            headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
        )
        with opener.open(request, timeout=15) as response:
            answer = json.load(response)
        assert answer["success"], answer.get("error")
        assert args.server_id in answer["data"]["success"], answer["data"]
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            time.sleep(1)
            try:
                updated = get_config()
            except (RuntimeError, ValueError, TimeoutError, urllib.error.URLError):
                continue
            if updated["report_delay"] == args.apply_report_delay:
                print("ApplyConfig persisted and reconnected with report_delay", args.apply_report_delay)
                break
        else:
            raise AssertionError("ApplyConfig did not reconnect with the new report_delay")

    if args.force_update:
        csrf = next(cookie.value for cookie in cookies if cookie.name == "nz-csrf")
        request = urllib.request.Request(
            args.dashboard + "/api/v1/force-update/server",
            data=json.dumps([args.server_id]).encode(),
            headers={**auth, "Content-Type": "application/json", "X-CSRF-Token": csrf},
        )
        with opener.open(request, timeout=15) as response:
            answer = json.load(response)
        assert answer["success"], answer.get("error")
        assert args.server_id in answer["data"]["success"], answer["data"]
        print("Force-update task dispatched to", args.server_id)


if __name__ == "__main__":
    main()
