"""Copy the screenshots the README and the docs use from a QA run, as small palette PNG files.

    vigily up --config examples/vigily.toml          # on the demo repository
    (cd crates/vigily-ui && npm run qa)                # every screen, both themes -> target/qa
    uv run --no-project --with pillow python assets/src/screenshots.py

Only the files in USED are kept in the repository; the full QA set stays in target/qa.
"""

import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
SRC = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/qa"
DST = ROOT / "docs/assets/screenshots"

USED = [
    "runs-desktop-light.png",
    "runs-desktop-dark.png",
    "metrics-desktop-light.png",
    "metrics-aggregate-desktop-dark.png",
    "run-desktop-dark.png",
    "palette-desktop-light.png",
    "runs-phone-light.png",
    "metrics-phone-dark.png",
    "suite-desktop-light.png",
    "suite-desktop-dark.png",
    "eval-items-desktop-light.png",
    "eval-items-desktop-dark.png",
    "compare-desktop-dark.png",
    "drift-desktop-light.png",
    "drift-desktop-dark.png",
    "schedule-desktop-light.png",
    "alerts-desktop-dark.png",
]


def main():
    DST.mkdir(parents=True, exist_ok=True)
    for old in DST.glob("*.png"):
        if old.name not in USED:
            old.unlink()
    total = 0
    for name in USED:
        image = Image.open(SRC / name).convert("RGB")
        # UI screenshots use few colours; 256 keeps text and anti-aliasing clean at a third of the size.
        image.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(
            DST / name, optimize=True
        )
        total += (DST / name).stat().st_size
    print(f"{len(USED)} screenshots, {total / 1024:.0f} KiB in {DST.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
