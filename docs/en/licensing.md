# Licensing policy

refrain is MIT-licensed and public. This page lists what may go into the
repository, what may not, and why. Check it before adding a dependency or a
file you did not write.

## Dependencies: permissive licenses only

Allowed: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0.
`cargo deny check` enforces this in CI, using the list in
[`deny.toml`](../../deny.toml).

Not allowed:

- **GPL and AGPL** in any version. They would extend to the whole program.
- **LGPL and MPL-2.0** need a decision first, not just a line in `deny.toml`.
  Both are usable in principle, with conditions.

Adding a license to the allow list is a policy change. Explain it in the
commit that does it.

## Never in this repository

| What | Why |
|---|---|
| The **ASIO SDK** | Under Steinberg's own license it may not be redistributed. Under GPLv3 it would make binaries GPL. ASIO support will be a build feature, and the builder supplies the SDK. |
| The **VST 2 SDK**, or headers rebuilt from it | It is no longer licensed for new hosts. refrain does not support VST 2. |
| **Third-party plugins**, samples, presets or recordings | We have no right to redistribute them. Tests use plugins and sounds made in this repository. |
| **Signing certificates and credentials** | Secrets. They live in CI secrets. `.gitignore` blocks the usual file types. |
| **Paths from a developer's machine** | Personal and useless to anyone else. Scripts use relative paths or environment variables. CI fails on home-directory paths. |

## Plugin formats

- **VST3**: the SDK is MIT-licensed since version 3.8. refrain uses the
  [`vst3`](https://github.com/coupler-rs/vst3-rs) crate (MIT OR Apache-2.0),
  whose bindings are generated from it.
- **CLAP**: MIT. refrain uses [`clack`](https://github.com/prokopyl/clack)
  (MIT OR Apache-2.0).
- **Audio Units**: part of macOS.

## Trademarks

The code license does not cover names and logos. "VST" and "ASIO" are
Steinberg's trademarks. They are used here only to say what refrain is
compatible with. Do not add the VST logo or a "VST compatible" badge without
first checking Steinberg's current usage terms.

## Codecs

Reading and writing WAV, and reading FLAC and MP3, are fine. **AAC encoding**
may still be covered by patents: use the operating system's encoder
(Media Foundation or AudioToolbox) instead of shipping one.
