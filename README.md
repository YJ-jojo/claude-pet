# Claude 宠物 · Claude Desktop Pet

一只住在 Windows 桌面上的小宠物，背后是你自己的 Claude Code（走 Claude 订阅额度，不另外花 API 的钱）。
A small desktop pet for Windows, powered by your own Claude Code (uses your Claude subscription, no separate API billing).

## 能做什么 · What it does

| 中文 | English |
|---|---|
| Claude Code 要你确认、停下等你时，叮一声并说出来 | Chimes and speaks when Claude Code needs a confirmation or is waiting for you |
| 看一眼当前窗口，讲给你听；呼吸模式下一直盯着，有变化就讲 | Looks at the current window and explains it; "breathing mode" keeps watching and speaks when something changes |
| 跑过去指按钮、帮你点和打字（不能撤销的操作先问你） | Runs to a button to point at it, or clicks and types for you (asks first before anything irreversible) |
| 框选翻译，中英对照 | Box-select translation, Chinese ⇄ English |
| 接入 Claude 桌面版：桌面版的聊天能让宠物看屏幕、指按钮、帮你操作 | Claude Desktop integration: chats in Claude Desktop can ask the pet to look, point, click and type |

## 安装 · Install

1. 在 [Releases](../../releases/latest) 下载 `ClaudePet-<版本>-windows.zip`，解压到固定的文件夹，双击 `ClaudePet.exe`。
   Download `ClaudePet-<version>-windows.zip` from [Releases](../../releases/latest), unzip it to a permanent folder and run `ClaudePet.exe`.
2. 第一次运行时设置页会帮你一键安装、登录 Claude Code。
   On first run the settings page installs and signs in Claude Code for you.

详细说明见 [docs/使用说明.md](docs/使用说明.md)，人物形象包格式见 [docs/形象包格式.md](docs/形象包格式.md)。
Full guide (Chinese): [docs/使用说明.md](docs/使用说明.md). Sprite pack format: [docs/形象包格式.md](docs/形象包格式.md).

发布里不带人物形象包，只带一个简单的占位形象。
Releases ship only a simple placeholder character; character sprite packs are not included.

## 编译 · Build

Rust + Tauri 2。Windows 版在 Linux 上交叉编译：
Rust + Tauri 2. The Windows build is cross-compiled on Linux:

```sh
sudo apt-get install -y mingw-w64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
# target/x86_64-pc-windows-gnu/release/ClaudePet.exe + WebView2Loader.dll
```

改了 `Cargo.toml` 和 `tauri.conf.json` 里的版本号并推送到 main，GitHub Actions 就自动编译并发布这个版本（见 `.github/workflows/release.yml`）。已安装的宠物会自己更新到签名对得上的新版本。
Bump the version in `Cargo.toml` and `tauri.conf.json` and push to main: GitHub Actions builds and publishes that version (see `.github/workflows/release.yml`). Installed pets update themselves to new versions whose signature checks out.

个人项目，仅供个人使用。
Personal project, for personal use.
