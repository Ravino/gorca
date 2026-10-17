# Third-party credits and licences

gorca is licensed as described in the README. This file lists the projects gorca learned from or
uses, and what that means for licensing. "Reference" means we read the code or documentation to
understand behaviour and wrote our own implementation; no code was copied unless stated.

## Content derived from third-party work

| Where | Source | Licence of the source | Nature |
|---|---|---|---|
| `crates/gorca-speech/data/symbols-en.dic`, `symbols-ru.dic` | NVDA `source/locale/{en,ru}/symbols.dic` (Copyright NVDA contributors) | GPL-2.0-or-later | Symbol names and levels modelled on NVDA's tables; a part of the entries (names or levels) coincides with NVDA's. The file format, the identifiers and the `@word.*` strings are gorca's own. Treated as derived from NVDA and distributed under GPL-3.0-or-later, which is compatible with the source licence. |

## References (behaviour studied, no code copied)

| Project | Licence | What we used it for |
|---|---|---|
| [NVDA](https://github.com/nvaccess/nvda) | GPL-2.0-or-later with exceptions | The model for browse mode, quick navigation, key layout, role and state wording and order, punctuation levels (`complexSymbols` rule set), forms and focus-mode rules. "NVDA-style" presets only follow its key layout. NVDA is a trademark of NV Access; gorca is not affiliated with it. |
| [Orca](https://gitlab.gnome.org/GNOME/orca) | LGPL-2.1-or-later | Behaviour of the GNOME Shell and application scripts, the "Orca-style" key layout, the key monitor protocol gorca joins when it takes over from Orca. gorca does not use Orca code. |
| [Odilia](https://github.com/odilia-app/odilia) | GPL-3.0 | Prior art for an async Rust screen reader on AT-SPI. |
| [atspi-rs](https://github.com/odilia-app/atspi) | Apache-2.0 / MIT | Reference for the AT-SPI wire types and event shapes. |
| Newton (Matt Campbell's prototype, https://gitlab.gnome.org/mwcampbell) and [AccessKit](https://github.com/AccessKit/accesskit) | MIT / Apache-2.0 | Design reference for the planned Newton connector (Wayland-native accessibility tree). |
| [Mutter](https://gitlab.gnome.org/GNOME/mutter) and GNOME Shell | GPL-2.0-or-later | Key monitor and accessibility manager semantics re-implemented in the input model and in the test fake (`crates/gorca-input-api/src/mutter_model.rs`, `tests/harness/fake_mutter.py`), and Shell behaviour for the Shell extension. |
| [at-spi2-core](https://gitlab.gnome.org/GNOME/at-spi2-core), [GTK](https://gitlab.gnome.org/GNOME/gtk), [VTE](https://gitlab.gnome.org/GNOME/vte), Firefox, Chromium | various open source licences | Accessibility interface contracts and toolkit quirks, recorded in `docs/platform/`. |
| [speech-dispatcher](https://github.com/brailcom/speechd), [espeak-ng](https://github.com/espeak-ng/espeak-ng), [BRLTTY](https://github.com/brltty/brltty) | GPL-2.0-or-later / LGPL | Speech and braille client protocols and behaviour. gorca talks to speech-dispatcher over its socket and links none of them. |
| voicego | AGPL-3.0 | Separate speech server, owned by the gorca author. gorca implements the client from the protocol description and shares no code with it. |

## Build-time and run-time dependencies

Rust dependencies are fetched by Cargo and are not vendored. The main ones: tokio, zbus,
wasmtime and wit-bindgen, thiserror, anyhow, tracing, serde, toml_edit, scc, arc-swap, regex,
chrono, inotify. All dependency licences (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0 and
combinations of these, some with the LLVM exception) are compatible with GPL-3.0-or-later; none is
GPL-2.0-only, AGPL or proprietary.

The licence notices for the dependencies that go into a binary are generated at release time with
[cargo-about](https://github.com/EmbarkStudios/cargo-about), using `about.toml` and `about.hbs`:

```sh
cargo install cargo-about
cargo about generate about.hbs > THIRD-PARTY-LICENSES.html
```

The generated file ships with release artefacts and is not committed.

## Test fixtures

The test pages under `tests/web/` are written for this project. They mimic the structure of common
sites (for example an encyclopedia article) but contain no copied site code.
