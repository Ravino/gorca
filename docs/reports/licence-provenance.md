# Licence provenance audit (pre-publication)

Date: 2026-10-17. Tree: `main` at 8d784101. Read-only audit; no code changed.
Question: can gorca be published as **GPL-3.0-or-later** (core) with the **extension SDK / WIT
interfaces under MIT OR Apache-2.0**, so third-party extensions can use any licence?

Answer: **yes**, with four pre-publication actions (section 6). Nothing must be rewritten; one
file pair must have its provenance header corrected and attribution added.

## 1. Current licence state of the repo

- No `LICENSE`, `COPYING` or `NOTICE` file is tracked (`git ls-files | grep -iE 'licen|copying|notice'`
  is empty). No SPDX headers in any tracked file.
- `Cargo.toml:16-17`: `# TBD: the owner has not chosen a license yet.` / `license = "TBD"`;
  every crate uses `license.workspace = true`, `publish = false`, `repository = "TBD"`.
- Git history: 512 commits, single author (`Evgeniy Nekrasov`). The owner holds copyright in all
  original code and can choose (and later add exceptions to) any licence.

## 2. Third-party-derived content

### 2.1 `crates/gorca-speech/data/symbols-en.dic`, `symbols-ru.dic` — DERIVED FROM NVDA

Header (line 1 of both files) claims: "Modelled on the behaviour of NVDA symbols.dic; names are
original." / "названия собственные". **This claim is false.** Compared against
`refs/nvda/source/locale/{en,ru}/symbols.dic` (NVDA commit 732d1ce2, 2026-10-09), keyed by symbol:

| table | gorca entries | keys also in NVDA | identical spoken name | identical level |
|---|---|---|---|---|
| en | 130 | 77 | 56 | 71 |
| ru | 130 | 83 | 65 | 76 |

Distinctive identical choices include ru `разорванная вертикальная черта` (¦),
`перевёрнутый восклицательный знак` (¡), `левая круглая` / `правая круглая`, `пробел нулевой
ширины`, `собака` (@), `крышка` (^); en `bang`, `left paren`, `star`, `broken bar`, `en dash`,
`double prime`. The `@sentence.*`, `@phrase.*`, `@multiple.dot`, `@decimal.*`,
`@inword.apostrophe`, `@negative.number` rules mirror NVDA's `complexSymbols` set with the same
levels and preserve flags (`crates/gorca-speech/src/symbols.rs:77` says so). The `@word.*` UI
strings and ~50 symbol names per table are original.

NVDA licence: `refs/nvda/copying.txt:3` — "NVDA is available under the GNU General Public
License version 2 or later, with two special exceptions", and the file states it is
authoritative over per-file headers. The `symbols.dic` headers say "Copyright (c) 2011-2024 NVDA
Contributors / This file is covered by the GNU General Public License." No carve-out for locale
data. So the source is **GPL-2.0-or-later** → can be incorporated into a GPL-3.0-or-later work.

Consequences:
- Compatible with the proposed core licence. **Not** usable in the permissive SDK, and blocks any
  future relicensing of `gorca-speech` (or these data files) to MIT/Apache unless re-sourced.
- GPL requires keeping NVDA's copyright notice and stating the modification; the current header
  does the opposite (asserts originality). Must be fixed before publishing.

Action (required): replace the header lines in both files with e.g.
```
# SPDX-License-Identifier: GPL-3.0-or-later
# Derived from NVDA's source/locale/<lang>/symbols.dic
#   Copyright (c) 2011-2024 NVDA Contributors, GPL-2.0-or-later (https://github.com/nvaccess/nvda).
# Modified for gorca: own format and identifiers, additional entries, UI words.
```
and list them in a `THIRD-PARTY.md`/`NOTICE`. Optional alternative if a permissive table is ever
needed: re-source names from Unicode CLDR annotations (Unicode-3.0, permissive) and drop the
NVDA-identical rows; not needed for GPL-3.0-or-later.

### 2.2 Behavioural ports ("after NVDA / Orca / Mutter"), all into GPL-compatible core

| file | source cited | nature | licence of source | verdict |
|---|---|---|---|---|
| `crates/gorca-core/src/browse/forms.rs:1-2` | NVDA `browseMode.py:326-440` `shouldPassThrough` | re-expressed algorithm in Rust over gorca's own `Role`/`StateSet`; no copied text or comments | GPL-2.0-or-later | OK under GPL-3.0+; keep the citation |
| `crates/gorca-core/src/aria.rs:9-13` | NVDA `controlTypes/role.py`, `state.py`, `speech.py`, `aria.py` | short English labels ("not checked", "half pressed", "opens dialog"…) and ordering | GPL-2.0-or-later | de minimis / functional; OK under GPL-3.0+ |
| `crates/gorca-speech/src/symbols.rs:77` | NVDA `complexSymbols` | rule set, hand-coded (no regex copied) | GPL-2.0-or-later | OK |
| `extensions/ext-gnome-shell/src/lib.rs:15-17,105`; `crates/gorca-core/src/builtin_ext.rs:65` | Orca `scripts/apps/gnome-shell/script.py`; gnome-shell JS | behavioural rules only, no code | LGPL-2.1-or-later / GPL-2.0-or-later | OK, even if the extension were permissive (ideas only) |
| `tests/harness/fake_mutter.py:18-23`, `tests/harness/README.md:231-234`, `crates/gorca-input-api/src/mutter_model.rs:3` | Mutter `meta-a11y-manager.c`, `meta-seat-impl.c` | "a port of real Mutter code": semantics re-implemented in Python/Rust | GPL-2.0-or-later | OK under GPL-3.0+. Keep `gorca-input-api` GPL (do not fold it into the permissive SDK) |
| `crates/gorca-text/src/caret.rs:44,96-103` | NVDA/Orca caret behaviour | prose citations | — | OK |
| `crates/gorca-config/presets/{nvda,orca}-{desktop,laptop}.toml` | NVDA/Orca key layouts | key→own-command mappings in own format | functional facts | OK (see trademark note, section 7) |
| `crates/gorca-input-api/src/keys.rs` | X11 keysym values | numeric constants | facts | OK |

No message catalogues (`.po/.pot/.ftl`) are tracked. Russian UI strings in Rust
(`present/echo.rs:77-81`, a few test assertions) are short and original. No copied code,
comments or docstrings from orca, nvda, odilia, brltty, speech-dispatcher or espeak-ng were found
(greps for "ported from / adapted from / copied from / verbatim / refs/<project>" and for Python/C
code shapes in `docs/`). `gorca-braille-brlapi` is a stub with no BrlAPI code or linkage.

### 2.3 Test fixture with third-party prose

`tests/web/m8/pages/wiki.html`: the lede "A screen reader is a form of assistive technology that
renders text and image content as speech or braille output. Screen readers are essential to
people who are blind." reproduces the English Wikipedia "Screen reader" article (CC BY-SA 4.0,
one-way compatible with GPLv3). Two sentences, likely de minimis; the rest is invented. The page
also mimics Wikipedia chrome ("About Wikipedia", "Search Wikipedia"). Action (recommended):
reword the two sentences (and check the e2e expectations that quote them) or add a CC BY-SA
attribution comment.

### 2.4 Data blobs

Tracked `.gz`/`.json` trees (`research/web-graph/real-trees/`, `tests/web/spa/results/`,
`docs/reports/m4b-shell-probe-logs/`) are accessibility-tree captures of the project's own
synthetic pages (`file:///…/static/large10000.html`) and own SPA test apps. No third-party
content. `tests/web/spa/node_modules`, `tests/web/b6/virt/node_modules` and `dist/` are ignored.

## 3. Rust dependency licences

`cargo metadata --format-version 1 --offline` (no build): 280 third-party packages.

| licence expression | count |
|---|---|
| MIT OR Apache-2.0 (all spellings) | 168 |
| MIT | 41 |
| Apache-2.0 WITH LLVM-exception (wasmtime, cranelift, regalloc2, pulley, target-lexicon) | 29 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT (wasm-tools, wit-bindgen, rustix, linux-raw-sys, wasi) | 20 |
| **Apache-2.0 only** (ciborium ×3, scc, saa, sdd) | 6 |
| Unlicense OR MIT | 6 |
| BSD-2-Clause OR Apache-2.0 OR MIT (zerocopy) | 3 |
| Zlib (foldhash) | 2 |
| ISC (inotify, inotify-sys) | 2 |
| (Apache-2.0 OR MIT) AND BSD-3-Clause (encoding_rs) | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 (unicode-ident) | 1 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later (r-efi) | 1 |

- No GPL-2.0-only, AGPL, SSPL, BUSL, unknown or missing licence fields.
- All compatible with GPL-3.0-or-later. The six Apache-2.0-only crates are **incompatible with
  GPL-2.0-only**, which is a concrete reason to choose GPL-3.0(-or-later), not GPL-2.0.
- No dependency ships a `NOTICE` file (checked every package directory), so Apache §4(d) adds no
  obligation. Binary distributions still need the MIT/BSD/ISC/Zlib/Unicode notices; generate a
  `THIRD-PARTY-LICENSES` file with `cargo about` or `cargo deny` at release time.
- SDK crate `gorca-ext-api` has **no** internal dependencies; its only runtime dependency is
  `wit-bindgen =0.46.0` (wasm32 only; Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) and
  dev-dep `wit-parser` (same). It can be MIT OR Apache-2.0 without conflict.

Non-Rust: `tests/web/*` JS/TS test apps depend on npm packages (not vendored); `contrib/` GNOME
Shell extension imports gnome-shell's runtime only.

## 4. voicego (AGPL-3.0)

- `crates/gorca-speech-voicego/src/lib.rs:24-25`: "voicego is AGPL-3.0-or-later. gorca implements
  the client from the protocol specification and does not vendor or link voicego code."
  Verified: the crate depends only on `gorca-model`, `gorca-rt`, `gorca-output-api`, `thiserror`,
  `tokio`, `tracing`; no `.go` files are tracked anywhere; no voicego source in `crates/`.
  Communication is SSIP over a UNIX socket with length framing → separate program, no linking.
- `docs/platform/voicego-protocol.md` and `docs/reports/2026-10-12-voicego-*.md` describe the
  protocol and fixes; per CLAUDE.md voicego is the owner's own project, so even quoted snippets
  are the owner's to publish.

## 5. Is "GPL core + permissive SDK" sound?

- **Mechanics: yes.** `gorca-ext-api` (`crates/gorca-ext-api/`, including
  `wit/gorca-ext.wit`) is self-contained and wholly owner-authored; it can carry
  `license = "MIT OR Apache-2.0"` while the workspace default is `GPL-3.0-or-later`. Same for
  `bench/wasm-hook/wit/quirk.wit` if published.
- **Legal gap: the GPL itself.** A permissive SDK does not by itself settle whether an extension
  loaded into the GPL host is a "covered work". The FSF's plugin FAQ treats plugins that share
  data structures/function calls with the host as possibly forming a combined work; a WASM
  component boundary is a strong separation argument but untested. Proprietary extension authors
  will want certainty. Fix: add a **GPL-3.0 section 7 additional permission** to the core (the
  owner is sole copyright holder, so this is possible now and only now without a CLA):
  > Additional permission under GNU GPL version 3 section 7: an Extension that communicates with
  > gorca only through the interfaces defined in `crates/gorca-ext-api/wit/` (and the
  > `gorca-ext-api` crate) is not a work based on gorca, and may be distributed under terms of
  > your choice.
  This is analogous to NVDA's "Non-GPL Components in Plugins" exception
  (`refs/nvda/copying.txt:976-985`) and the GCC Runtime Library Exception. The NVDA-derived
  symbol tables do not conflict: extensions never receive them.
- **First-party extensions** (`extensions/ext-gnome-shell`, `ext-vte`, `ext-test`): native
  builds are linked into the daemon as fallbacks; either licence works. Recommend MIT OR
  Apache-2.0 so they double as SDK examples (sources are behavioural only, section 2.2).
- **Keep under GPL:** `gorca-input-api` (Mutter-derived model), `gorca-speech` (NVDA-derived
  tables), `gorca-core`. Do not later move `connector-api`/`output-api`/`input-api` into the
  permissive set without re-checking `mutter_model.rs`.
- **Contributions:** adopt DCO; state inbound=outbound per directory (GPL for core, MIT/Apache
  for SDK). Without a CLA, outside contributions will freeze the ability to add further
  exceptions; add the section 7 permission before accepting external PRs.
- `contrib/gnome-shell-extension/`: extensions.gnome.org requires GPL-2.0-or-later-compatible
  terms; GPL-3.0-or-later is accepted there, GPL-2.0-or-later is the conventional choice.

## 6. Required / recommended actions before going public

Required:
1. `crates/gorca-speech/data/symbols-en.dic:1`, `symbols-ru.dic:1`: replace the false
   "names are original" line with NVDA attribution (template in 2.1); add an SPDX line.
2. Add `LICENSE` (GPL-3.0 text) at the root, `LICENSES/` or `crates/gorca-ext-api/LICENSE-MIT`
   and `LICENSE-APACHE`; set `Cargo.toml:16-18` `license = "GPL-3.0-or-later"`, real
   `repository`; set `license = "MIT OR Apache-2.0"` in `crates/gorca-ext-api/Cargo.toml` (and
   the first-party extensions if chosen).
3. Add the GPL-3.0 section 7 extension permission (section 5) to `LICENSE`/`COPYING` notice and
   README.
4. Add `THIRD-PARTY.md` listing NVDA (symbol tables, GPL-2.0-or-later) and noting behavioural
   references to NVDA, Orca, Mutter, gnome-shell; generate dependency notices at release
   (`cargo about`).

Recommended:
5. `tests/web/m8/pages/wiki.html`: reword the two Wikipedia lede sentences or attribute them.
6. SPDX headers on source files (`// SPDX-License-Identifier: GPL-3.0-or-later`, SDK:
   `MIT OR Apache-2.0`); optionally `cargo deny check licenses` in CI with an allow-list.

## 7. Not licence, but check before publishing (unverified)

- Trademarks: preset names `nvda`/`orca`, process/bus-name impersonation of Orca, and the
  project name "gorca". Nominative use is normally fine; "NVDA" is an NV Access trademark
  (not verified here). Describe presets as "NVDA-style"/"Orca-style".
- `.claude/` (agent memory, prompts), `bench/results/`, `docs/reports/` contain host details and
  internal notes; commit author email `evgeniy@nekrasov.pw` becomes public. Untracked
  `gorca.err` should not be committed.
- No legal advice: the section 7 wording should be checked by someone who drafts licence
  exceptions (e.g. compare with the GCC RLE and Linux syscall note).

## Commands

```
git ls-files | grep -iE 'licen|copying|notice'                 # empty
. ./env.sh && cargo metadata --format-version 1 --offline       # 280 third-party packages
python3 compare symbols-{en,ru}.dic vs refs/nvda/source/locale/{en,ru}/symbols.dic
git ls-files | xargs grep -n -iE 'ported from|adapted from|copied from|verbatim|refs/(nvda|orca|...)'
```
