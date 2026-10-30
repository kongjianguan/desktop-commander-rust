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

## 终端管理（替代 Flutter 控制面板）

在一个 Termux 会话启动 Agent（或使用自己的 Termux:Boot / KernelSU 启动脚本）；在另一个会话管理同一个后台实例。
除 `start` 负责启动进程外，其余管理命令只访问本机的认证控制接口，不会占用第二个实例，也不会泄露本机 Bearer Token。
`start` 的 stderr 日志位于配置目录中的 `agent.log`（私有权限）；Android 仍可能受后台进程限制杀掉 Agent，可靠保活需要单独处理。

```sh
# 一条命令启动后台 Agent
desktop-commander start
desktop-commander status
# 也可以用 desktop-commander remote --headless 在前台运行
# 在另外一个 Termux 会话使用以下命令管理
desktop-commander status --json
desktop-commander connect
desktop-commander disconnect
desktop-commander events --limit 10
desktop-commander events --json
desktop-commander diagnostics

# 修改原本在 Flutter 界面里的选项（无需重启服务）
desktop-commander settings show
desktop-commander settings show --json
desktop-commander settings get deviceName
desktop-commander settings set deviceName 'Android Phone'
desktop-commander settings set proxy 'socks5://127.0.0.1:10808'
desktop-commander settings set autoConnect on
desktop-commander settings set tools.defaultShell "$PREFIX/bin/bash"
desktop-commander settings set tools.allowedDirectories '["/data/data/com.termux/files/home"]'

# 账号与生命周期
desktop-commander logout   # 清除本机设备授权，不等于云端撤销设备
desktop-commander quit     # 退出整个后台进程
```

`settings show/get/set` 在 Agent 尚未启动时也能操作配置文件；如果 Agent 已经运行，则通过本机
`127.0.0.1` 的 Bearer-auth API 更新设置。布尔值接受 `true/false`、`on/off` 等，
数组要求 JSON 数组格式；修改在线配置会使用后台已有的设置校验及连接重启逻辑。
`status --json`、`events --json`、`settings show --json` 可供 Agent/脚本消费。

`startAtLogin` 和 `preventSleep` 为 Android 不支持的桌面语义，设置为 `true` 会被拒绝，
请通过 Termux:Boot、KernelSU 和 Termux:API 管理。

## 功能与限制

- Rust 保留原本 **26 个 MCP 工具接口**；截至 2026-10-29，Android 16 实机已验证其中 24 项可调用工具。
- 文件、终端、进程、DOCX、XLSX 和图片功能均不需要 Debian。
- Android 版 ripgrep 随包构建；如仅复制主 ELF，也可以在 Termux 执行 `pkg install ripgrep`。
- PDFium 为 Android ARM64 专用 `libpdfium.so`，不能使用桌面 Linux 版。
- PDF **生成**依赖 Android 版 Typst。最简单的安装方式是 `pkg install typst`；
  Termux 主仓库的 ARM64 Typst 0.15.1 已在 Android 16 实机与此项目的 `cmarker` 模板配合测试通过（中文 PDF 创建、PDFium 回读）。
  该可执行文件仅在生成 PDF 时启动，不增加 Agent 常驻内存。发行包中的 `manifest.json`
  可能显示 `features.typstBundled=false`，但只要 `$PREFIX/bin/typst` 可执行，仍可通过 `PATH` 回退正常生成。
  PDF 阅读与修改只依赖随包提供的 PDFium。
- 这不是 Android APK：没有桌面系统托盘或 Flutter 管理窗口。
- Android 后台持久运行不由二进制自动保证。休眠控制可由 Termux:API 的 `termux-wake-lock` 完成；开机启动可由 Termux:Boot 或 KernelSU 服务完成，但这两者都未自动启用。
- 默认 shell 优先 `$PREFIX/bin/sh`，否则回退 `/system/bin/sh`。应保留 Termux 环境变量。

## Root 安全

**Agent 本体建议始终以普通 Termux 用户运行，仅在必要的命令里使用 `su -c`。**
例如通过 MCP 终端工具执行 `su -c id`，由 KernelSU 或 Magisk 负责授权。
整个远程 Agent 若以 root 身份启动，连接端即具有相应的任意命令执行权限；
`allowedDirectories` 和 `blockedCommands` 仅防止误操作，并不是安全沙箱。

## GitHub Actions 验证

工作流分为互相独立的并行任务：Android ARM64 Rust Agent、原生 ripgrep、PDFium/文档资源、
以及**可选的** Android Typst。最终打包任务仅等待前三项必需产物，通过 Actions Artifact
下载后执行 ELF 校验并生成 Termux tar.gz 和 SHA-256。Typst Job 不再编译复杂的 OpenSSL 依赖树：
它下载 Termux 官方主仓库的 aarch64 原生 Typst 0.15.1 .deb，验证**固定 SHA-256** 和 ELF 架构后输出
`desktop-commander-android-typst-arm64` 独立 Artifact，不会自动合并进主安装包。
如需离线安装，可把此附件的 `resources/bin/typst` 提取到已安装目录的同名路径；运行时依赖
Termux 的 `openssl` 包。在线设备直接运行 `pkg install typst` 更简单。可选附件失败不阻塞主安装包。
使用固定 NDK 与 Rust 1.90 交叉编译；工作流会检查 ELF 架构、Bionic 动态依赖与打包资源。
这些构建检查与 Android 实机 E2E 互相补充，实机测试记录列出已覆盖功能与未覆盖工具。

## Android 实机测试记录（2026-10-29）

在 Android 16 / Termux / KernelSU 设备上，26 个 MCP 工具中已有 24 项可调用工具实测通过；
另两项 `get_prompts`、`give_feedback_to_desktop_commander` 未暴露在当前远程 API，尚未测试。
验证覆盖文件写入与编辑、ripgrep 搜索与分页、XLSX 读写、DOCX 生成与 XML 修改、PDFium 读取/插页/删页、图片读取、终端交互/进程终止、CLI 生命周期、`su -c id`。
安装 Termux Typst 0.15.1 后，`write_pdf` 已成功生成含中文的 PDF 并经 PDFium 回读验证。
旧二进制读取 PDF/DOCX 时的 `structuredContent.fileType=unsupported` 属于元数据误标，
源码已修复为 `pdf`/`docx` 并附 MIME 类型；此修复需要安装**更新后的 Agent** 才能观察到。
