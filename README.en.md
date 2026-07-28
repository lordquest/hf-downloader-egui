# HF Model Downloader (egui)

> 🇨🇳 中文文档：[README.md](README.md)

[![Latest Release](https://img.shields.io/github/v/release/lordquest/hf-downloader-egui?label=release)](https://github.com/lordquest/hf-downloader-egui/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/lordquest/hf-downloader-egui/total)](https://github.com/lordquest/hf-downloader-egui/releases)
[![License: MIT](https://img.shields.io/github/license/lordquest/hf-downloader-egui)](LICENSE)

**Download:** [hf-downloader-egui.exe (Windows, ~4.8 MB)](https://github.com/lordquest/hf-downloader-egui/releases/latest/download/hf-downloader-egui.exe)

A native desktop tool for downloading files from [Hugging Face](https://huggingface.co) model / dataset repositories — no Git, no Git LFS, and no runtime dependencies required.

---

## Features

- **No LFS needed**: Downloads individual files directly through the HF file API, so Git LFS does not have to be installed.
- **Flexible selection**: Paste an `owner/repo` id or a full HF URL → list files → select all / none / individual files.
- **Parallel downloads**: Multiple files download at once, with live progress, speed and status per file. **Cancel** and **Retry** are supported.
- **HF token login**: Paste an access token to download private and gated repositories. Login status is shown in the top bar.
- **Mirror / endpoint**: An optional mirror base URL (e.g. `https://hf-mirror.com`) can be set for restricted networks.
- **External language files**: The UI ships in Chinese and English and auto-detects the system language. Strings live in `lang/*.json` so **users can translate and add languages themselves**.
- **Single-file, zero-dependency**: Compiles to a single ~4.8 MB exe with a statically linked CRT. TLS uses the system Schannel, so only built-in Windows DLLs are required — no VC++ redistributable needed.
- **About window**: A top-bar "About" button (or right-click the top bar) shows the version and author.

---

## Quick Start

### Run (prebuilt)

Just double-click:

```
target/release/hf-downloader-egui.exe
```

On first launch a `lang/` folder (containing `en.json` and `zh.json`) is created next to the exe.

### Build from source

You need the [Rust](https://www.rust-lang.org/) toolchain (on Windows, `x86_64-pc-windows-msvc` is recommended).

```bash
cargo build --release
```

During the build, `build.rs` automatically copies `lang/*.json` next to the exe. `lang/` is listed in `.gitignore` (it is a build artifact); the source language files live at the repo root under `lang/`.

---

## Usage

1. Enter a repo in the input box, e.g. `meta-llama/Llama-2-7b` or `https://huggingface.co/meta-llama/Llama-2-7b`.
2. Click **List Files** to fetch the file list.
3. Tick the files you want (or use **Select All** / **Select None**).
4. Click **Start Download**. The "File List" panel shows per-file progress and speed, with cancel / retry support.
5. The download directory defaults to **the directory the exe lives in** (next to the exe). Change it in **Settings**.
6. For private / gated repos: click **Login HF** and paste your token (login status is also visible in Settings).

---

## Localization

UI strings are stored in `lang/<code>.json` next to the exe, e.g. `zh.json`, `en.json`.

- Chinese and English are embedded as fallbacks. At startup the external `lang/*.json` files are loaded and override / extend them.
- Any missing key falls back to English, so a translation does not have to be complete up front.

**Add a new language:**

1. Copy `en.json` to `lang/<code>.json` (code like `ja`, `ko`, `fr`…).
2. Change `__name__` to the language's native name (shown in the top-bar dropdown); translate the other keys.
3. Restart the app — the new language appears in the top-bar language dropdown.

No recompile needed.

---

## About

- **Version**: `0.2.3` (from `Cargo.toml`, read at runtime via `CARGO_PKG_VERSION`)
- **Author**: lordquest@163.com

Accessible any time from the top-bar right-click menu or the "About" button.

---

## Technical Notes

- GUI: [egui](https://github.com/emilk/egui) + [eframe](https://github.com/emilk/egui) 0.29 (`egui_glow` / OpenGL backend, immediate mode).
- Network: [reqwest](https://github.com/seanmonstar/reqwest) 0.12 using `rustls-tls` (pure-Rust TLS, no system OpenSSL / Schannel dependency), with bundled CA roots — a single self-contained executable with zero system dependencies.
- Download threads send progress to the UI thread via `std::sync::mpsc`.
- Chinese text: a CJK font is picked per platform (Windows uses the system Microsoft YaHei; Linux/macOS use system Noto / WenQuanYi / PingFang, etc.). You can also drop any `.ttf/.ttc/.otf` into the `fonts/` directory next to the exe and it will be used first.
- Static CRT: the Windows build statically links the C runtime via `+crt-static` in `.cargo/config.toml`, so no VC++ redistributable is needed.

## Platforms

| Platform | Status | Notes |
| --- | --- | --- |
| Windows | ✅ recommended | single exe, double-click to run |
| Linux | ✅ supported | needs a graphical environment (X11/Wayland + OpenGL); the CI tarball already bundles a CJK font so Chinese renders after extraction |
| macOS | ✅ supported | needs a graphical environment; CI produces a universal binary (Intel + Apple Silicon) tarball — extract and run (unsigned, so on first launch you may need `xattr -cr hf-downloader-egui` in Terminal) |

> Note: pushing a `v*` tag to GitHub triggers CI that produces the Windows exe, a Linux tarball, and a macOS universal-binary tarball (see `.github/workflows/release.yml`).

---

## License

This project is open source under the **MIT License** — see [LICENSE](LICENSE).

Content from Hugging Face is still subject to its Terms of Service and the license of each respective model.
