#!/usr/bin/env python3
"""Check repository documentation links and the built site's local resources."""
from html.parser import HTMLParser
import hashlib
import json
from pathlib import Path
from urllib.parse import unquote, urlsplit

import markdown
import yaml

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / '.local/docs-site'


class Links(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.links, self.ids = [], set()
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'id' in attrs:
            self.ids.add(attrs['id'])
        for key in ('href', 'src'):
            if key in attrs:
                self.links.append(attrs[key])
        if tag == 'img':
            assert attrs.get('alt'), f'Image lacks alt text: {attrs}'


def parsed(path):
    text = path.read_text()
    if path.suffix == '.md':
        text = markdown.markdown(text, extensions=['tables', 'fenced_code', 'toc'])
    return Links(text)


def check(path, site=False):
    document = parsed(path)
    for link in document.links:
        url = urlsplit(link)
        if url.scheme or url.netloc:
            continue
        target = path if not url.path else path.parent / unquote(url.path)
        if url.path.startswith('/'):
            target = (SITE if site else ROOT) / unquote(url.path.lstrip('/'))
        target = target.resolve()
        if site:
            assert target.is_relative_to(SITE), (path, 'link leaves built site', link)
        if target.is_dir():
            target /= 'index.html' if site else 'README.md'
        assert target.is_file(), (path, 'missing resource', link)
        if url.fragment and target.suffix in ('.md', '.html'):
            assert unquote(url.fragment) in parsed(target).ids, (path, 'missing anchor', link)


def pages(nav):
    for row in nav:
        if isinstance(row, str):
            yield row
        else:
            for value in row.values():
                if isinstance(value, list):
                    yield from pages(value)
                else:
                    yield value


def main():
    config = yaml.safe_load((ROOT / 'mkdocs.yml').read_text())
    public = [ROOT / 'README.md', *(ROOT / 'docs' / page for page in pages(config['nav']))]
    for path in public:
        check(path)
    html = list(SITE.rglob('*.html'))
    assert html, 'Build the site first with mkdocs build --strict'
    for path in html:
        check(path, site=True)
    for private in ('session-handoff', 'requirements', 'hardware-validation', 'hooks.py'):
        assert not (SITE / private).exists(), ('maintainer document was published', private)
    for source, destination in (
        ('scripts/install.sh', 'install.sh'),
        ('LICENSE', 'assets/licenses/SERICON-LICENSE'),
        ('examples/config.toml', 'assets/examples/config.toml'),
        ('formulas/examples/tplink-uboot-interrupt.rhai', 'assets/examples/tplink-uboot-interrupt.rhai'),
        ('helper/MUSL-COPYRIGHT', 'assets/licenses/MUSL-COPYRIGHT'),
        ('vendor/vt100/LICENSE', 'assets/licenses/VT100-LICENSE'),
    ):
        assert (ROOT / source).read_bytes() == (SITE / destination).read_bytes(), destination
    manifest = json.loads((ROOT / 'screenshots/manifest.json').read_text())
    screenshots = {path.name: path for path in (ROOT / 'docs/assets/screenshots').glob('*.png')}
    assert set(screenshots) == set(manifest), 'Screenshot inventory differs from manifest'
    for name, screenshot in screenshots.items():
        data = screenshot.read_bytes()
        assert hashlib.sha256(data).hexdigest() == manifest[name]['sha256'], ('screenshot hash differs', name)
        original = ROOT / 'screenshots' / manifest[name]['original']
        if original.exists():
            assert data == original.read_bytes(), ('screenshot differs from original', name)
    print(f'Checked {len(public)} source documents and {len(html)} built pages: links, anchors, resources, alt text, downloads and screenshot copies passed')


if __name__ == '__main__':
    main()
