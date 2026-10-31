# Release notes and checklist: lemonade-engines v0.4.0 (not tagged, not pushed)

New in v0.4.0: the `ternlight` embeddings engine and the "Bitter Lemons" group with the `movidius` (Intel NCS1) engine.
Catalogue fields added: `kind` (chat, embeddings, vision), `group`, `group_note`, `requires_usb`, `user_file`, `firmware`, `pending`.
New routes: `POST /api/engines/{id}/usb` and `/firmware` (the latter downloads only with `{"accept": true}`).
Release order: ternlight-server v0.1.1 (done, public) and movidius-rust v0.3.0 (done, public), then tag this.

Before tagging v0.4.0:
- Pins to check: ternlight-server v0.1.1 SHA256SUMS (four archives, already in `assets/engines.toml`), movidius-rust v0.3.0 tarball sha256 `067f6d27...2e69c`.
- Movidius needs a compiled graph from the user (`LE_MOVIDIUS_GRAPH`). Decide whether to publish a SqueezeNet v1.1 graph on a release so Set up needs nothing else.
- Lemonade 2026.43.3 does not forward `/v1/embeddings` to cloud providers; the ternlight test therefore passes on the engine's own answer and prints Lemonade's refusal.
- Re-run the e2e against the CI-built Stand binaries on Linux, Windows (needs github.com reachable from the Windows box) and the stick machine.

---

# Release checklist: lemonade-engines v0.3.0

Nothing here has been pushed, tagged or released. Branch to release from: `finish/entries-2026-10-28`
(merge to master first). `cargo test` passes (8 tests); also run on Windows 11 (6 tests at the time).

## 1. Artifacts to build and upload to the v0.3.0 release

| File | How | sha256 (of the tested file) |
|---|---|---|
| `lemonade-engines` for Linux x86-64, Windows x86-64, macOS (Intel, Apple silicon) | CI from the tag, as v0.2.0. This branch adds the `zip` crate, so rebuild all four. The Windows binary was built and run on Windows 11 here with `cargo build --release` | n/a (CI) |
| `iced-lemcake-2026.43.3-x86_64-linux.tar.gz` | `packaging/iced-lemcake/build-linux.sh WORKDIR` on t440 (glibc 2.39; needs glibc 2.38 or newer) | `67e4b72f4de89e5254140c060ba09a7ad24efae0f9a15cb8afd2d63db43ead74` |
| `iced-lemcake-2026.43.3-x86_64-windows.tar.gz` | `packaging/iced-lemcake/build-windows.cmd` (VS 2022 Build Tools, git, cargo, a Python for CMake). Tested file was assembled by hand with the same steps | `a9e5b6d6aaa88329e98ac2f67e166679caa8141778ac72122e46af6387d241a0` |

The iced packages are not byte-reproducible. Upload the exact files that were tested, or rebuild, retest and
replace the two sha256 lines in `assets/engines.toml` (`icedlemcake`, search `sha256`). The tested Linux file is
on t440 at `/mnt/fast/le-test-20261028/pkgbuild/`; the Windows one on nautik9 at `C:\Users\thom\pk\`.
Source of the packages: lemonade-fork branch `iced-lemcake/2026.43.3` plus
`packaging/iced-lemcake/0001-*.patch`; bridge from lemonade_cake_integration `bridge/runtime-name-2026-10-22`;
lemonade-burn-server 0.1.1 (sha256 pinned in the scripts). Ship the patch and licences (already inside the tarballs).
Decide first whether publishing the two modified-Lemonade packages is wanted (Lemonade is Apache-2.0; the
Cake backend only launches Cake and does not contain it).

## 2. Pins in `assets/engines.toml`

Already set and verified by running: burn (4 platform tarballs + model), bonsai/bonsaicpu (PrismML builds
.tar.gz and .zip, model), strata `install.sh`. To update before tagging:
- `icedlemcake`: the two `sha256` lines above, if the files are rebuilt. The URLs already point at
  `releases/download/v0.3.0/`.
- Nothing else changes. Cake has no files (not redistributed).
- Optional: `lemonade-burn` is pinned at v0.1.1; Windows download verified.

## 3. Tests that passed (2026-10-28, Lemonade 2026.43.3, through the page with `tests/e2e/setup.mjs`)

See the Status table in `README.md`. In short: burn, bonsaicpu, bonsai (GPU), icedlemcake pass on Linux and
Windows; Cake passes on Linux (cesarops4om) only; Strata installer and plan pass on Linux, nothing further.

## 4. Before tagging

- Re-run `node tests/e2e/setup.mjs` for each engine against the final CI binaries, on Linux and Windows, with the
  catalogue pointing at the uploaded URLs (the tests here used `file://` copies of the iced packages).
- Teaser entries ready now: burn, bonsaicpu, bonsai, icedlemcake. Not ready: Cake (needs a Cake program the user
  builds; not tested on Windows), Strata (Download/Start/Stop never run).
- Strata has no NautiBurn scraper hook (fleet rule: every MoE engine gets one). Not built in this task.
- Open item: the README Status text is a draft for operator OK.
