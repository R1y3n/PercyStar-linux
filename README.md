# PercyStar-linux

Linux fork of [rpatty-dev/PercyStar](https://github.com/rpatty-dev/PercyStar).

This repo keeps the original PercyStar codebase and adds the changes needed to build and use it on Linux, plus a small CLI scan mode. The original project, format research, and GUI/features are by [@rpatty-dev](https://github.com/rpatty-dev).

## What changed in this fork

- Added a GitHub Actions workflow to build a Linux x64 release binary (`.github/workflows/linux-build.yml`, built on `ubuntu-22.04` for better glibc compatibility).
- Fixed Linux file dialogs by enabling the `rfd` GTK3 backend on Linux.
- Disabled the startup update check and in-app update UI so the build runs offline.
- Improved RPF scan performance by skipping already-identified zlib offsets during the second pass.
- Reduced idle model-viewer RAM by releasing CPU raster buffers when the mesh is not animating.
- Fixed a scan regression from earlier perf work so second-pass entries (XML/RBF0/GXT/PSIN/DAT/ZLIB/bin, etc.) are discovered again for <=512MB archives.
- Added a CLI scan mode for searching archives without launching the GUI.

## Download

Linux builds are published on the [Releases](https://github.com/R1y3n/PercyStar-linux/releases) page. Download `PercyStar-linux-x64.tar.gz`, extract it, and run `./PercyStar`.

## Build on GitHub

The workflow builds on `ubuntu-22.04` and uploads `PercyStar-linux-x64.tar.gz` as an artifact. You can also run it manually from the Actions tab.

## CLI scan

The same binary has a no-GUI scan mode:

```bash
./PercyStar --scan /path/to/archive.rpf
./PercyStar --scan /path/to/archive.rpf --name "asset_name"
./PercyStar --scan /path/to/archive.rpf --name "asset_name" --type MODEL --limit 20
./PercyStar --scan /path/to/folder --name "asset_name" --json
```

Options:

- `--name PAT` - case-insensitive substring match on asset name
- `--type KIND` - match entry kind (for example `MODEL`, `VEH`, `ZLIB`, `RBF0`, `AWC`)
- `--min-size N` / `--max-size N` - size filter (`bytes`, or `KB`/`MB`/`GB`)
- `--limit N` - stop after N matches
- `--json` - print one JSON object per match

## Notes

- This is a Linux-focused fork; the original repo has the full Windows/macOS release context and project history.
- GUI file dialogs on Linux use GTK3.
- The Linux build is produced by CI; I didn't install Rust on the local machine used to set this up.

## License

See [LICENSE](LICENSE). Original project and attribution belong to the upstream PercyStar repo.
