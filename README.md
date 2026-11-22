# PercyStar-linux

<p>
  <img src="https://img.shields.io/badge/Platform-Linux%20x64-FCC624?style=flat-square&logo=linux&logoColor=black" alt="Linux" />
  <img src="https://img.shields.io/badge/Build-GitHub%20Actions-2088FF?style=flat-square&logo=githubactions&logoColor=white" alt="GitHub Actions" />
  <img src="https://img.shields.io/badge/Version-v1.4.3-FFA81C?style=flat-square" alt="Version" />
  <img src="https://img.shields.io/badge/License-PercyStar%20Attribution%20(%40rpatty)-red?style=flat-square" alt="License" />
</p>

Linux fork of [rpatty-dev/PercyStar](https://github.com/rpatty-dev/PercyStar). The original project, RAGE 9 research, and core tooling are by [@rpatty-dev](https://github.com/rpatty-dev) and contributors.

## Latest release

- **[v1.4.2-linux](https://github.com/R1y3n/PercyStar-linux/releases/tag/v1.4.2-linux)** - Linux tarball + AppImage

Download from the [Releases page](https://github.com/R1y3n/PercyStar-linux/releases).

## What this fork adds

- Linux x64 GitHub Actions build on Ubuntu 22.04.
- AppImage packaging (`linuxdeploy` + `appimagetool`) for easier distribution.
- GTK3 file dialog backend on Linux (`rfd`).
- Offline mode: startup update check and in-app update UI disabled.
- RPF scan improvements: skip already-identified zlib offsets in the second pass and lower idle model-viewer RAM.
- Scan regression fix so second-pass entries (XML/RBF0/GXT/PSIN/DAT/ZLIB/bin, etc.) are found again for <=512MB archives.
- `--scan` CLI mode for searching archives without launching the GUI.

## Downloads

- `PercyStar-linux-x64.tar.gz` - extract and run `./PercyStar`
- `PercyStar-linux-x86_64.AppImage` - mark executable and run

## AppImage notes

- Built on Ubuntu 22.04, so it targets glibc 2.35+ systems.
- Bundles app dependencies through linuxdeploy/appimagetool; GPU drivers still come from the host.
- If FUSE is not available, run with `--appimage-extract-and-run`.
- Sets `GIO_MODULE_DIR` inside the AppImage to avoid host GIO module symbol mismatches.

## CLI scan

```bash
./PercyStar --scan /path/to/archive.rpf
./PercyStar --scan /path/to/archive.rpf --name "asset_name"
./PercyStar --scan /path/to/archive.rpf --name "asset_name" --type MODEL --limit 20
./PercyStar --scan /path/to/folder --name "asset_name" --json
```

Options:

- `--name PAT` - case-insensitive substring match
- `--type KIND` - entry kind (`MODEL`, `VEH`, `ZLIB`, `RBF0`, `AWC`, ...)
- `--min-size N` / `--max-size N` - size filter (`KB`/`MB`/`GB` supported)
- `--limit N` - stop after N matches
- `--json` - one JSON object per match

## Upstream sync

This fork tracks upstream `rpatty-dev/PercyStar` and cherry-picks compatible code changes. Merged from upstream on this branch: audio AWC superblock fix, emergency light preview, siren sequencer patterns, selectable modular vehicle parts, vehicle configuration presets from `packs.rpf`, and AWC channel detection/version bump to 1.4.3. Upstream README-only credit changes were not copied over this fork's Linux README.

---

## Project Layout

```text
PercyStar/
├── assets/
│   ├── logo.png            # App & window icon
│   ├── logo.ico            # Windows executable icon
├── scripts/
│   ├── build_macos.sh      # Upstream macOS helper
│   ├── setup_deps.ps1      # Optional RAD Video Tools (binkplay.exe) setup
│   ├── awc_extract.py      # Standalone CLI ADAT (.awc) Opus -> .wav helper
│   └── rpf9_carve.py       # Standalone CLI RPF9 carver
├── docs/
│   └── breakthroughs.json  # Research/breakthroughs feed
├── src/
│   ├── main.rs             # PercyStar egui app, scanners, viewers, CLI
│   ├── rdna2.rs            # RDNA2 PS\x00\x09 parser/disassembler/lifter
│   ├── sgd5.rs             # SGD5 shader-group container & DXBC/FSSM extractor
│   ├── dxbc.rs             # DXBC / NOBC / FSSM RDNA2 shader disassembler
│   ├── psin.rs             # PSIN binary PSO schema, enum & instance decoder
│   ├── anim.rs             # crClipDictionary (0DRPz) animation header decoder
│   ├── dat.rs              # RAGE 9 audio metadata (DAT25/26/41/61) decoder
│   ├── rsc9.rs             # RSC9 0x71717171 table & phBound AABB inspector
│   ├── boot.rs             # PE64 bring-up & manifest inspector (--inspect-exe)
│   ├── model3d.rs          # RAGE 9 meshlet decoder, UV/TXD parser & Z-buffer renderer
│   ├── tex.rs              # Console RDNA2 textures, TTPF, RBF0, GFx, 0DRPz & CNavMesh
│   ├── awc.rs              # Pure-Rust ADAT (.awc) Opus -> .wav decoder
│   ├── meta7a.rs           # 0x7a metadata parser
│   └── vehicle_variations.rs # Vehicle variation/preset helpers
├── .github/workflows/linux-build.yml # Linux tarball + AppImage CI
├── app.rc / build.rs       # Win32 icon resource
├── Cargo.toml / Cargo.lock # Pinned Rust dependencies
└── LICENSE
```

## Building from Source (Linux)

Requires Rust on Linux, or use the GitHub Actions workflow in this repo.

```bash
sudo apt-get install -y build-essential pkg-config libgtk-3-dev libssl-dev libasound2-dev libudev-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libwayland-dev \
  libxrandr-dev libxi-dev libxcursor-dev libgl1-mesa-dev libvulkan-dev
cargo build --release --bin PercyStar
```

Binary: `target/release/PercyStar`.

## Credits & Community

- **Original Creator:** [@rpatty-dev](https://github.com/rpatty-dev) (`@rpatty`)
- **Code & Research Contributors:**
  - [@clayeon](https://github.com/clayeon) (PR #34) — Emergency lights with siren sequencer timing, modular fragment/roofbar composition, and `packs.rpf` `0x7a` authored vehicle configurations.
  - [@worstgirlinamerica](https://github.com/worstgirlinamerica) (PRs #30, #31, #32, #33) — `scripts/build_linux.sh`, AWC superblock directory & stereo channel fix in `awc_extract.py`, `cfg(windows)` gating, and explicit `f32` literal fix.
  - [@Xyzstk](https://github.com/Xyzstk) (PRs #8, #29) — `SGD5` shader-group parser, `DXBC`/`FSSM` extraction, and reflection resource naming.
  - [@techfoundrynz](https://github.com/techfoundrynz) (PRs #12, #13, #21–#28) — `>4 GiB` TTPF addressing, format 99 `float3` meshlets, world-scale MHIU terrain in GPU World, north-up 2D map & heightmap alignment, Model Viewer sidebar stability, window/taskbar icons, startup feedback, and `Cargo.lock` sync.
  - [@asm512](https://github.com/asm512) (PR #19) — In-file & sibling vehicle LOD switching (`Hi`/`Mid`/`Low`, `LOD0`–`LOD3`) and per-texture material layers + Texture Browser.
  - [@GamingChelsea](https://github.com/GamingChelsea) (PRs #10, #18) — `audio.rpf` DAT metadata catalog & AWC auto-naming, AWC decode diagnostics, superblock stream splitting, and waveform audio player foundation.
  - [@Ahmet-Dedeler](https://github.com/Ahmet-Dedeler) ([Percy6Stars](https://github.com/Ahmet-Dedeler/Percy6Stars), PRs #6, #7, Issue #9) — macOS build & 3D viewer decoding, sorting, `RADTools.exe` fix, cutout/glass/decal blending, and licensing feedback.
  - [@paryzius](https://github.com/paryzius) ([ttpf-viewer](https://github.com/paryzius/ttpf-viewer)) — `.ttpf` reduced-mip fallback for unreadable mip 0 chunks.
  - [@holdmysocks](https://github.com/holdmysocks) ([SixNG](https://github.com/holdmysocks/SixNG)) — `0DRPz` (`0x7a`) map placement schema for world entity streaming.
- **Bug Reports & Feature Suggestions (Issues):**
  - [@YellowMaker](https://github.com/YellowMaker) (#20 — 2D world map orientation), [@adamjuankili](https://github.com/adamjuankili) (#17 — GPU World `m_vb` VRAM crash), [@crexlight](https://github.com/crexlight) (#16 — police dispatch `.awc` sample rate), [@Bluredguy](https://github.com/Bluredguy) (#15 — `binkplay.exe` path lookup), [@rbence05](https://github.com/rbence05) (#14 — PSIN placed scene assembly), [@kotelkonrad](https://github.com/kotelkonrad) (#11 — keyboard/arrow navigation across Explorer, models, audio & video), [@CedarCoder1](https://github.com/CedarCoder1) (#5 — copy file names & paths), [@Mrsuss60](https://github.com/Mrsuss60) (#4 — sort by name & size), [@adastmin1](https://github.com/adastmin1) (#2, #3 — multi-language & audio/text feedback).

## Scope, Clean-Room Research & Legal Notice

PercyStar is an **independent, clean-room binary inspection and parser tool** built from scratch in Rust. It only parses **unencrypted, user-supplied files** (`KB2*` streams, `ADAT` audio, plain `zlib` XML/PSIN containers, and standard uncompressed meshlet vertex/index blocks) for research and interoperability (cf. 17 U.S.C. § 1201(f)):

- **Zero Circumvention / Zero Keys:** Contains **no** decryption routines, encryption keys, hashes, DRM bypasses, or access-control workarounds. Encrypted data is skipped untouched.
- **Zero Proprietary Content:** Ships **no** third-party binaries, copyrighted models, textures, audio, shaders, or game assets. Users are solely responsible for supplying archives they legally own or are authorized to inspect.
- **Independent Trademark Disclaimer:** PercyStar is an independent open-source project and is not affiliated with, endorsed by, or sponsored by any game studio or publisher.

**Copyright (c) 2026 rpatty-dev — All Rights Reserved.**  
See [LICENSE](LICENSE). You may download, run, and fork this repository on GitHub for personal use/reference, **but you may not modify, alter, or redistribute modified versions of the code or binaries without explicit written permission.**
