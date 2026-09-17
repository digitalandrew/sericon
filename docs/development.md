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

## Prepare release binaries

Use an x86-64 Linux build host with Python 3.12, rustup, `readelf`, the [helper cross-compilers](helper.md#building) and QEMU user emulators on `PATH`. `scripts/build-release.sh` builds all four DUT helpers, then static x86-64, ARM64 and ARMv6 hard-float host executables using the pinned Rust toolchain and its bundled linker.

```sh
sh scripts/build-release.sh
python3 tests/helper.py
SERICON_TEST_BINARY=target/x86_64-unknown-linux-musl/release/sericon python3 tests/integration.py
python3 tests/arm_smoke.py
```

Run the Rust checks above as well. Commit the release source, then package from the clean checkout:

```sh
python3 scripts/package-release.py
cd target/releases/v0.1.0
sha256sum --check SHA256SUMS
```

The packager checks static linking, executable versions and embedded helper hashes, including ARM startup under QEMU. It creates three archives with installation instructions, examples, license notices, MPL dependency sources and build provenance, plus `SHA256SUMS`. Output defaults to `target/releases/vVERSION`; it must be a new directory. Use `--output PATH` for a separate validation run.

Tag the validated source commit and upload these files to a GitHub release. Use a prerelease for beta builds. Verify downloaded asset hashes before publishing download links. Keep private logs and toolchains outside release packages.

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

The Sericon mark is a vessel crossed by a serial pulse, using the Athanor family's 64×64 grid and rounded instrument linework. The [brass and copper mark](assets/sericon.svg), [favicon tile](assets/sericon-favicon.svg) and [single-colour mark](assets/sericon-mono.svg) are SVGs. Keep their vessel, pulse and terminal geometry identical; the favicon uses a heavier stroke for small sizes. Terminal text uses its compact text symbol because ordinary terminals cannot render the SVG mark.

The session handoff, historical requirements and detailed local hardware notes remain local maintainer records, excluded from Git and the site. No serial captures, `.local` files or runtime logs are site inputs.

## Publish the beta documentation

The documentation is hosted on GitHub Pages at [sericon.xyz](https://sericon.xyz/). The [Documentation workflow](https://github.com/digitalandrew/sericon/actions/workflows/docs.yml) builds the site in strict mode and checks links, downloadable assets and screenshot hashes on pull requests and pushes to `main`. Only a successful build from `main` is published. You can also run the workflow manually from the Actions tab.

The deployment uploads only `.local/docs-site/`. GitHub Pages uses a custom Actions workflow, with `sericon.xyz` set as the repository's custom domain. `mkdocs.yml` uses the same canonical URL. DNS is managed in Porkbun, with the apex and `www` pointing to GitHub Pages. The workflow uses GitHub's deployment permissions and needs no Porkbun credentials.

The public source repository is [digitalandrew/sericon](https://github.com/digitalandrew/sericon), licensed under [MIT](../LICENSE). Binary downloads and checksum instructions are in [installation](installation.md#download-a-prebuilt-release).

The build publishes `scripts/install.sh` unchanged at `https://sericon.xyz/install.sh`; the docs checker compares its bytes with the source. For a new release, update the installer's version and three pinned archive checksums after verifying the published assets. Run `python3 tests/installer.py` to check installation, architecture selection and failure handling with temporary prefixes before publishing the script.
