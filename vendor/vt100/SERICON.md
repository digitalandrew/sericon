# Local resize patch

Source: vt100 0.16.2, https://github.com/doy/vt100-rust, MIT (see LICENSE).
Runtime source and upstream license/documentation are retained. The manifest
omits upstream development dependencies; Sericon tests the renderer directly.

`src/grid.rs` changes resizing to move rows above a displaced live cursor into
bounded history when shrinking and pull history into the screen when growing.
Upstream `set_size` truncates the bottom of the screen, which loses the most
recent serial output on a height reduction. The existing history append logic
is shared with scrolling. No synthetic bytes are fed through the DUT parser;
partial UTF-8 and escape sequences survive a resize.

Regression coverage lives in `src/live.rs`, with end-to-end resize/navigation
coverage in `tests/integration.py`. Keep this patch explicit when updating vt100.
