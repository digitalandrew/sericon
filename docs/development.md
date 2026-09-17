# Development

Use the Rust toolchain pinned in `rust-toolchain.toml`. Serial access belongs to the session broker; terminal clients, CLI commands and MCP bridges use the same local interface. New operations must preserve writer coordination, exact RX/TX history and terminal restoration.

## Build and validate

From the source checkout:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python3 tests/integration.py
```

Integration tests use isolated pseudoterminals and temporary storage. They do not open physical adapters. Set `SERICON_TEST_BINARY=target/release/sericon` to test a native release build.

Prepare the [DUT helper toolchains](helper.md#building) before a full release build:

```sh
python3 helper/build.py
python3 tests/helper.py
cargo build --release --locked
sh scripts/build-pi.sh
python3 tests/arm_smoke.py
```

The helper and ARM host tests require QEMU user emulators. Record physical tests separately from CPU emulation. Current coverage is in [beta validation](beta.md#validation).

## Preview the documentation

The website uses MkDocs with Material. These Python packages are documentation build dependencies; they do not become Sericon runtime dependencies.

```sh
python3 -m venv .local/docs-venv
.local/docs-venv/bin/python -m pip install -r requirements-docs.txt
.local/docs-venv/bin/mkdocs serve --dev-addr 127.0.0.1:8765
```

Open `http://127.0.0.1:8765`. The guides in `docs/` are the website source. The site uses unmodified screenshots with descriptive filenames under `docs/assets/screenshots/`. Their hashes are recorded in `screenshots/manifest.json`; original captures remain local to the maintainer checkout.

Build and check the static output:

```sh
.local/docs-venv/bin/mkdocs build --strict
.local/docs-venv/bin/python scripts/check-docs.py
```

Output is written to `.local/docs-site/`. The build includes the canonical config, formula example, MIT license and dependency notices as downloadable files. The hook rewrites those source-tree links for the website, so Markdown links still work in a checkout.

The small `docs-theme/` override renders the GitHub link without fetching repository or release statistics. Fonts, search and images are served with the site.

The session handoff, historical requirements and detailed local hardware notes remain local maintainer records, excluded from Git and the site. No serial captures, `.local` files or runtime logs are site inputs.

## Publish the beta documentation

`mkdocs.yml` sets the intended canonical URL to `https://sericon.xyz/`. Deploy only the contents of `.local/docs-site/` to a static host. Domain registration, DNS, hosting and publication are separate from a local build.

The public source repository is [digitalandrew/sericon](https://github.com/digitalandrew/sericon), licensed under [MIT](../LICENSE). Release binaries have not yet been published. When they are ready, publish artifacts with checksums and add their verified download links to the installation guide and README.
