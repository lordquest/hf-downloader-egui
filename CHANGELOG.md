# Changelog

All notable changes to HF Model Downloader (egui) are documented here.

## v0.2.2 (2026-07-28)

Download reliability and resume fixes.

### Fixed
- **断点续传在「取消后继续」时失效**：任务的 `cancelled` 标志只置位、从不复位，导致取消后再点「继续下载」会立刻被判定为已取消而退出，文件永远停在半途。现在 `start` / `retry` 会正确清零该标志，续传真正可用。
- **切换镜像站点不生效**：复用已有任务时忽略新的 endpoint / 下载目录，切到 `https://hf-mirror.com` 再继续仍走旧地址。现在 `get_or_create` 会同步最新 endpoint、下载目录与 revision。
- **「重试」按钮点击无反应**：`engine.retry` 先把文件状态设为 `Pending`，而 `start_downloads` 会跳过所有 `Pending` 文件，导致重试什么都不做。现在 `retry` 不再预置 `Pending`，交由 `start_downloads` 复位 `Failed/Cancelled` 并真正派发下载；同时 `retry` 会写入当前 endpoint / 下载目录，使「停止 → 切镜像 → 重试」真正走新镜像续传。

### Improved
- **下载速度更稳定**：单文件在遇瞬时网络错误 / 超时（5xx、429、连接错误）时自动重试最多 3 次（退避 1 / 2 / 5 秒），每次从磁盘已下载字节处续传，网络抖动不再直接整文件失败。

### Tests
- 新增本地自动化测试（手搓 HTTP/1.1 + Range 服务器）覆盖：整文件下载、取消后续传、镜像 endpoint 切换生效、瞬时错误自动重试、`retry` 重试取消文件并应用新 endpoint。

## v0.2.1 (2026-07-27)

- 所有输入框加黑边、输入内容自动裁剪前后空格。
- 设置页两个输入框宽度上限 420px。
- 新增 macOS CI，产出 Intel + Apple Silicon 通用二进制 tar 包。
- 跨平台 CJK 字体探测 + 内置 Noto Sans CJK 字体（Linux / macOS 自包含中文显示）。
- 使用 rustls-tls（纯 Rust TLS，零系统依赖）。

## v0.2.0 (2026-07-27)

- 从 Tauri v2 重写为 egui + eframe 原生桌面应用，单文件零依赖 exe。
- 静态链接 CRT（Windows 无需 VC++ 运行库）。
- 外置 `lang/*.json` 多语言，自动跟随系统语言，用户可自行翻译新增语言。
- 支持 HF Token 登录、镜像 / 加速节点、并行下载、取消与重试。

## v0.1.0 (2026-07-26)

- 初始版本：列文件、勾选下载、进度与速度显示。
