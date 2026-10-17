# Contributing to gorca

Thank you for your interest. Open an issue before you start on a large change, so that the
design can be agreed first. Design records are in `docs/adr/`, decisions in `docs/decisions.md`.

## Licence of contributions

There is no contributor licence agreement (CLA). Contributions are licensed inbound = outbound:

- Files under `crates/gorca-ext-api/` and `crates/gorca-i18n/` (the extension SDK) are
  MIT OR Apache-2.0.
- Everything else, including the built-in extensions, is GPL-3.0-or-later.

Every source file carries an `SPDX-License-Identifier` line; keep it, and add it to new files.
You keep the copyright of your contribution.

## Developer Certificate of Origin

Every commit must carry a `Signed-off-by` line with your real name and email, added with
`git commit -s`:

```
Signed-off-by: Jane Doe <jane@example.org>
```

By adding it you certify the Developer Certificate of Origin, version 1.1 (https://developercertificate.org/):

1. The contribution was created in whole or in part by you and you have the right to submit it
   under the open source licence indicated in the file; or
2. The contribution is based upon previous work that, to the best of your knowledge, is covered
   under an appropriate open source licence and you have the right under that licence to submit
   that work with modifications, whether created in whole or in part by you, under the same open
   source licence (unless you are permitted to submit under a different licence), as indicated in
   the file; or
3. The contribution was provided directly to you by some other person who certified (1), (2) or
   (3) and you have not modified it.
4. You understand and agree that this project and the contribution are public and that a record
   of the contribution (including all personal information you submit with it, including your
   sign-off) is maintained indefinitely and may be redistributed consistent with this project or
   the open source licence(s) involved.

Do not copy code from projects whose licence is not compatible with the licence of the file you
change. If you were inspired by another screen reader, say so in the commit message.

## Building and checking

See the README for the toolchain. Before you send a change:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./tools/check-deps.sh      # crate dependency rules (ADR-0001, section 11)
```

Rules the code follows:

- Every state machine is an `enum` with an exhaustive `match`.
- No `unwrap()`, `expect()` or `panic!` outside tests.
- No IPC call without a deadline; actors never block.
- App-specific behaviour goes into an extension, not into the core.
- All repository text (code, comments, docs, commit messages) is English.

## Commits

Use a conventional, imperative subject line, for example `fix(terminal): retry the baseline read`.
