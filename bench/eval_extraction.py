#!/usr/bin/env python3
"""Plan G-07: main-content extraction quality versus upstream on fixtures/extraction.

Run with the upstream venv's Python (needs lxml and the pinned checkout):

  <venv>/bin/python -I bench/eval_extraction.py target/release/rustscrapling ../scrapling-v05-reference

For every page:
  gold      = tokens of the element marked data-gold="1" (authored article body)
  eval set  = gold tokens that upstream `Convertor` (main_content_only=True,
              text) retains -- Plan G-07's "evaluation words"
  candidates: rust-convertor (RustScrapling's port: `shell` `text`) and
              rust-extract (RustScrapling's own extractor: `md --format text`)
Reported per candidate: retention of the eval set, recall of gold, output
characters versus upstream, and leaks of hidden/script text. Tokenizer
(identical for every side): NFKC, lower-case, [a-z0-9]+ words plus CJK
character bigrams.
"""
import json
import pathlib
import re
import subprocess
import sys
import unicodedata

ROOT = pathlib.Path(__file__).resolve().parents[1]
CORPUS = ROOT / "fixtures" / "extraction"
# Hidden (display:none / [hidden]) injection text and script/style text must
# never appear. The aria-hidden ad is visible on screen, so it is boilerplate
# (reported separately), not a leak.
LEAKS = ["HIDDEN-INJECTION-7F3A", "SCRIPT-TEXT-55D0", "UA-0000", "tracking pixel", ".ad{"]
BOILERPLATE = ["VISIBLE-AD-91C2", "Copyright 2026 Example Media", "We use cookies", "Ten tips for gardening"]
CJK = re.compile(r"[぀-ヿ㐀-鿿豈-﫿]+")


def tokens(text: str) -> set[str]:
    text = unicodedata.normalize("NFKC", text).lower()
    out = set(re.findall(r"[a-z0-9]+", text))
    for run in CJK.findall(text):
        out.update(run[i:i + 2] for i in range(max(1, len(run) - 1)))
    return out


def main() -> int:
    binary, upstream = sys.argv[1], sys.argv[2]
    sys.path.insert(0, upstream)
    import logging
    logging.disable(logging.CRITICAL)
    from lxml import html as lxml_html
    from scrapling.core.shell import Convertor
    from scrapling.parser import Selector

    rows, ok = [], True
    for path in sorted(CORPUS.glob("*.html")):
        source = path.read_text(encoding="utf-8")
        gold_text = " ".join(lxml_html.fromstring(source).xpath('//*[@data-gold="1"]')[0].itertext())
        gold = tokens(gold_text)
        upstream_text = "\n".join(Convertor._extract_content(Selector(source), "text", main_content_only=True))
        eval_set = gold & tokens(upstream_text)
        candidates = {
            "upstream": upstream_text,
            # First stdout line is the `load` page summary, not content.
            "rust-convertor": subprocess.run([binary, "shell", "-c", f"load {path}\ntext"],
                                             capture_output=True, text=True, check=True
                                             ).stdout.split("\n", 1)[1],
            "rust-extract": subprocess.run([binary, "md", str(path), "--format", "text"],
                                           capture_output=True, text=True, check=True).stdout,
        }
        row = {"page": path.name, "gold_tokens": len(gold), "eval_tokens": len(eval_set)}
        for name, text in candidates.items():
            found = tokens(text)
            leaks = [marker for marker in LEAKS if marker.lower() in text.lower()]
            boilerplate = [marker for marker in BOILERPLATE if marker.lower() in text.lower()]
            row[name] = {
                "chars": len(text.strip()),
                "eval_retention": round(len(eval_set & found) / len(eval_set), 4) if eval_set else 1.0,
                "gold_recall": round(len(gold & found) / len(gold), 4) if gold else 1.0,
                "leaks": leaks,
                "boilerplate": boilerplate,
            }
            if name != "upstream":
                row[name]["chars_le_upstream"] = len(text.strip()) <= len(upstream_text.strip())
                if row[name]["eval_retention"] < 0.95 or leaks:
                    ok = False
        rows.append(row)
    summary = {}
    for name in ("upstream", "rust-convertor", "rust-extract"):
        summary[name] = {
            "min_eval_retention": min(r[name]["eval_retention"] for r in rows),
            "mean_gold_recall": round(sum(r[name]["gold_recall"] for r in rows) / len(rows), 4),
            "total_chars": sum(r[name]["chars"] for r in rows),
            "pages_with_leaks": sum(bool(r[name]["leaks"]) for r in rows),
            "pages_with_boilerplate": sum(bool(r[name]["boilerplate"]) for r in rows),
        }
        if name != "upstream":
            summary[name]["pages_chars_le_upstream"] = sum(r[name]["chars_le_upstream"] for r in rows)
    print(json.dumps({"pages": rows, "summary": summary}, indent=1, ensure_ascii=False))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
