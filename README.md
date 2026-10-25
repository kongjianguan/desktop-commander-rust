# Desktop Commander Rust

Rust 远程工具服务与按需启动的 Flutter 管理界面。

## 运行

下载 GitHub Actions 对应平台的完整发行包，解压后运行 `desktop-commander remote`。
通过状态栏菜单打开管理界面、连接或停止远程服务。关闭管理窗口后，Rust 继续运行。

服务器使用单独提供的 `desktop-commander-headless` 压缩包。该程序默认以无界面方式运行，Linux 程序不依赖 GTK。

```sh
desktop-commander remote --headless
desktop-commander stdio
desktop-commander gui
desktop-commander logout
desktop-commander --config-dir /absolute/path remote --headless
```

首次连接通过现有 Desktop Commander 服务完成设备授权。配置与凭据保存在系统用户配置目录中的独立目录，旧程序的配置保持完整。
使用 `DC_CONFIG_DIR` 可以指定配置目录。代理读取 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`，也可以在管理界面填写代理地址。

## 功能

保留终端、进程、文件编辑、搜索、DOCX、XLSX、PDF 和图片处理工具，以及本地 stdio MCP。
PDF 生成使用 Typst，接受 Markdown。`options` 支持 `pageWidth`、`pageHeight`、`margin`（单位为 `mm`）及 `font`。
PDF 操作支持插入和删除页面。原生依赖随发行包提供，无需安装 Node.js。

## 构建与验证

GitHub Actions 编译 macOS ARM64、macOS x64、Windows x64 和 Linux x64 的 Rust、Flutter 程序。
每个平台通过真实 stdio MCP 完成文件、终端、搜索与文档操作，并通过 Flutter integration_test 操作真实 Rust 管理接口。
验证文件、脱敏日志、SHA-256 和内存采样报告保存在构建产物内。

后台在线空闲内存的验收目标为 50 MiB。离线 E2E 的内存采样单独标记，不能替代在线验证。
实际服务授权、恢复和长时间在线测试在下载的程序上执行，凭据留在本机。

## 来源与许可证

工具接口、MCP 网页资源和提示内容来自 [DesktopCommanderMCP](https://github.com/wonderwhy-er/DesktopCommanderMCP)，采用 MIT 许可证。
第三方原生程序、字体和插件的许可证随发行包提供。
