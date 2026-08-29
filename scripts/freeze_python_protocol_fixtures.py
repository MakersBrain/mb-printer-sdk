#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Execute the Python driver and freeze its observable wire actions.

This deliberately imports and calls the reference implementation instead of
transcribing protocol constants. Run from the SDK with:

  python scripts/freeze_python_protocol_fixtures.py ../mb-cli-printer \
    fixtures/protocol/python-actions.json
"""

from __future__ import annotations

import asyncio
import json
import subprocess
import sys
from pathlib import Path


class CaptureTransport:
    name = "fixture-capture"
    max_write = 65_535

    def __init__(self) -> None:
        self.actions: list[dict[str, object]] = []

    async def send(self, data: bytes) -> None:
        self.actions.append({"action": "write", "hex": data.hex()})

    async def delay(self, ms: int) -> None:
        self.actions.append({"action": "delay", "milliseconds": ms})

    async def wait_for_response(self, timeout_ms: int = 500) -> bytes:
        self.actions.append({"action": "wait", "timeoutMs": timeout_ms})
        return b"\x01"


async def generate(reference: Path) -> dict[str, object]:
    sys.path.insert(0, str(reference))
    from mbprint.printers import by_id
    from PIL import Image
    from mbprint.protocol import PrintOptions, prepare_raster, print_raster
    from mbprint import media as media_module, raster as raster_module

    models = ["m03", "m02", "m04s-53", "m110", "d-series", "p12", "pm241", "ql-1100"]
    profiles = [
        {"name":"low","density":1,"feed":8,"continuous":False,"speed":2,"copies":1,"gap":10,"offset":-5,"align":"left","x":0,"y":0,"media":"29x42","cut":False,"cutEvery":1,"compress":False,"quality":False},
        {"name":"balanced","density":4,"feed":17,"continuous":True,"speed":4,"copies":2,"gap":25,"offset":-15,"align":"center","x":1,"y":1,"media":"62x29","cut":True,"cutEvery":2,"compress":True,"quality":True},
        {"name":"high","density":8,"feed":32,"continuous":False,"speed":6,"copies":3,"gap":40,"offset":10,"align":"right","x":2,"y":2,"media":"62x29","cut":True,"cutEvery":3,"compress":True,"quality":False},
    ]
    cases = []
    for model in models:
        printer = by_id(model)
        if printer is None:
            raise RuntimeError(f"reference model disappeared: {model}")
        for profile in profiles:
            transport = CaptureTransport()
            image = Image.new("1", (16, 8), 1)
            pixels = image.load()
            for y in range(8):
                for x in range(16):
                    if (x + 2 * y) % 5 < 2:
                        pixels[x, y] = 0
            media = media_module.by_id(profile["media"]) if printer.protocol == "brother" else None
            options = PrintOptions(density=profile["density"],feed=profile["feed"],continuous=profile["continuous"],
                speed=profile["speed"],copies=profile["copies"],gap_mm=profile["gap"]/10,tspl_offset_mm=profile["offset"]/10,
                label_width_mm=25.0,label_height_mm=15.0,align=profile["align"],offset_x=profile["x"],offset_y=profile["y"],
                media=media,cut=profile["cut"],cut_every=profile["cutEvery"],compress=profile["compress"],high_quality=profile["quality"])
            input_raster = raster_module.pack(image, "threshold")
            prepared = prepare_raster(image, printer, options, "threshold")
            await print_raster(transport, printer, prepared, options)
            cases.append(
            {
                "profile": profile["name"],
                "model": model,
                "protocol": printer.protocol,
                "printerRotated": printer.rotated,
                "inputRaster": {"widthBytes": input_raster.width_bytes, "height": input_raster.height, "hex": bytes(input_raster.data).hex()},
                "preparedRaster": {"widthBytes": prepared.width_bytes, "height": prepared.height, "hex": bytes(prepared.data).hex()},
                "options": {
                    "density": profile["density"],
                    "feed": profile["feed"],
                    "continuous": profile["continuous"],
                    "speed": profile["speed"],
                    "copies": profile["copies"],
                    "gapTenthsMm": profile["gap"],
                    "offsetTenthsMm": profile["offset"],
                    "labelWidthTenthsMm": 250,
                    "labelHeightTenthsMm": 150,
                    "alignment": profile["align"],
                    "offsetX": profile["x"],
                    "offsetY": profile["y"],
                    "brotherMedia": {"widthMm": round(media.width_mm), "lengthMm": round(media.length_mm), "continuous": media.continuous, "feedMargin": media.feed_margin} if media else None,
                    "brotherRightMarginDots": options.media.offset_r + printer.additional_offset_r + options.offset_x if printer.protocol == "brother" else None,
                    "cut": profile["cut"],
                    "cutEvery": profile["cutEvery"],
                    "compress": profile["compress"],
                    "highQuality": profile["quality"],
                },
                "actions": transport.actions,
            }
        )
    commit = subprocess.check_output(
        ["git", "-C", str(reference), "rev-parse", "HEAD"], text=True
    ).strip()
    return {
        "spdxLicense": "AGPL-3.0-or-later",
        "provenance": {
            "generator": "scripts/freeze_python_protocol_fixtures.py",
            "referenceRepository": "https://github.com/MakersBrain/mb-cli-printer",
            "referenceCommit": commit,
            "method": "captured Transport.send/delay/wait_for_response calls",
            "knownDivergences": ["Rust waits for and validates the requested Brother status; the Python print flow sends the request but does not await it."],
        },
        "cases": cases,
    }


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} PYTHON_REPOSITORY OUTPUT_JSON")
    reference, output = Path(sys.argv[1]), Path(sys.argv[2])
    payload = asyncio.run(generate(reference))
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
