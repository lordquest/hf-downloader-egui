# HF Model Downloader (egui)

> 🇬🇧 English documentation: [README.en.md](README.en.md)

[![Latest Release](https://img.shields.io/github/v/release/lordquest/hf-downloader-egui?label=release)](https://github.com/lordquest/hf-downloader-egui/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/lordquest/hf-downloader-egui/total)](https://github.com/lordquest/hf-downloader-egui/releases)

**下载 / Download：** [hf-downloader-egui.exe（Windows，约 4.8 MB）](https://github.com/lordquest/hf-downloader-egui/releases/latest/download/hf-downloader-egui.exe)

一个用于下载 [Hugging Face](https://huggingface.co) 模型 / 数据集仓库的**原生桌面工具**，无需安装 Git、Git LFS 或任何运行时依赖。

A native desktop tool for downloading Hugging Face model/dataset repos — no Git, no Git LFS, no runtime dependencies required.

---

## 特性 / Features

- **免 LFS 下载**：直接通过 HF 文件 API 下载仓库内任意文件，不必安装 Git LFS。
- **灵活选择**：粘贴 `owner/repo` 或完整 HF 链接 → 列出文件 → 全选 / 全不选 / 勾选单个文件。
- **并行下载**：多文件同时下载，实时显示进度、速度、状态，支持**取消**与**重试**。
- **HF Token 登录**：支持填写访问令牌，用于下载私有仓库与 gated（受限）仓库。
- **镜像 / 加速节点**：可填可选镜像地址（如 `https://hf-mirror.com`），方便网络受限环境。
- **外置语言文件**：界面默认中文 / 英文，自动跟随系统语言；语言以 `lang/*.json` 外置，**用户可自己翻译新增语言**。
- **单文件零依赖**：编译为单个约 4.8 MB 的 exe，静态链接 CRT，使用系统 Schannel 做 TLS，仅依赖 Windows 内置 DLL，无需安装 VC++ 运行库。
- **关于窗口**：顶栏「关于」按钮（或右键顶栏）可查看版本与作者。

---

## 快速开始 / Quick Start

### 运行（已构建版本）/ Run (prebuilt)

直接双击运行：

```
target/release/hf-downloader-egui.exe
```

首次启动会在 exe 同级目录生成 `lang/` 文件夹（含 `en.json`、`zh.json`）。

### 从源码构建 / Build from source

需要安装 [Rust](https://www.rust-lang.org/) 工具链（Windows 建议 `x86_64-pc-windows-msvc`）。

```bash
cargo build --release
```

构建过程由 `build.rs` 自动把 `lang/*.json` 复制到 exe 同级目录。`lang/` 已被 `.gitignore` 忽略（属于构建产物），源码中的语言文件位于仓库根 `lang/`。

---

## 使用说明 / Usage

1. 在输入框填写仓库，例如 `meta-llama/Llama-2-7b` 或 `https://huggingface.co/meta-llama/Llama-2-7b`。
2. 点击 **List Files** 获取文件列表。
3. 勾选要下载的文件（或 **Select All** / **Select None**）。
4. 点击 **Start Download** 开始；可在「File List」面板查看每个文件的进度与速度，并支持取消 / 重试。
5. 下载目录默认是**程序所在目录**（exe 同级），可在「Settings」中修改。
6. 下载私有 / gated 仓库：点击 **Login HF** 填入 token（设置里也可看到登录状态）。

---

## 语言 / Localization

界面文本存放在 exe 同级 `lang/<语言代码>.json`，例如 `zh.json`、`en.json`。

- 程序内置中文、英文作为兜底；启动时加载外部 `lang/*.json` 并覆盖 / 补充。
- 缺失的 key 会自动回退到英文，因此翻译不必一次性补全。

**新增一种语言 / Add a new language：**

1. 复制 `en.json` 为 `lang/<代码>.json`（代码如 `ja`、`ko`、`fr`…）。
2. 把 `__name__` 改成该语言的母语名称（用于顶栏下拉菜单显示），其余键值翻译为你自己的语言。
3. 重启程序即可在顶栏语言下拉菜单中看到并切换。

无需重新编译。

---

## 关于 / About

- **版本 / Version**：`0.1.0`（见 `Cargo.toml`，运行时从 `CARGO_PKG_VERSION` 读取）
- **作者 / Author**：lordquest@163.com

顶栏右键菜单或「关于」按钮可随时查看。

---

## 技术说明 / Technical Notes

- GUI：[egui](https://github.com/emilk/egui) + [eframe](https://github.com/emilk/egui) 0.29（`egui_glow` / OpenGL 后端，即时模式）。
- 网络：[reqwest](https://github.com/seanmonstar/reqwest) 0.12，使用 `native-tls`（Windows 上走系统 Schannel，不额外打包 TLS 库）。
- 下载线程通过 `std::sync::mpsc` 把进度发送给 UI 线程。
- 中文显示依赖运行时加载 `C:\Windows\Fonts\msyh.ttc`（微软雅黑）；在非中文 Windows 上若未内置中文字体，中文会显示为缺字，需自行打包字体。

> 注：本项目面向 Windows 优化（静态 CRT、`+crt-static`）。在 Linux / macOS 上可编译，但部分路径（如下载目录默认、字体）需相应调整。

---

## 许可证 / License

本项目仅供学习与个人使用。Hugging Face 相关内容请遵守其服务条款与对应模型的许可协议。
