# Third-party components

Sericon's own source and documentation use the [MIT license](../LICENSE), copyright 2026 Andrew Bellini. The components below retain their own notices.

The optional static DUT helpers include musl libc 1.2.5. The [musl copyright notice](../helper/MUSL-COPYRIGHT) is retained with the source and embedded in Sericon. GCC startup and integer runtime objects may also be linked under the [GCC Runtime Library Exception](https://www.gnu.org/licenses/gcc-exception-3.1.html). Helper build manifests record compiler versions and payload hashes.

The terminal uses a vendored vt100 parser. Its [MIT license](../vendor/vt100/LICENSE) is retained in the source tree. Rust dependencies and exact versions are recorded in `Cargo.toml` and `Cargo.lock`; their individual licenses remain applicable.

Print the embedded helper notices with:

```sh
sericon files helpers --licenses
```
