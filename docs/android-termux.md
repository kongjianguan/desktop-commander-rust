# Android ARM64 / Termux（原生 Headless）

> 实验性移植。所有 Android 编译和打包都在 GitHub Actions 完成，Mac/Termux 不承担构建。
> 目标产物为 Android Bionic ELF (`aarch64-linux-android`)，不需要 Debian、PRoot、chroot、Node.js、Flutter 或 APK。

## 下载与运行

从 GitHub Actions 的 **Android ARM64 (Termux)** 工作流下载 `desktop-commander-android-arm64` artifact，取出其中的 `.tar.gz` 后放在 Termux 私有目录中。不要直接在 `/sdcard` 中执行。

```sh
pkg install tar
tar -xzf desktop-commander-*-android-arm64.tar.gz
cd desktop-commander-*-android-arm64
sh ./install-termux.sh
desktop-commander --version
RUST_LOG=info desktop-commander remote --headless
```

首次连接按照终端里显示的授权网址及设备验证码进行绑定。
默认配置：`$HOME/.config/desktop-commander-rust`，可通过 `--config-dir` / `DC_CONFIG_DIR` 覆盖。
也可以跳过安装脚本，直接在提取目录运行 `./desktop-commander remote --headless`。

安装脚本将主 ELF 和同目录下的原生资源放到 `$HOME/.local/share/desktop-commander-rust`，
并在 `$PREFIX/bin/desktop-commander` 建立启动包装器，不会请求 Root 或安装后台服务。

## 功能与限制

- Rust 保留原本 **26 个 MCP 工具接口**；完整运行行为仍待真实 Android 设备端对照 E2E。
- 文件、终端、进程、DOCX、XLSX 和图片功能均不需要 Debian。
- Android 版 ripgrep 随包构建；如仅复制主 ELF，也可以在 Termux 执行 `pkg install ripgrep`。
- PDFium 为 Android ARM64 专用 `libpdfium.so`，不能使用桌面 Linux 版。
- PDF **生成**依赖 Android 版 Typst。CI 尝试交叉编译，但这个步骤不阻止基础包产出：
  检查 `manifest.json` 的 `features.typstBundled`。若为 `false`，需自行提供 Termux 中可运行的 `typst` 才能创建 PDF；PDF 阅读与修改仍依赖随包提供的 PDFium。
- 这不是 Android APK：没有桌面系统托盘或 Flutter 管理窗口。
- Android 后台持久运行不由二进制自动保证。休眠控制可由 Termux:API 的 `termux-wake-lock` 完成；开机启动可由 Termux:Boot 或 KernelSU 服务完成，但这两者都未自动启用。
- 默认 shell 优先 `$PREFIX/bin/sh`，否则回退 `/system/bin/sh`。应保留 Termux 环境变量。

## Root 安全

**Agent 本体建议始终以普通 Termux 用户运行，仅在必要的命令里使用 `su -c`。**
例如通过 MCP 终端工具执行 `su -c id`，由 KernelSU 或 Magisk 负责授权。
整个远程 Agent 若以 root 身份启动，连接端即具有相应的任意命令执行权限；
`allowedDirectories` 和 `blockedCommands` 仅防止误操作，并不是安全沙箱。

## GitHub Actions 验证

工作流使用固定 NDK 与 Rust 1.90 编译 Android ARM64 ELF、原生 ripgrep，
尝试构建 Typst，并打包 PDFium/模板/字体及 SHA-256。验证脚本仅检查 ELF 架构、
Bionic 动态依赖与打包资源，**不是 Android 手机上的真实运行时测试**。
网络授权、动态加载、Termux 权限、16 KiB page size 和 26 项工具的设备端 E2E 尚需实测。
