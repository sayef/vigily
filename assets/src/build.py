"""Brand assets for vigily, with every word as outlines.

    (cd crates/vigily-ui && npm ci)    # provides the fonts
    uv run --no-project --with fonttools --with brotli --with resvg-py python assets/src/build.py

The font is Recursive (SIL Open Font License), read from the UI's fontsource package so the
assets and the app use the same file: its MONO and weight axes, Linear (CASL 0) and upright. Writes to assets/ and copies what
the docs site and the web app use. Run it again after changing anything here; the output is
deterministic.

The identity (brand/DESIGN.md, "Mark"):
- the mark: a pair of binoculars, "keeping watch": an ink body with ink-blue glass and a white glint
  in light, a light-blue body with deep glass in dark. A heavier drawing without the glint for 16 px;
- the wordmark: the binoculars beside `vigily` in Recursive Sans Linear 650, plain letters.
"""

import shutil
import sys
from pathlib import Path

import resvg_py
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.recordingPen import DecomposingRecordingPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parents[2]
FONTS = ROOT / "crates/vigily-ui/node_modules/@fontsource-variable"
OUT = ROOT / "assets"


# The fontsource subset with the MONO and wght axes; CASL is 0 (Linear) and slnt 0 in it.
FONT_FILE = "recursive-latin-mono-normal.woff2"


def instance(weight, mono):
    path = FONTS / "recursive" / "files" / FONT_FILE
    if not path.exists():
        sys.exit(f"missing {path}: run `npm ci` in crates/vigily-ui first")
    font = instantiateVariableFont(TTFont(path), {"wght": weight, "MONO": mono})
    # Recursive swaps glyphs between its Mono and Sans forms with required feature variations
    # (`rvrn`), which a browser applies; drawing from the cmap alone would show the Mono `l` and `0`
    # in the Sans. Fold them into the cmap.
    gsub = font["GSUB"].table
    swaps = {}
    for record in gsub.FeatureList.FeatureRecord:
        if record.FeatureTag != "rvrn":
            continue
        for index in record.Feature.LookupListIndex:
            for sub in gsub.LookupList.Lookup[index].SubTable:
                sub = getattr(sub, "ExtSubTable", sub)
                swaps.update(getattr(sub, "mapping", None) or {})
    for table in font["cmap"].tables:
        table.cmap = {c: swaps.get(g, g) for c, g in table.cmap.items()}
    return font


WORD_WEIGHT = 650
WORD = instance(WORD_WEIGHT, 0)
TEXT = instance(450, 0)
MONO = instance(450, 1)

# Tokens from brand/DESIGN.md: warm paper and ink blue. `body`, `glass` and `glint` colour the mark.
THEMES = {
    "light": {
        "ground": "#F6F5F1",
        "ink": "#151719",
        "meta": "#55595F",
        "accent": "#1F4E8C",
        "series": "#8E8C85",
        "body": "#151719",
        "glass": "#1F4E8C",
        "glint": "#FFFFFF",
    },
    "dark": {
        "ground": "#161513",
        "ink": "#EAE7E0",
        "meta": "#ADA89E",
        "accent": "#8DB3EC",
        "series": "#6E695F",
        "body": "#8DB3EC",
        "glass": "#14304F",
        "glint": "#EAE7E0",
    },
}
TAGLINE = "Keeps watch over your runs, evals and drift"
TRACKING = -0.01
# At size 80: the `v` ink of the original lockup began 3.27 units further right than Recursive's does.
V_GAP_SHIFT = 3.27

# ---------------------------------------------------------------------------------------------------
# The icon


def binoculars(theme, small=False):
    """The binoculars on a 64 grid, centred on y = 32 and spanning x 6 to 58: two barrels joined by a
    bridge, a hinge and a focus wheel, each lens with a glint. The 16 px drawing has smaller glass
    (heavier rims) and no glint, so the lenses stay open at that size."""
    t = THEMES[theme]
    body, glass, glint = t["body"], t["glass"], t["glint"]
    out = [
        f'<rect x="18.5" y="28.25" width="27" height="7.5" rx="3.75" fill="{body}"/>',
        f'<rect x="28" y="23.25" width="8" height="17.5" rx="3.75" fill="{body}"/>',
        f'<rect x="25.125" y="19.25" width="13.75" height="7.5" rx="2.625" fill="{body}"/>',
    ]
    for cx in (18.5, 45.5):
        out.append(f'<circle cx="{cx}" cy="32" r="12.5" fill="{body}"/>')
        out.append(f'<circle cx="{cx}" cy="32" r="{6 if small else 7.5}" fill="{glass}"/>')
        if not small:
            out.append(
                f'<path d="M{cx - 4.59:.2f} 30.77A4.75 4.75 0 0 1 {cx - 0.41:.2f} 27.27" fill="none" '
                f'stroke="{glint}" stroke-width="1.62" stroke-linecap="round"/>'
            )
    return "".join(out)


def icon_body(theme, small=False, radius=14):
    """The app tile: the binoculars on a paper tile, two units below centre like the source drawing."""
    # At 16 px the drawing fills the tile edge to edge so the lenses keep a few pixels each.
    place = "translate(-4.4 -4.8) scale(1.15)" if small else "translate(0 2)"
    return f'<rect width="64" height="64" rx="{radius}" fill="{THEMES[theme]["ground"]}"/><g transform="{place}">{binoculars(theme, small)}</g>'


def icon(theme, x=0.0, y=0.0, size=64.0, small=None):
    small = size <= 20 if small is None else small
    return f'<g transform="translate({x:.2f} {y:.2f}) scale({size / 64:.4f})">{icon_body(theme, small)}</g>'


# ---------------------------------------------------------------------------------------------------
# The wordmark

GLYPHS = WORD.getGlyphSet()
CMAP = WORD.getBestCmap()
UPM = WORD["head"].unitsPerEm


def _contours(ch):
    pen = DecomposingRecordingPen(GLYPHS)
    GLYPHS[CMAP[ord(ch)]].draw(pen)
    out, cur = [], []
    for op, args in pen.value:
        cur.append((op, args))
        if op in ("closePath", "endPath"):
            out.append(cur)
            cur = []
    return out


def _bounds(contour):
    pen = BoundsPen(GLYPHS)
    for op, args in contour:
        getattr(pen, op)(*args)
    return pen.bounds


def text_path(font, text, x, baseline, size, tracking=0.0):
    """SVG path data for `text` and its advance width, in user units."""
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = size / font["head"].unitsPerEm
    pen = SVGPathPen(glyphs, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
    cursor = x
    for ch in text:
        name = cmap[ord(ch)]
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, cursor, baseline)))
        cursor += glyphs[name].width * scale + tracking * size
    return pen.getCommands(), cursor - x - tracking * size


def wordmark(theme, x, baseline, size):
    """The plain letters `vigily`; returns the SVG body and the advance width."""
    word, width = text_path(WORD, "vigily", x, baseline, size, tracking=TRACKING)
    return f'<path d="{word}" fill="{THEMES[theme]["ink"]}"/>', width


def lockup(x, baseline, size, theme):
    """The binoculars, then the letters; `size` is the font size. As in the reference drawing at
    80 px: the binoculars at 1.5 times the 64 grid, from the x-height to the baseline, 9 units before the v.
    `x` is the binoculars' left edge. Returns the SVG body and its width."""
    k = size / 80
    gx, gy = x - 9 * k, baseline - 68 * k
    # Recursive's `v` has a smaller side bearing than the drawing assumed: move the letters right so
    # the gap between the binoculars and the `v` stays as drawn.
    shift = V_GAP_SHIFT * k
    body, w = wordmark(theme, gx + 95.06 * k + shift, baseline, size)
    marks = f'<g transform="translate({gx:.2f} {gy:.2f}) scale({1.5 * k:.4f})">{binoculars(theme)}</g>'
    return marks + body, 95.06 * k + shift - 9 * k + w


# ---------------------------------------------------------------------------------------------------


def svg(width, height, body, label):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:g} {height:g}" width="{width:g}" '
        f'height="{height:g}" role="img" aria-label="{label}">\n{body}\n</svg>\n'
    )


def write(name, text):
    (OUT / name).write_text(text)


def png(svg_text, width, height):
    return bytes(resvg_py.svg_to_bytes(svg_string=svg_text, width=width, height=height))


def loss_lines(width, top, height, theme):
    """Faint loss curves with one run in accent: the subject, kept quiet behind the words."""
    t = THEMES[theme]
    out = []
    for i, (start, end, wobble) in enumerate([(0.05, 0.62, 0.05), (0.0, 0.48, 0.04), (0.12, 0.80, 0.06)]):
        pts = []
        for k in range(41):
            u = k / 40
            v = start + (end - start) * (1 - (1 - u) ** 4) + wobble * ((k * 7 + i * 3) % 5 - 2) / 4
            pts.append(f"{u * width:.1f},{top + v * height:.1f}")
        color, op = (t["accent"], 1) if i == 0 else (t["series"], 0.55)
        out.append(
            f'<polyline points="{" ".join(pts)}" fill="none" stroke="{color}" stroke-opacity="{op}" '
            'stroke-width="2" stroke-linejoin="round"/>'
        )
    return "".join(out)


def explorations():
    """The icon at each size with the drawing made for it, and the wordmark at header sizes."""
    rows = []
    for r, theme in enumerate(("light", "dark")):
        t = THEMES[theme]
        y0 = r * 150
        rows.append(f'<rect y="{y0}" width="900" height="150" fill="{t["ground"]}"/>')
        x = 24
        for size in (16, 24, 32, 48, 64, 96):
            rows.append(icon(theme, x, y0 + 116 - size, size))
            x += size + 18
        for h in (24, 32, 48):
            body, w = wordmark(theme, x, y0 + 116, h)
            rows.append(body)
            x += w + 24
        label, _ = text_path(TEXT, "16, 24, 32, 48, 64, 96 px; wordmark at 24, 32, 48 px", 24, y0 + 140, 12)
        rows.append(f'<path d="{label}" fill="{t["meta"]}"/>')
    return svg(900, 300, "".join(rows), "vigily icon sizes and wordmark")


def ui_paths():
    """The letters' outlines for the web app, which draws them and the binoculars with theme colours."""
    size, base = 100, 100
    word, width = text_path(WORD, "vigily", 0, base, size, tracking=TRACKING)
    return (
        f"// Generated by assets/src/build.py from Recursive Sans Linear {WORD_WEIGHT}. Do not edit.\n"
        f'export const WORDMARK_VIEWBOX = "0 {base - 82} {width:.2f} 104";\n'
        f'export const WORDMARK_WORD = "{word}";\n'
    )


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for theme, suffix in (("light", ""), ("dark", "-dark")):
        t = THEMES[theme]
        write(f"logo{suffix}.svg", svg(64, 64, icon(theme), "vigily"))
        body, w = lockup(4, 84, 80, theme)
        write(f"wordmark{suffix}.svg", svg(round(w + 8), 104, body, "vigily"))

        tag, tag_w = text_path(TEXT, TAGLINE, 0, 0, 26)
        _, lw = lockup(0, 0, 96, theme)
        width = round(max(lw, tag_w) + 96)
        body, _ = lockup((width - lw) / 2, 118, 96, theme)
        tag, _ = text_path(TEXT, TAGLINE, (width - tag_w) / 2, 176, 26)
        panel = f'<rect width="{width}" height="208" rx="16" fill="{t["ground"]}"/>'
        write(
            f"banner-{theme}.svg",
            svg(
                width,
                208,
                f'{panel}\n{body}\n<path d="{tag}" fill="{t["meta"]}"/>',
                f"vigily: {TAGLINE.lower()}",
            ),
        )
        # A PNG copy for the README: GitHub's mobile app does not render SVG images.
        (OUT / f"banner-{theme}.png").write_bytes(
            png((OUT / f"banner-{theme}.svg").read_text(), width * 2, 416)
        )

    # App tile and favicon: the light version (paper tile, ink binoculars) reads on light and dark
    # browser chrome alike. The favicon is the 16 px drawing; the tile is the full one.
    write("icon.svg", svg(256, 256, icon("light", 0, 0, 256), "vigily"))
    write("favicon.svg", svg(64, 64, icon("light", 0, 0, 64, small=True), "vigily"))
    # iOS rounds the corners itself and fills transparent ones black: a full-bleed square.
    square = svg(64, 64, icon_body("light", radius=0), "vigily")
    (OUT / "apple-touch-icon.png").write_bytes(png(square, 180, 180))

    _, lw = lockup(0, 0, 128, "dark")
    body, _ = lockup((1280 - lw) / 2, 300, 128, "dark")
    tag, tag_w = text_path(TEXT, TAGLINE, 0, 0, 36)
    tag, _ = text_path(TEXT, TAGLINE, (1280 - tag_w) / 2, 380, 36)
    cmd, cmd_w = text_path(MONO, "vigily up", 0, 0, 26)
    cmd, _ = text_path(MONO, "vigily up", (1280 - cmd_w) / 2, 446, 26)
    d = THEMES["dark"]
    social = (
        f'<rect width="1280" height="640" fill="{d["ground"]}"/>'
        f'<g opacity=".5">{loss_lines(1280, 470, 150, "dark")}</g>'
        f'{body}<path d="{tag}" fill="{d["meta"]}"/><path d="{cmd}" fill="{d["accent"]}"/>'
    )
    write("social-preview.svg", svg(1280, 640, social, f"vigily: {TAGLINE.lower()}"))
    (OUT / "social-preview.png").write_bytes(png((OUT / "social-preview.svg").read_text(), 1280, 640))

    (OUT / "src/explorations.svg").write_text(explorations())

    docs = ROOT / "docs/assets"
    docs.mkdir(parents=True, exist_ok=True)
    for name in (
        "logo.svg",
        "logo-dark.svg",
        "icon.svg",
        "favicon.svg",
        "wordmark.svg",
        "wordmark-dark.svg",
        "banner-light.svg",
        "banner-dark.svg",
        "social-preview.png",
    ):
        shutil.copy(OUT / name, docs / name)
    shutil.copy(OUT / "src/explorations.svg", docs / "explorations.svg")

    ui = ROOT / "crates/vigily-ui"
    shutil.copy(OUT / "favicon.svg", ui / "public/icon.svg")
    shutil.copy(OUT / "apple-touch-icon.png", ui / "public/apple-touch-icon.png")
    (ui / "src/components/wordmark-paths.ts").write_text(ui_paths())

    # The docs site serves the same font files as the app (SIL Open Font License).
    (docs / "fonts").mkdir(exist_ok=True)
    for old in (docs / "fonts").glob("atkinson-*"):
        old.unlink()
    for file in (FONT_FILE, FONT_FILE.replace("latin-", "latin-ext-")):
        shutil.copy(FONTS / "recursive" / "files" / file, docs / "fonts")
    shutil.copy(FONTS / "recursive" / "LICENSE", docs / "fonts" / "recursive-OFL.txt")


if __name__ == "__main__":
    main()
