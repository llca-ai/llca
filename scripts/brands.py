#!/usr/bin/env python3
"""Write static/brands.js: the logos named by `icon` rules in rules/services.rules.

Usage:  python3 scripts/brands.py PATH/TO/simple-icons

PATH is a checkout of https://github.com/simple-icons/simple-icons (its
`icons/` and `data/` folders are enough):

    git clone --depth 1 https://github.com/simple-icons/simple-icons.git

Only logos without a licence of their own (the project's CC0) are copied; the
others are refused, so that every service logo in this repository is CC0
(Codex's own logo, kept in OWN_AGENT_LOGOS, is the exception). The logos
are trademarks of their owners and are shown only to identify each service.
Standard library only, like the rest of the project.
"""

import json
import re
import subprocess
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RULES = ROOT / "rules" / "services.rules"
OUT = ROOT / "static" / "brands.js"
# The agents' own logos, for the dashboard's side menu: agent id → slug.
AGENTS = {"claude-code": "claudecode"}
# OpenAI's logos (Codex's included) are not in Simple Icons: Codex's is kept
# here, traced from OpenAI's artwork. Not CC0: a trademark of OpenAI, shown only
# to identify Codex. Agent id → [colour, SVG path in a 24×24 box].
OWN_AGENT_LOGOS = {"codex": ["1F2326", "M12.38 14.56L12.6 14.29L12.91 14.11L13.13 14.07L16.91 14.07L17.4 14.24L17.62 14.47L17.8 14.87L17.8 15.22L17.58 15.67L17.31 15.89L17.09 15.98L12.96 15.98L12.56 15.76L12.38 15.53L12.24 15.18L12.24 14.91ZM8.07 8.07L8.33 8.2L8.47 8.33L10.38 11.49L10.51 11.8L10.51 12.2L10.33 12.6L10.02 13.04L8.64 15.4L8.29 15.8L7.93 15.93L7.62 15.93L7.27 15.8L6.96 15.49L6.82 15.09L6.82 14.87L6.91 14.56L8.47 12.02L6.91 9.44L6.82 9.22L6.82 8.69L6.96 8.42L7.18 8.2L7.58 8.02ZM9.98 0.07L10.02 0.02L11.04 0.02L11.09 0.07L11.44 0.07L12.29 0.24L12.96 0.47L13.71 0.82L14.47 1.31L14.78 1.58L15.4 1.53L15.44 1.49L16.42 1.49L16.47 1.53L16.87 1.53L17.71 1.71L18.6 2.02L19.22 2.33L20.07 2.91L20.87 3.67L21.36 4.29L21.67 4.78L22.07 5.62L22.42 6.82L22.47 7.44L22.51 7.49L22.51 8.6L22.47 8.64L22.42 9.22L22.78 9.67L23.22 10.38L23.76 11.71L23.93 12.6L23.93 12.91L23.98 12.96L23.98 14.02L23.93 14.07L23.89 14.64L23.67 15.53L23.44 16.11L23.18 16.64L22.69 17.4L21.89 18.29L21.36 18.73L20.51 19.27L19.62 19.67L19.22 20.6L18.87 21.18L18.38 21.8L17.84 22.33L17.22 22.82L16.64 23.18L15.4 23.71L14.02 23.98L12.91 23.98L12.87 23.93L12.51 23.93L12.47 23.89L12.02 23.84L11.13 23.58L10.2 23.13L9.22 22.42L8.64 22.47L8.6 22.51L7.53 22.51L7.49 22.47L7.13 22.47L6.29 22.29L5.4 21.98L4.78 21.67L3.93 21.09L3.18 20.38L2.78 19.89L2.29 19.13L1.98 18.47L1.67 17.53L1.53 16.87L1.49 15.53L1.53 15.49L1.58 14.78L0.87 13.8L0.51 13.09L0.24 12.33L0.02 11.18L0.02 9.98L0.07 9.93L0.11 9.31L0.2 9.09L0.2 8.91L0.6 7.8L1.22 6.73L1.8 6.02L2.6 5.31L3.49 4.73L4.07 4.47L4.29 4.42L4.38 4.33L4.87 3.27L5.18 2.78L5.76 2.07L6.38 1.49L7.13 0.96L7.8 0.6L9.13 0.16ZM8.73 2.24L8.2 2.51L7.58 2.96L6.91 3.62L6.51 4.2L6.11 5.04L6.02 5.44L5.71 5.89L5.44 6.02L4.82 6.2L4.11 6.56L3.36 7.13L2.87 7.67L2.51 8.2L2.24 8.73L1.98 9.62L1.93 10.24L1.89 10.29L1.89 10.78L1.93 10.82L1.93 11.22L2.02 11.67L2.2 12.24L2.56 12.96L3 13.58L3.36 13.93L3.53 14.29L3.53 14.69L3.36 15.4L3.36 16.56L3.62 17.62L4.16 18.64L4.6 19.18L5.09 19.62L5.67 20.02L6.2 20.29L7.27 20.6L8.38 20.64L8.42 20.6L8.96 20.56L9.36 20.42L9.76 20.47L10.11 20.69L10.82 21.31L11.58 21.71L12.07 21.89L12.96 22.07L13.84 22.07L13.89 22.02L14.42 21.98L15.04 21.8L15.71 21.49L16.47 20.96L16.96 20.47L17.49 19.71L17.84 18.91L17.89 18.64L18.02 18.33L18.29 18.07L19.22 17.76L20.2 17.18L21.09 16.29L21.4 15.84L21.71 15.22L21.98 14.38L22.07 13.27L22.02 13.22L22.02 12.78L21.93 12.33L21.71 11.67L21.4 11.04L20.91 10.38L20.56 10.02L20.42 9.76L20.42 9.22L20.51 9L20.56 8.47L20.6 8.42L20.6 7.58L20.56 7.53L20.51 7L20.29 6.29L19.76 5.31L18.87 4.38L18.29 3.98L17.76 3.71L16.73 3.4L15.44 3.36L14.73 3.53L14.24 3.53L13.89 3.36L13.49 2.96L12.87 2.51L12.33 2.24L11.67 2.02L11.22 1.98L11.18 1.93L10.56 1.93L10.51 1.89L9.62 1.98Z"]}


def slug_of(title):
    """Simple Icons' rule for a slug, used when an entry has no `slug`."""
    for a, b in (("+", "plus"), (".", "dot"), ("&", "and"), ("đ", "d"), ("ħ", "h"), ("ı", "i"), ("ĸ", "k"), ("ŀ", "l"), ("ł", "l"), ("ß", "ss"), ("ŧ", "t")):
        title = title.replace(a, b)
    title = unicodedata.normalize("NFD", title)
    return re.sub(r"[^a-z0-9]", "", title.lower())


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    source = Path(sys.argv[1])
    data = json.loads((source / "data" / "simple-icons.json").read_text())
    entries = data if isinstance(data, list) else data["icons"]
    by_slug = {e.get("slug") or slug_of(e["title"]): e for e in entries}

    wanted = []
    for n, line in enumerate(RULES.read_text().splitlines(), 1):
        line = line.split(" #")[0].strip()
        if line.startswith("icon "):
            _, slug, name = line.split(None, 2)
            wanted.append((n, slug, name.strip()))

    problems = []

    def logo(where, slug):
        """[colour, path] for a CC0 logo, or None after noting the problem."""
        entry = by_slug.get(slug)
        svg = source / "icons" / f"{slug}.svg"
        if entry is None or not svg.exists():
            problems.append(f"{where}: no logo named {slug!r} in Simple Icons")
            return None
        licence = (entry.get("license") or {}).get("type", "CC0-1.0")
        if licence != "CC0-1.0":
            problems.append(f"{where}: {slug} is under {licence}, not CC0: remove it")
            return None
        paths = re.findall(r'<path d="([^"]+)"', svg.read_text())
        if len(paths) != 1 or not re.fullmatch(r"[0-9A-Fa-f]{6}", entry["hex"]):
            problems.append(f"{where}: {slug} has an unexpected format")
            return None
        return [entry["hex"].upper(), paths[0]]

    logos = {name: value for n, slug, name in wanted if (value := logo(f"line {n}", slug))}
    agents = {**OWN_AGENT_LOGOS, **{agent: value for agent, slug in AGENTS.items() if (value := logo(f"agent {agent}", slug))}}
    if problems:
        sys.exit("\n".join(problems))

    try:
        commit = subprocess.run(["git", "-C", str(source), "log", "-1", "--format=%h %cs"], capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        commit = "unknown version"
    body = ",\n".join(f"  {json.dumps(name)}: {json.dumps(value)}" for name, value in sorted(logos.items(), key=lambda kv: kv[0].lower()))
    OUT.write_text(
        "// Generated by scripts/brands.py from Simple Icons (" + commit + "), CC0,\n"
        "// except the Codex logo (OpenAI's, not CC0: see OWN_AGENT_LOGOS).\n"
        "// Do not edit: add an `icon` rule to rules/services.rules and run the script.\n"
        "// The logos are trademarks of their owners, shown only to identify each service.\n"
        "// Service name → [brand colour, SVG path in a 24×24 box].\n"
        "const BRAND_LOGOS = {\n" + body + "\n};\n"
        "// Agent id → [brand colour, SVG path]: the logos of the side menu.\n"
        "const AGENT_LOGOS = " + json.dumps(agents) + ";\n"
    )
    print(f"{len(logos) + len(agents)} logos written to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
