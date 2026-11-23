# GitControl

macOS Git 工作台。左侧切换项目，中间查看工作区、提交差异或 Claude 对话，右侧查看带本地和云端分支标签的提交图。

## 安装与使用

1. 打开 `release/GitControl_0.1.0_aarch64.dmg`，将 GitControl 拖到“应用程序”。
2. 从 Finder 打开 GitControl，点击资源管理器的 **＋** 选择本地仓库。应用使用本机 Git 与已有的远端凭据。
3. 点击分支位置表里的提交号，在右侧提交图定位。点提交行查看文件，再点文件打开差异。
4. 用右上角按钮打开 Claude 对话或终端。Claude 依赖本机已登录的 Claude Code，可在聊天页的设置按钮指定可执行路径。
5. 在工作区勾选改动后，可对 Claude 说「提交选中的改动」。也可说「提交已暂存的改动」。提供说明时可以写「提交选中的改动，提交说明：更新截图配置」。

本安装包面向 Apple Silicon，使用本地临时签名。通过网络另行分发时，系统可能要求在“隐私与安全性”中允许打开。已验证的系统以随发布包的验收记录为准。

## 开发

```sh
npm ci
npm run tauri -- dev
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri -- build --bundles app,dmg
```

启动依赖 Node.js、Rust 和 macOS Command Line Tools。构建产物位于 `src-tauri/target/release/bundle/`。

[桌面架构](docs/design/0001-GitControl-macOS桌面架构.md) · [七阶段实施方案](docs/design/0002-macOS首版实施方案.md)
