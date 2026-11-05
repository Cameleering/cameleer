# Sphinx configuration for the Read the Docs build of the Cameleer documentation.
#
# The pages are the same Markdown files that GitHub shows and that scripts/build-docs.py turns
# into the project website; MyST reads them unchanged. Local build:
#   pip install -r docs/requirements.txt
#   sphinx-build -b html docs docs/_build/html

import os
import re

project = "Cameleer"
author = "Sudheesh PM and contributors"
copyright = "2026, the Cameleer authors. Apache License 2.0"


def _version():
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "..", "Cargo.toml"), encoding="utf-8") as f:
        m = re.search(r'^version\s*=\s*"([^"]+)"', f.read(), re.M)
    return m.group(1) if m else "dev"


release = _version()
version = ".".join(release.split(".")[:2])

extensions = ["myst_parser", "sphinx_copybutton"]
source_suffix = {".md": "markdown"}
root_doc = "index"
exclude_patterns = ["_build", "README.md", "requirements.txt", "conf.py"]

# MyST: GitHub-style anchors for "#section" links, and `{{env:...}}` placeholders in route
# examples must not be read as substitutions
myst_heading_anchors = 4
myst_enable_extensions = ["colon_fence", "deflist", "fieldlist"]
myst_substitutions = {}
suppress_warnings = ["myst.header", "myst.xref_missing", "myst.substitution"]

# links that leave docs/ (README, examples/, crates/) point to GitHub
github_url = "https://github.com/pmsudhi/cameleer"

html_theme = "furo"
html_title = f"Cameleer {release}"
html_baseurl = os.environ.get("READTHEDOCS_CANONICAL_URL", "https://cameleer.readthedocs.io/en/latest/")
html_theme_options = {
    "source_repository": "https://github.com/pmsudhi/cameleer/",
    "source_branch": "main",
    "source_directory": "docs/",
}
html_static_path = []
html_show_sourcelink = True

# one language for code blocks that have no lexer (text, sh)
highlight_language = "text"
