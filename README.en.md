# HF Model Downloader (egui)

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

- **Version**: `0.1.0` (from `Cargo.toml`, read at runtime via `CARGO_PKG_VERSION`)
- **Author**: lordquest@163.com

Accessible any time from the top-bar right-click menu or the "About" button.

---

## Technical Notes

- GUI: [egui](https://github.com/emilk/egui) + [eframe](https://github.com/emilk/egui) 0.29 (`egui_glow` / OpenGL backend, immediate mode).
- Network: [reqwest](https://github.com/seanmonstar/reqwest) 0.12 using `native-tls` (system Schannel on Windows — no extra TLS library bundled).
- Download threads send progress to the UI thread via `std::sync::mpsc`.
- Chinese text relies on loading `C:\Windows\Fonts\msyh.ttc` (Microsoft YaHei) at runtime. On a non-Chinese Windows without a bundled CJK font, Chinese glyphs will be missing — you would need to bundle a font.

> Note: This project is optimized for Windows (static CRT via `+crt-static`). It can compile on Linux / macOS, but some paths (default download dir, font) would need adjustments.

---

## License

For learning and personal use only. HF content is subject to its Terms of Service and the license of each respective model.
