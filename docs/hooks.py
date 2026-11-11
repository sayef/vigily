"""Includes repository files (CHANGELOG.md, DESIGN.md and others) in the site with working links.

A line `--8<-- "PATH"` on its own is replaced by that file, without its YAML front matter. Relative
links in it are resolved against the file's own directory: targets inside docs/ become site links,
anything else a link to the file on GitHub.
"""

import os
import re
from pathlib import Path

REPO = "https://github.com/sayef/vigily/blob/main/"
INCLUDE = re.compile(r'^--8<-- "([^"]+)"\s*$', re.M)
LINK = re.compile(r"(\]\()([^)\s#]+)(#[^)\s]*)?(\))")
FRONT_MATTER = re.compile(r"\A---\n.*?\n---\n", re.S)


def _rewrite(text, source, page_dir, root):
    def target(match):
        href, anchor = match.group(2), match.group(3) or ""
        if re.match(r"[a-z]+:", href) or href.startswith("/"):
            return match.group(0)
        path = (source.parent / href).resolve()
        rel = path.relative_to(root).as_posix()
        if rel.startswith("docs/"):
            href = os.path.relpath(root / rel, page_dir).replace(os.sep, "/")
        else:
            href = REPO + rel
        return f"{match.group(1)}{href}{anchor}{match.group(4)}"

    return LINK.sub(target, text)


def on_page_markdown(markdown, page, config, **kwargs):
    root = Path(config.config_file_path).parent.resolve()
    page_dir = (root / "docs" / page.file.src_path).parent

    def include(match):
        source = root / match.group(1)
        text = FRONT_MATTER.sub("", source.read_text())
        return _rewrite(text, source, page_dir, root)

    markdown = INCLUDE.sub(include, markdown)
    # The page's Markdown, includes inlined, for the Copy page button (overrides/partials/actions.html).
    page.vg_source = markdown.strip()
    return markdown
