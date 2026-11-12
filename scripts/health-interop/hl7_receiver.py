#!/usr/bin/env python3
"""An independent MLLP receiver for the health pack's live tier, built on python-hl7.

    PYTHONPATH=<hl7 wheel> python3 hl7_receiver.py HOST PORT OUT_DIR

Each message is parsed by python-hl7, written to OUT_DIR/<MSH-10>.hl7 and acknowledged with
python-hl7's own ACK (MSA|AA). A message whose MSH-10 starts with REJECT is answered with AR.
Started by scripts/health-interop.sh; used by crates/cameleer-health/tests/live_health_hl7.rs.
"""

import asyncio
import os
import sys

import hl7
from hl7.mllp import start_hl7_server


async def handle(reader, writer):
    try:
        while not writer.is_closing():
            message = await reader.readmessage()
            control_id = str(message.segment("MSH")[10])
            path = os.path.join(OUT_DIR, "%s.hl7" % control_id)
            with open(path, "w", encoding="utf-8", newline="") as f:
                f.write(str(message))
            code = "AR" if control_id.startswith("REJECT") else "AA"
            writer.writemessage(message.create_ack(ack_code=code))
            await writer.drain()
    except asyncio.IncompleteReadError:
        pass
    finally:
        writer.close()


async def main(host, port):
    async with await start_hl7_server(handle, host=host, port=port) as server:
        await server.serve_forever()


if __name__ == "__main__":
    HOST, PORT, OUT_DIR = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    os.makedirs(OUT_DIR, exist_ok=True)
    asyncio.run(main(HOST, PORT))
