# 护眼吧 (huyanba)

桌面护眼小软件：防蓝光过滤 + 定时休息锁屏，支持 Windows x64 与 macOS Universal。

## 功能概览

- 过滤蓝光：强度 + 色调调节，预设模式（智能/办公/影视/游戏）
- 定时休息：默认每 30 分钟休息 1 分钟
- 全屏休息锁屏：多显示器覆盖、倒计时显示
- 托盘/菜单栏控制：显示、隐藏、立即休息、退出
- 壁纸来源：Unsplash 在线搜索与故宫候选壁纸

## 界面截图

- 首页总览（当前护眼状态 + 下一次休息）
![首页总览](docs/images/dashboard-overview.png)

- 壁纸控制台 / Unsplash（在线搜索与下载）
![壁纸控制台 Unsplash](docs/images/wallpaper-console-unsplash.png)

- 壁纸控制台 / 故宫来源（来源切换与分页控制）
![壁纸控制台 故宫概览](docs/images/wallpaper-console-palace-overview.png)

- 壁纸控制台 / 故宫候选网格（本地候选预览）
![壁纸控制台 故宫候选网格](docs/images/wallpaper-console-palace-grid.png)

## 版本与下载

当前开发版本：`2.4.0`。

同一个 GitHub Release 会包含两个平台的产物：

- Windows x64：NSIS 安装包（`.exe`）和 MSI 安装包（`.msi`）。
- macOS Universal：签名并公证的应用包（`.app`）和 DMG 安装包（`.dmg`），同时支持 Apple Silicon 与 Intel。

`v2.4.0` 发布后可从 [v2.4.0 Release 页面](https://github.com/guoruya/huyanba/releases/tag/v2.4.0) 下载对应平台文件。Windows 产物通常命名为 `huyanba_2.4.0_x64-setup.exe` 与 `huyanba_2.4.0_x64_en-US.msi`；macOS 文件名以 Release 页面提供的 Universal 产物为准。

## 本地开发

```bash
npm ci
npm run tauri dev
```

## 本地构建

### Windows x64

需要 Rust MSVC 工具链、WebView2 和 Visual C++ Build Tools：

```powershell
npm ci
npm run tauri build -- --target x86_64-pc-windows-msvc --bundles nsis,msi
```

产物目录：

```text
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/msi/
```

### macOS Universal

需要 Xcode Command Line Tools、Rust stable 和 Node.js LTS。Universal 构建需要两个 Rust target：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm ci
npm run tauri build -- --target universal-apple-darwin --bundles app,dmg
```

产物目录：

```text
src-tauri/target/universal-apple-darwin/release/bundle/macos/
src-tauri/target/universal-apple-darwin/release/bundle/dmg/
```

`src-tauri/tauri.windows.conf.json` 与 `src-tauri/tauri.macos.conf.json` 会由 Tauri 按目标平台自动合并。Windows 配置固定生成 NSIS/MSI；macOS 配置固定生成 app/DMG，最低支持 macOS 12.0。

## GitHub Actions 发布与签名

推送版本标签后，`.github/workflows/release.yml` 使用官方 `tauri-apps/tauri-action` 的两个 job 构建并上传到同一个 Release：

- `build-windows` 在 `windows-latest` 构建 x86_64 NSIS/MSI。
- `build-macos` 在 `macos-latest` 构建 `universal-apple-darwin` app/DMG。

工作流先把两个平台产物上传到草稿 Release，只有 macOS job（含签名与公证）成功后才公开发布，避免留下单平台的半成品 Release。

例如发布 2.4.0：

```bash
git tag v2.4.0
git push origin v2.4.0
```

正式 macOS DMG 使用 Developer ID Application、Hardened Runtime、公证和 stapling。仓库的 Actions secrets 必须由维护者配置真实凭据，workflow 不包含或伪造任何证书/密码：

- `APPLE_CERTIFICATE`：Base64 编码的 Developer ID `.p12`
- `APPLE_CERTIFICATE_PASSWORD`、`KEYCHAIN_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`、`APPLE_PASSWORD`（Apple ID 的 app-specific password）、`APPLE_TEAM_ID`

未配置这些 secrets 时不要把 CI 产物当作正式签名发布包。

## 说明

- 过滤蓝光通过系统 gamma 曲线实现。
- 锁屏使用全屏覆盖窗口，不是系统安全锁屏。
- 壁纸网络请求由 Rust 处理；WebView 的 CSP 只放行实际 API、图片、Google Fonts、Tauri IPC 和 asset protocol 来源。
- 安装包不内置 Unsplash Access Key；可在应用的壁纸设置中填写，或通过 `UNSPLASH_ACCESS_KEY` 提供。

---

# Huyanba

Desktop eye-care app: blue-light filtering and scheduled break reminders, available for Windows x64 and macOS Universal.

## Features

- Blue-light filter with strength, tone, and smart/office/movie/game presets
- Scheduled breaks (default 30 minutes work / 1 minute rest)
- Fullscreen rest lockscreen across multiple monitors
- Tray/menu-bar controls for show, hide, rest now, and quit
- Unsplash search and Palace Museum candidate wallpapers

## Version and downloads

Current development version: `2.4.0`.

The same GitHub Release contains:

- Windows x64 NSIS (`.exe`) and MSI (`.msi`) installers.
- A signed and notarized macOS Universal app (`.app`) and DMG (`.dmg`) for Apple Silicon and Intel.

After `v2.4.0` is published, download the platform-specific files from the [v2.4.0 Release page](https://github.com/guoruya/huyanba/releases/tag/v2.4.0). Windows artifacts are normally named `huyanba_2.4.0_x64-setup.exe` and `huyanba_2.4.0_x64_en-US.msi`; use the Universal macOS filenames shown on the Release page.

## Development

```bash
npm ci
npm run tauri dev
```

## Local builds

### Windows x64

Requires the Rust MSVC toolchain, WebView2, and Visual C++ Build Tools:

```powershell
npm ci
npm run tauri build -- --target x86_64-pc-windows-msvc --bundles nsis,msi
```

Installers are written to:

```text
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/msi/
```

### macOS Universal

Requires Xcode Command Line Tools, Rust stable, and Node.js LTS. Install both Rust targets before building Universal:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm ci
npm run tauri build -- --target universal-apple-darwin --bundles app,dmg
```

Outputs are written to:

```text
src-tauri/target/universal-apple-darwin/release/bundle/macos/
src-tauri/target/universal-apple-darwin/release/bundle/dmg/
```

Tauri automatically merges `src-tauri/tauri.windows.conf.json` or `src-tauri/tauri.macos.conf.json` for the selected target. Windows is restricted to NSIS/MSI; macOS is restricted to app/DMG with a macOS 12.0 minimum.

## GitHub Actions release and signing

Pushing a version tag runs `.github/workflows/release.yml`. The official `tauri-apps/tauri-action` is used by two jobs and uploads both platforms to the same Release:

- `build-windows` runs on `windows-latest` for x86_64 NSIS/MSI.
- `build-macos` runs on `macos-latest` for `universal-apple-darwin` app/DMG.

Both jobs upload to a draft Release first. The workflow publishes it only after the signed and notarized macOS job succeeds, so a failed platform build cannot leave a partial public Release.

For example:

```bash
git tag v2.4.0
git push origin v2.4.0
```

The production macOS DMG uses a Developer ID Application certificate, Hardened Runtime, notarization, and stapling. Configure real values as repository Actions secrets; the workflow never embeds or invents credentials:

- `APPLE_CERTIFICATE`: Base64-encoded Developer ID `.p12`
- `APPLE_CERTIFICATE_PASSWORD` and `KEYCHAIN_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD` (an Apple ID app-specific password), and `APPLE_TEAM_ID`

Do not treat CI output as a signed distribution package until these secrets are configured and the notarization step succeeds.

## Notes

- The blue-light filter uses system gamma curves.
- The rest screen is a dismissible fullscreen overlay, not a secure system lock screen.
- Rust handles wallpaper network requests; the WebView CSP allows only the required APIs, image hosts, Google Fonts, Tauri IPC, and asset protocol sources.
- Distribution bundles do not embed an Unsplash Access Key; configure one in wallpaper settings or through `UNSPLASH_ACCESS_KEY`.
