"""Local DNS responder for the original-dashboard connection test."""

import argparse
import ipaddress
import socket
import struct


def question(packet):
    offset = 12
    labels = []
    while offset < len(packet):
        length = packet[offset]
        offset += 1
        if length == 0:
            break
        if length > 63 or offset + length > len(packet):
            raise ValueError("invalid DNS question")
        labels.append(packet[offset:offset + length].decode("ascii"))
        offset += length
    if offset + 4 > len(packet):
        raise ValueError("truncated DNS question")
    qtype, qclass = struct.unpack_from("!HH", packet, offset)
    return ".".join(labels).lower(), qtype, qclass, offset + 4


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18553)
    parser.add_argument("--name", default="dashboard.test")
    parser.add_argument("--address", default="127.0.0.1")
    args = parser.parse_args()
    answer = ipaddress.IPv4Address(args.address).packed
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as server:
        server.bind(("127.0.0.1", args.port))
        print(f"DNS fixture listening on 127.0.0.1:{args.port}", flush=True)
        while True:
            packet, peer = server.recvfrom(4096)
            try:
                name, qtype, qclass, end = question(packet)
            except (ValueError, UnicodeDecodeError):
                continue
            found = name == args.name and qtype == 1 and qclass == 1
            flags = 0x8180 if found else 0x8183
            response = packet[:2] + struct.pack("!HHHHH", flags, 1, int(found), 0, 0)
            response += packet[12:end]
            if found:
                response += b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 60, 4) + answer
            server.sendto(response, peer)


if __name__ == "__main__":
    main()
