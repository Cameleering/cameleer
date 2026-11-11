#!/usr/bin/env python3
"""Send one HL7 v2 message over MLLP with python-hl7's client and print the acknowledgement.

    PYTHONPATH=<hl7 wheel> python3 hl7_send.py HOST PORT < message.hl7

Segments may be separated by CR or LF on input; python-hl7 sends them with CR. The
acknowledgement is printed with segments on separate lines. Used by
crates/cameleer-health/tests/live_health_hl7.rs against a Cameleer MLLP consumer.
"""

import sys

import hl7
from hl7.client import MLLPClient


def main():
    host, port = sys.argv[1], int(sys.argv[2])
    text = sys.stdin.read().replace("\r\n", "\r").replace("\n", "\r").strip("\r")
    message = hl7.parse(text)
    with MLLPClient(host, port) as client:
        ack = client.send_message(message)
    if isinstance(ack, bytes):
        ack = ack.decode("utf-8")
    ack = ack.strip("\x0b\x1c\r\n")
    print(ack.replace("\r", "\n"))


if __name__ == "__main__":
    main()
