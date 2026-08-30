#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Generate Phomemo media definitions from the vendor's own sources.

Two sources, because the vendor splits them: the tape families are served by
the public paper API, keyed by the serial-number prefix a printer advertises,
while the M-series list ships inside the application as an offline table.
Neither vendor file is redistributed; this writes our own catalogue.

    python3 scripts/import_phomemo_paper.py \
        --devices <extracted>/assets/DefaultPrinter.json \
        --local-paper <extracted>/assets/localPaper.json

Pass --offline to skip the network and use only the bundled table.
"""

from __future__ import annotations

import argparse
import json
import re
import time
import urllib.parse
import urllib.request
from datetime import date
from pathlib import Path

BASE_URL = "https://api-oversea.qu-in.top/"
# Our printer id -> the model names the vendor registry uses.
MODELS = {
    "m02": ["M02", "M02S"],
    "m02-pro": ["M02 PRO", "M02Pro"],
    "m03": ["M03"],
    "t02": ["T02"],
    "m04s-53": ["M04S"],
    "m04s-80": ["M04S"],
    "m04s-110": ["M04S"],
    "m110": ["M110", "M110C"],
    "m110s": ["M110SA", "M110S"],
    "m200": ["M200", "M200C"],
    "m220": ["M220", "M220C", "M220S"],
    "m221": ["M221"],
    "m250": ["M250"],
    "m260": ["M260"],
    "d-series": ["D30", "D30S", "D30Pro", "D30N", "D35", "D50"],
    "p12": ["P12", "P12Pro"],
    "a30": ["A30"],
}
# Families whose second dimension is the one crossing the head: tape stock is
# named by its length first, so the width filter must read the tape width.
TAPE_FAMILIES = {"d-series", "p12", "a30"}
# The offline table is keyed by series string, matched by prefix.
LOCAL_SERIES = {
    "m110": "^M110",
    "m110s": "^M110",
    "m200": "^M200",
    "m220": "^M200",
    "m221": "^M200",
    "m250": "^M200",
    "m260": "^M200",
    "d-series": "^D30",
    # The 12mm tape printers share the D-series stock; the width filter trims it.
    "p12": "^(P12|D30)",
    "a30": "^D30",
}


def api_papers(sn: str, delay: float) -> list[dict]:
    headers = {
        "device": "android",
        "ua": "mb-printer-sdk-media-import",
        "versionCode": "5.21.1.2",
        "versionName": "Printmaster",
        "sn": sn,
        "lang": "en",
        "region": "FR",
        "User-Agent": "mb-printer-sdk media import",
    }
    url = urllib.parse.urljoin(BASE_URL, "api/d30/paper/queryAllPaperGroup/v2")
    url = f"{url}?{urllib.parse.urlencode({'sn': sn})}"
    time.sleep(delay)
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=30) as response:
        payload = json.load(response)
    out = []
    for group in payload.get("data") or []:
        # 0 is continuous stock, 3 black-mark, anything else gap sensing.
        shape = "continuous" if group.get("paperType") == 0 else "rectangle"
        for item in group.get("list") or []:
            out.append({"width": float(item["width"]), "height": float(item["height"]), "shape": shape})
    return out


def local_papers(table: list[dict], pattern: str) -> list[dict]:
    out = []
    for row in table:
        if not re.match(pattern, row.get("series", "")):
            continue
        # The offline rows carry the feed length in width and the media width in height.
        paper_type = row.get("paperType", "").lstrip(":")
        shape = "continuous" if paper_type == "0" else "rectangle"
        out.append({"width": float(row["width"]), "height": float(row["height"]), "shape": shape})
    return out


def preset(entry: dict, tape: bool) -> dict:
    width, height = entry["width"], entry["height"]
    trim = lambda value: int(value) if float(value).is_integer() else value
    built = {
        "id": f"{trim(width)}x{trim(height)}",
        "name": f"{trim(width)} × {trim(height)}mm" + (" continuous" if entry["shape"] == "continuous" else ""),
        "widthMm": width,
        "heightMm": 0.0 if entry["shape"] == "continuous" else height,
        "shape": entry["shape"],
    }
    if tape:
        built["tapeWidthMm"] = int(height)
    return built


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--devices", type=Path, required=True, help="DefaultPrinter.json from the vendor application")
    parser.add_argument("--local-paper", type=Path, required=True, help="localPaper.json from the vendor application")
    parser.add_argument("--offline", action="store_true", help="skip the paper API")
    parser.add_argument("--delay", type=float, default=0.4, help="seconds between API calls")
    parser.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "crates/mb-printer-core/data/phomemo-media.json",
    )
    arguments = parser.parse_args()

    devices = json.loads(arguments.devices.read_text())
    table = json.loads(arguments.local_paper.read_text())
    serials: dict[str, list[str]] = {}
    for device in devices:
        for printer_id, names in MODELS.items():
            if device.get("type") in names and device.get("sn"):
                serials.setdefault(printer_id, []).append(device["sn"])

    models, sources = {}, {}
    for printer_id in MODELS:
        entries, from_api = [], 0
        if not arguments.offline:
            for sn in serials.get(printer_id, []):
                found = api_papers(sn, arguments.delay)
                if found:
                    entries.extend(found)
                    from_api += 1
                    break  # one answering serial is the whole catalogue for the model
        if pattern := LOCAL_SERIES.get(printer_id):
            entries.extend(local_papers(table, pattern))
        seen, presets = set(), []
        for entry in entries:
            key = (entry["width"], entry["height"], entry["shape"])
            if key in seen:
                continue
            seen.add(key)
            presets.append(preset(entry, printer_id in TAPE_FAMILIES))
        presets.sort(key=lambda item: (item["shape"], -item["widthMm"], -item["heightMm"]))
        if presets:
            models[printer_id] = presets
            sources[printer_id] = {
                "paperApi": bool(from_api),
                "offlineTable": bool(LOCAL_SERIES.get(printer_id)),
                "serials": serials.get(printer_id, [])[:1],
            }

    document = {
        "_comment": "Generated by scripts/import_phomemo_paper.py from the vendor paper API and the application's offline table. Do not edit by hand.",
        "generatedOn": date.today().isoformat(),
        "sources": sources,
        "models": models,
    }
    arguments.out.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {arguments.out} ({sum(len(v) for v in models.values())} media across {len(models)} models)")


if __name__ == "__main__":
    main()
