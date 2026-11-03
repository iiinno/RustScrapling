#!/usr/bin/env python3
"""Record upstream CurlParser.parse results for fixtures/oracle/curl/cases.json.

  python -I oracle/curl_parser_oracle.py ../scrapling-v05-reference

Writes fixtures/oracle/curl/expected.json: per case either the parsed Request
(as a dict, proxy dict kept) or {"error": <exception type>, "message": ...}.
"""
import json
import logging
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, sys.argv[1])
logging.disable(logging.CRITICAL)
from scrapling.core.shell import CurlParser  # noqa: E402

cases = json.loads((ROOT / "fixtures/oracle/curl/cases.json").read_text())
parser = CurlParser()
out = []
for command in cases:
    try:
        request = parser.parse(command)
    except Exception as error:  # upstream raises AttributeError for unknown flags
        out.append({"error": type(error).__name__, "message": str(error)})
        continue
    if request is None:
        out.append({"error": "None", "message": "parse returned None"})
        continue
    value = request._asdict()
    if isinstance(value["data"], bytes):
        value["data"] = value["data"].decode()
    out.append(value)
(ROOT / "fixtures/oracle/curl/expected.json").write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
print(f"{len(out)} cases")
