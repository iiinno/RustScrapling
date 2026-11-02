"""Generate Python round(x, 2) cases independently of Scrapling.

The source numbers and expected results are serialized with repr(), so
the Rust test parses the same IEEE 754 input and compares exact f64 values.
"""
import json
import math
import os
import random


def main():
    cases = []
    # Decimal xx.x05 ties and their immediately adjacent binary floats.
    # They reveal differences between mathematical decimal rounding and
    # Python's actual round-to-even on a binary floating-point number.
    for n in range(-1000, 1001):
        value = n / 200.0
        cases.extend((value, math.nextafter(value, -math.inf), math.nextafter(value, math.inf)))

    rng = random.Random(20261102)
    for _ in range(2000):
        cases.append(rng.uniform(-100.0, 100.0))

    path = os.path.join(os.path.dirname(__file__), "..", "fixtures", "stage2_round.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump([[repr(x), repr(round(x, 2))] for x in cases], f, separators=(",", ":"))
    print(f"wrote {len(cases)} round cases to {path}")


if __name__ == "__main__":
    main()
