# NautiK9 Lemonade Stand

(Formerly "Lemonade Engines"; the program and repo keep the name `lemonade-engines`.)

Add engines to the [Lemonade](https://github.com/lemonade-sdk/lemonade) server you already run.
Each engine runs as its own OpenAI-compatible server on `127.0.0.1` and joins Lemonade as a
provider, so its models show up in Lemonade's list and work in every app that talks to Lemonade.
Lemonade itself is not modified.

## Run

```sh
cargo build --release
./target/release/lemonade-engines
```

The page opens in your browser at `http://127.0.0.1:13500`. Press **Set up** on an engine: it
installs the files, starts the server, adds it to Lemonade and asks one test question, showing
each step. The individual steps are still available under "Individual steps".

| Variable | Default | Meaning |
|---|---|---|
| `LEMONADE_HOST`, `LEMONADE_PORT` | `127.0.0.1`, `13305` | Your Lemonade server |
| `LE_LISTEN` | `127.0.0.1:13500` | Address of this page |
| `LE_CATALOG` | built-in `assets/engines.toml` | Engine catalogue (TOML) |
| `LE_HOME` | `~/.local/share/lemonade-engines` | Where engines are installed |
| `LE_NO_BROWSER` | unset | Set to skip opening the browser |

## Download

Builds for Linux x86-64 (glibc 2.30 or newer), Windows x86-64 and macOS (Intel and Apple silicon)
are on the [releases page](https://github.com/festeraeb/lemonade-engines/releases); GitHub Actions
builds them from each tag. Unpack and run `lemonade-engines`. The page then opens in your browser.

## Engines

| Engine | What Set up does | Platforms |
|---|---|---|
| lemonade-burn | Downloads the server for this computer and the BitNet model (sha256 pinned), starts it, adds it to Lemonade, asks a test question | Linux, Windows, macOS |
| Strata | Runs Strata's own signed installer into the engine folder, shows the plan (model and download size) and waits for **Download and start**, then serves through `strata launch` | Linux x86-64, Apple silicon Mac |
| Ternary Bonsai 8B (GPU / CPU) | Downloads PrismML's llama.cpp build for this computer (.tar.gz or .zip) and the ternary Bonsai 8B model (both sha256 pinned), starts it, adds it to Lemonade, asks a test question | Linux, Windows |
| Iced lemon cake | Downloads a Lemonade 2026.43.3 server with the Cake and lemonade-burn backends (see `packaging/iced-lemcake`), starts it on 127.0.0.1:13410, loads a small test model, adds it to Lemonade, asks a test question | Linux x86-64, Windows x86-64 |
| Cake | Attaches to the Iced lemon cake server, registers a Cake model and adds it to Lemonade. Cake's program is not bundled (FAIR licence): put it at `bin/cake` in the engine folder, on PATH, or set `LE_CAKE_BIN` | Where Cake builds |

## Tests

`tests/e2e/setup.mjs` drives the page in a real browser (Playwright): it presses Set up for one engine,
waits for each step, prints the test chat the page shows, then presses Stop.

```sh
cd tests/e2e && npm install
node setup.mjs --url http://127.0.0.1:13500 --engine burn --channel chrome   # or --channel msedge, or --exe /usr/bin/google-chrome
```

Add `--accept` for Cake, `--plan` to accept Strata's download plan, `--shots DIR` for screenshots.
`cargo test` runs the unit tests.

## Status

Tested on 2026-10-28 with the stock Lemonade 2026.43.3 (lemond), through the page, driven by `tests/e2e/setup.mjs`.
Each run: install, start, add to Lemonade, test chat through Lemonade, Stop. Fresh engine folder and a fresh Lemonade each time.
Machines: Windows 11 (build 26200) i7-7820HQ, Quadro M2200 4 GB, Edge; Linux t440 (Ubuntu 24.04, Tesla P100s); Linux cesarops4om (Arch, GTX 1070). Speeds are end to end as the page prints them, on shared machines.

| Engine | Linux | Windows |
|---|---|---|
| lemonade-burn | pass, t440 and c4 (22 tokens, 2.2 to 2.8 s) | pass (22 tokens, 2.0 s idle) |
| Ternary Bonsai 8B (CPU) | pass, t440 and c4 (18 tokens, 1.7 to 12 s depending on load) | pass (18 tokens, 6.6 s idle) |
| Ternary Bonsai 8B (GPU) | pass, t440 (Tesla P100, Vulkan) and c4 (GTX 1070, Vulkan) | pass on the Quadro M2200 through Vulkan (18 tokens, 2.2 s; llama.cpp logged 13.4 tok/s generation) |
| Iced lemon cake | pass, t440 and c4 (LFM2-1.2B, 24 to 29 tokens) | pass (LFM2-1.2B, 24 tokens, 0.7 s) |
| Cake | pass, c4 (Cake 0.1.0 built locally; 23 tokens, 10.2 s, 2.3 tok/s) | not run: no Windows Cake program (see below) |
| Strata | installer and plan pass on t440 (84 GB model, 89.7 GB download); Download, Start and Stop not run | not offered by the catalogue; this machine's GPU is 4 GB, Strata needs 12 GB |

Not run, and why:
- Strata Download, Start, Stop: needs 12 GB or more of free video memory and a 90 GB download; no GPU on the checked nodes (t440, c2, c3, c4) had 12 GB free without displacing a running seat.
- Cake on Windows: Cake publishes no binaries, and building it from source on the test machine was not completed.
- Cake on cesarops3: the node became unreachable during the Cake load; the pass above is from cesarops4om.
- macOS: no machine.

Found and fixed while testing:
- A provider that Lemonade keeps across a restart (its runtime key is lost, so it lists no models) counted as registered; Set up skipped Add to Lemonade.
- Cake's licence acceptance was never recorded, because attach engines skipped the install step.
- Windows Stop left the engine's child processes (llama-server) running.
- Cake's launch line pointed at `/home/cesarops/cake-test/bin/cake`.
- Cake and lemonade-burn backends did not compile against Lemonade 2026.43.3, and `cake_port` as a JSON number failed to load.

## Licence

MIT OR Apache-2.0, at your option. Engines keep their own licences; Cake (FAIR, non-commercial)
is never bundled and is fetched only after you accept its licence on the page.
