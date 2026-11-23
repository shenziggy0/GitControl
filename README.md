# GitControl

macOS Git 工作台。左侧切换项目，中间查看工作区、提交差异或 Claude 对话，右侧查看带本地和云端分支标签的提交图。

## 安装与使用

1. 在“关于本机”查看芯片，Apple M 系列选择 `GitControl_0.1.0_aarch64.dmg`，Intel 选择 `GitControl_0.1.0_x64.dmg`。打开镜像，将 GitControl 拖到“应用程序”。
2. 从 Finder 打开 GitControl，点击资源管理器的 **＋** 选择本地仓库。应用使用本机 Git 与已有的远端凭据。
3. 点击分支位置表里的提交号，在右侧提交图定位。点提交行查看文件，再点文件打开差异。
4. 用右上角按钮打开 Claude 对话或终端。Claude 依赖本机已登录的 Claude Code，可在聊天页的设置按钮指定可执行路径。
5. 在工作区勾选改动后，可对 Claude 说「提交选中的改动」。也可说「提交已暂存的改动」。提供说明时可以写「提交选中的改动，提交说明：更新截图配置」。

安装包要求 macOS 13 或更高版本，使用本地临时签名。通过网络下载后，系统可能要求在“隐私与安全性”中允许打开。镜像内附中文安装说明。

## 开发

```sh
npm ci
npm run tauri -- dev
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri -- build --bundles app -- --locked
python3 scripts/package_macos.py --app src-tauri/target/release/bundle/macos/GitControl.app --output-dir release/v0.1.0
```

本地构建需要 Node.js 22.12+、Rust 和 macOS Command Line Tools。上述命令打包本机架构，DMG 位于 `release/v0.1.0/`。

## 在 GitHub 手动打包和发布

1. 要发布新版本，先更新项目版本号（`package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`），同步 npm 和 Cargo 锁文件，并推送准备发布的提交。
2. 打开 [Actions 打包页面](https://github.com/shenziggy0/GitControl/actions/workflows/package.yml)，点 **Run workflow**，选择要打包的分支并运行。
3. 两个构建任务成功后，在该次运行的 **Artifacts** 下载两个压缩包。解压得到 Apple Silicon 和 Intel 各一个 `.dmg`。
4. 打开 [新建 Release](https://github.com/shenziggy0/GitControl/releases/new)，创建或选择对应版本标签（如 `v0.1.0`），确认标签指向本次打包的提交。填写说明，上传两个 `.dmg`，再点 **Publish release**。

[桌面架构](docs/design/0001-GitControl-macOS桌面架构.md) · [七阶段实施方案](docs/design/0002-macOS首版实施方案.md)
