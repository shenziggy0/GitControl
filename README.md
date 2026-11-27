# GitControl

GitControl 是一个 macOS 桌面 Git 工作台。选中一个仓库，就能看到改了哪些文件、还有几次提交待推送，以及本地和远端分支分别停在哪次提交上。

![GitControl 工作台：左侧是项目和分支，中间是文件改动与分支位置，右侧是提交图](docs/images/workspace.png)

图中的 `main` 比 `origin/main` 领先一次提交。点中间「分支位置」里的提交号，可以在右侧图中定位；再点那次提交，就能查看改过的文件和逐行差异。

## 安装

需要 **macOS 13 或更高版本**和本机 Git。可以在终端运行 `git --version` 检查。Agent CLI 是可选依赖，可在使用问答时安装和登录。

1. 在苹果菜单  → **关于本机** 查看芯片。Apple M 系列选择文件名以 `_aarch64.dmg` 结尾的安装包，Intel 选择 `_x64.dmg`。
2. 发布版本后，从 **Assets** 下载对应的 DMG。若页面尚无版本，可以按下方「从源码运行」启动。[GitHub Releases](https://github.com/shenziggy0/GitControl/releases)
3. 打开 DMG，将 **GitControl** 拖到 **Applications**，推出镜像后，从「应用程序」启动。

当前安装包采用本地临时签名，未经过 Apple 公证。如果 macOS 提示无法验证开发者，确认安装包来自本仓库后，可在「系统设置 → 隐私与安全性」中选择「仍要打开」。[完整安装说明](docs/install/macOS安装说明.txt)

## 打开第一个仓库

点左侧「资源管理器」的 **＋**，选择本地 Git 仓库。如果多个项目放在同一个普通文件夹里，也可以直接选这个父目录，GitControl 会查找两层以内的子仓库，点仓库名即可打开。

| 想做什么 | 在哪里操作 |
| --- | --- |
| 看哪些改动已经暂存 | 在「工作台 → 工作区更改」查看已暂存、未暂存和未跟踪文件。点文件名查看差异。 |
| 确认本地和远端是否同步 | 点「刷新」，在「分支位置」对照两边的提交号。远端连接失败时，界面会标出错误和已有结果的核对时间。 |
| 找到某次提交 | 按 **⌘P** 输入分支名、提交说明或短提交号。搜索分支和提交默认限于当前仓库。切换「所有项目」可查已添加的仓库及已发现的子仓库。 |
| 看父目录下还有什么没处理 | 在父目录工作台查看每个仓库的改动文件数和待推送数。点「查看改动」或「查看待推送」进入对应详情。 |
| 查看并推送本地提交 | 点「待推送」的数字，核对分支、实际推送目标和提交列表，再确认推送。拉取、合并和首次发布分支可在终端完成。 |
| 记住改动前的工作树 | 打开「工作树快照」并点「记住现在」。选快照和文件可以预览恢复结果，恢复后也能从对应版本撤回。 |
| 在项目目录里运行命令 | 点窗口下方的「终端」。切换项目后再回来，可以接着使用原来的 shell。 |

### 查看差异并提交

在「工作区更改」勾选要提交的改动，或在差异页点「加入本次提交」。填写中文说明后点击「提交」。同一个文件有已暂存和未暂存改动时，提交区会显示所选组数。

差异会显示旧版与新版行号。点击行号可查该行来自哪次提交。需要原始 Git 补丁时，点「原始补丁」。

可以让 Agent 为所选改动生成说明草稿，或解释某一行以及所在提交。也可以在 Agent 问答里输入「提交选中的改动，提交说明：更新安装文档」或「提交已暂存的改动」，由 GitControl 执行本地提交。

### 使用 Agent 问答

GitControl 打开时会自动查找本机已安装的 Agent CLI。在「Agent 问答」顶部选择工具，应用会记住上次选择。各 CLI 沿用本机登录。

检测名单参照 CC Switch 4.0.43 的 CLI，并额外支持 Cursor Agent。问答支持 Claude Code、Codex、Antigravity、Pi、CodeBuddy、Cursor、Gemini CLI、Grok Build、OpenCode、OpenClaw、MiniMax Code、Kimi Code、Oh My Pi 和 GitHub Copilot CLI 的兼容版本。Hermes Agent、DeepSeek Harness 和缺少问答所需命令的旧版本可在 GitControl 的终端中运行。

Antigravity 需要重新登录时，可以在问答中点「登录 Antigravity」。在浏览器的 Google 页面复制授权码，回到 GitControl 的登录窗口，点击下方区域，按 **⌘V** 粘贴，再按回车。登录成功后，GitControl 会把原问题放回输入框。

切换后，下一次提问会开启新会话。之前的回答仍可在当前窗口查看。Antigravity 在临时目录接收 GitControl 提供的项目资料。

## 从源码运行

准备 Node.js 22.12+、npm、Rust 和 macOS Command Line Tools。在仓库根目录运行：

```sh
npm ci
npm run tauri -- dev
```

运行仓库测试：

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

## 维护者手动打包与发布

1. **准备标签。** 将发布提交推送到 GitHub，再为该提交创建并推送三段数字版本的标签，例如 `0.1.1` 或 `v0.1.1`。
2. **按标签打包。** 手动运行 `gh workflow run package.yml --ref 0.1.1`，将 `0.1.1` 换成实际标签。Apple Silicon 和 Intel 会分别构建。标签的数字版本会写入应用并用于命名 DMG。按分支运行时使用 `src-tauri/tauri.conf.json` 中的版本。[打开打包工作流](https://github.com/shenziggy0/GitControl/actions/workflows/package.yml)
3. **取出安装包。** 在本次运行的 **Artifacts** 下载两个 ZIP，分别解压，得到 `GitControl_<版本>_aarch64.dmg` 和 `GitControl_<版本>_x64.dmg`。
4. **手动发布。** 选择用于打包的标签，填写更新说明，上传两个 DMG，确认后点 **Publish release**。[新建 Release](https://github.com/shenziggy0/GitControl/releases/new)

如需在自己的 Mac 上打包当前芯片的版本，可在安装依赖后运行：

```sh
npm run tauri -- build --bundles app -- --locked
python3 scripts/package_macos.py \
  --app src-tauri/target/release/bundle/macos/GitControl.app \
  --output-dir release
```
