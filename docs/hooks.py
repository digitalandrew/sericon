"""Publish canonical source examples without duplicating editable files."""
from pathlib import Path

from mkdocs.structure.files import File

ROOT = Path(__file__).resolve().parents[1]
ASSETS = {
    "../scripts/install.sh": "install.sh",
    "../LICENSE": "assets/licenses/SERICON-LICENSE",
    "../examples/config.toml": "assets/examples/config.toml",
    "../formulas/examples/tplink-uboot-interrupt.rhai": "assets/examples/tplink-uboot-interrupt.rhai",
    "../helper/MUSL-COPYRIGHT": "assets/licenses/MUSL-COPYRIGHT",
    "../vendor/vt100/LICENSE": "assets/licenses/VT100-LICENSE",
}


def on_files(files, config):
    for source, destination in ASSETS.items():
        files.append(File.generated(config, destination, abs_src_path=str(ROOT / source[3:])))
    return files


def on_page_markdown(markdown, **kwargs):
    for source, destination in ASSETS.items():
        markdown = markdown.replace(f"]({source})", f"]({destination})")
    if '<!-- formula-api -->' in markdown:
        reference = (ROOT / 'docs/formula-api.txt').read_text()
        markdown = markdown.replace('<!-- formula-api -->', f'## Runtime reference\n\n```text\n{reference}\n```')
    return markdown
