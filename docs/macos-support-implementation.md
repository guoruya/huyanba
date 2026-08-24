# 护眼吧 Windows + macOS 双平台修改实施文档

> 文档状态：实施方案
>
> 编写日期：2026-08-21
>
> 当前项目版本：前端/Tauri 配置为 2.3.0，Cargo 与 package-lock 仍为 2.1.0，实施时需要统一

## 1. 目标

本次修改不是把 Windows 版本改造成 macOS 版本，而是在保留现有 Windows 产品、功能和安装包的前提下，为同一代码仓库增加 macOS 支持。

最终交付物：

- Windows x64：继续输出 NSIS `.exe` 和 MSI `.msi`。
- macOS：新增 Intel 与 Apple Silicon 通用的 Universal `.app` 和 `.dmg`。
- Windows 与 macOS 共用 React 界面、业务模型、壁纸数据格式和大部分 Rust 逻辑。
- 涉及系统 API 的功能通过编译期平台适配分别实现，不在运行时调用错误平台的 API。
- 同一个版本标签同时发布 Windows 和 macOS 安装产物。

首个 macOS 正式版本建议支持 macOS 12.0 及以上，先通过官网下载/GitHub Release 分发签名并公证的 DMG，不把 Mac App Store 作为首期目标。

## 2. 不在本次范围内

- 不删除或降级 Windows 的滤蓝光、托盘、多显示器休息页和壁纸功能。
- 不建立一份独立的 Mac 前端或复制整个项目为第二个仓库。
- 不把休息覆盖页包装成安全锁屏；它仍是可退出的休息提醒界面。
- 不在首期接入 Mac App Store。App Sandbox、长期安全作用域书签和商店审核另行实施。
- 不在本次顺带重做整体 UI、账号系统或自动更新系统。

## 3. 双平台实施原则

1. **共享业务，隔离系统能力**

   React UI、壁纸管理、网络解析、图片处理和设置模型保持共享；gamma、窗口层级、进程调用和系统生命周期放入平台模块。

2. **Windows 逻辑先迁移、后验证，不改算法**

   现有 Win32 gamma 代码先原样移动到 Windows 适配器。平台拆分阶段不调整 Windows 的颜色公式、托盘交互和下载回退顺序。

3. **保持现有前后端命令兼容**

   `set_gamma`、`reset_gamma`、`show_lock_windows`、`hide_lock_windows` 等 Tauri command 名称继续保留。前端不需要根据操作系统选择不同命令。

4. **使用 `cfg` 编译期隔离**

   Windows 安装包不链接 macOS 框架，macOS 安装包不链接项目直接声明的 Win32 API。

5. **分别构建、共同发布**

   Windows 产物在 Windows runner 构建，macOS 产物在 macOS runner 构建。不能用 Windows 构建机直接生成可签名公证的 Mac 安装包。

## 4. 当前代码现状与影响

| 位置 | 当前状态 | 双平台影响 |
| --- | --- | --- |
| `src-tauri/Cargo.toml` | `windows` crate 是通用依赖 | Mac 构建仍会处理 Windows 专属依赖，需改为 target dependency |
| `src-tauri/src/lib.rs:31-33` | 无条件导入 Win32 GDI | Mac 编译阻塞 |
| `src-tauri/src/lib.rs:533-580` | gamma 只实现 `SetDeviceGammaRamp` | Windows 保留，Mac 新增 CoreGraphics 实现 |
| `src-tauri/src/lib.rs:584-650` | 所有平台使用同一全屏窗口策略 | Mac 的 Spaces、Retina 和窗口层级行为不同 |
| `src-tauri/src/lib.rs:1927-2036` | PowerShell、`curl.exe` 和 `creation_flags` 混在共享代码中 | Mac 编译和故宫壁纸回退不可用 |
| `src/App.tsx:1430-1433` | 每秒 JS timer 驱动时间 | Mac 主窗口隐藏后可能被 WebView 后台节流 |
| `src/App.tsx:1759-1815` | 休息到期判断在前端 | 隐藏到菜单栏后 30 分钟提醒可能不可靠 |
| `src/App.tsx:2961-2967` | “开机自启”只有无事件复选框 | 两个平台都尚未真正实现 |
| `src-tauri/tauri.conf.json` | 只有共享配置，bundle target 为 `all` | 需增加 Windows/Mac 平台配置 |
| `package.json`、`Cargo.toml`、`package-lock.json` | 版本号不一致 | 安装包、应用元数据和 Release 名称可能不一致 |

可以直接复用的主要部分：

- React 页面和锁屏页面内容。
- Unsplash 与故宫数据解析、图片筛选和图片处理。
- Tauri AppCache/AppConfig 路径解析。
- 壁纸目录迁移、本地索引、固定壁纸和轮播逻辑。
- Tauri 事件模型和大部分托盘菜单逻辑。
- 已有 Windows `.ico` 和有效的 macOS `.icns` 图标资源。

## 5. 目标代码结构

建议逐步把系统相关代码从当前单一的 `lib.rs` 中抽出：

```text
src-tauri/src/
├── lib.rs                         # Tauri 组装、command 注册
├── settings.rs                    # 跨平台设置持久化
├── scheduler.rs                   # 跨平台休息调度
└── platform/
    ├── mod.rs                     # 统一平台接口
    ├── display_filter/
    │   ├── mod.rs                 # 统一滤镜接口和数据类型
    │   ├── windows.rs             # 现有 Win32 GDI 实现
    │   └── macos.rs               # 新增 CoreGraphics 实现
    ├── lock_window/
    │   ├── mod.rs                 # 共享窗口创建流程
    │   ├── windows.rs             # 保留当前 Windows 行为
    │   └── macos.rs               # Mac Spaces/全屏配置
    └── download_transport/
        ├── mod.rs                 # reqwest 优先及回退编排
        ├── windows.rs             # PowerShell + curl.exe
        └── macos.rs               # /usr/bin/curl
```

`platform/mod.rs` 只暴露平台无关接口，例如：

```rust
pub fn apply_display_filter(config: FilterConfig) -> Result<FilterStatus, String>;
pub fn restore_display_filter() -> Result<(), String>;
pub fn reapply_display_filter() -> Result<(), String>;
pub fn configure_lock_window(window: &tauri::WebviewWindow) -> Result<(), String>;
pub fn fallback_download(url: &str, output: &Path) -> Result<(), String>;
```

调用方不直接出现 Win32 或 AppKit/CoreGraphics 类型。

## 6. 具体修改内容

### 6.1 Cargo 依赖按平台拆分

把 `windows` 从公共 `[dependencies]` 移到 Windows target：

```toml
[dependencies]
tauri = { version = "2", features = ["protocol-asset", "tray-icon", "image-png"] }
tauri-plugin-dialog = "2"
tauri-plugin-opener = "2"
# 其余共享依赖保持不变

[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.56", features = [
  "Win32_Foundation",
  "Win32_Graphics_Gdi",
  "Win32_UI_ColorSystem"
] }

[target.'cfg(target_os = "macos")'.dependencies]
core-graphics = "0.24"
# 若需要监听 NSWorkspace 睡眠/唤醒或补充窗口行为，
# 再直接声明与当前 Tauri lockfile 对齐的 objc2/AppKit 依赖。
```

同时把 `src-tauri/src/main.rs` 的属性限制为 Windows release：

```rust
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]
```

### 6.2 Windows 滤蓝光实现

将现有以下代码移动到 `platform/display_filter/windows.rs`：

- `GetDC(HWND(0))`
- 256 项 RGB gamma ramp 计算
- `SetDeviceGammaRamp`
- `ReleaseDC`

Windows 迁移要求：

- 保持现有 `temperature_to_rgb`、强度、green boost、red cut、blue cut 计算结果不变。
- 保持 `set_gamma`/`reset_gamma` command 入参和返回方式不变。
- 托盘退出、窗口退出时继续恢复颜色。
- 迁移阶段不顺带修改 Windows gamma 算法。

### 6.3 macOS 滤蓝光实现

Mac 使用 Apple 公开 CoreGraphics display transfer table API：

1. 枚举全部活动显示器。
2. 第一次启用时读取并保存每台显示器原始 RGB transfer table。
3. 在原始 transfer table 上应用当前色温与强度系数。
4. 对每台显示器调用 `CGSetDisplayTransferByTable`。
5. 关闭滤镜时恢复各显示器保存的原始表。
6. 如果原始表不可恢复，使用 `CGDisplayRestoreColorSyncSettings` 兜底。
7. 显示器插拔、镜像模式变化、分辨率变化或屏幕唤醒后，重新枚举并根据最后一次配置重放滤镜。

Mac 状态建议放入 Tauri managed state：

```text
MacDisplayFilterState
├── enabled
├── last_config
├── original_tables: display_id -> RGB tables
└── applied_display_ids
```

退出恢复路径必须覆盖：

- 托盘“退出”。
- 主菜单 Cmd+Q。
- 前端 `request_quit`。
- Tauri 正常退出事件。
- 下一次应用启动时先恢复 ColorSync，再根据已保存设置应用，修复上次非正常退出可能留下的状态。

需要新增“恢复屏幕原始颜色”托盘菜单项，作为用户可见的恢复手段。

不得使用私有 Night Shift/CoreBrightness API。私有 API 会增加系统升级失效和签名/审核风险。

#### Mac gamma 实测项目

- Apple Silicon 内置 Retina 屏。
- Intel Mac 或实际 Intel 构建运行环境。
- 外接 1x/2x 缩放显示器。
- 显示器位于主屏左侧、右侧和上方。
- HDR 开关、True Tone、Night Shift 开关组合。
- 合盖/睡眠/唤醒。
- 外接显示器热插拔和镜像模式。

若个别显示器明确不支持 transfer table，再对“不支持的显示器”提供透明色层降级；不把覆盖层作为所有 Mac 的默认方案。

### 6.4 多显示器休息窗口

共享部分继续负责：

- 遍历 `available_monitors()`。
- 创建 `lockscreen-{index}` WebView。
- 传递结束时间、暂停状态和 ESC 设置。
- 广播倒计时和用户操作。

Windows 分支：

- 保留现有窗口尺寸、位置、`always_on_top`、`skip_taskbar` 和 `set_fullscreen(true)` 行为。
- 首期不借 Mac 适配修改 Windows 多显示器算法。

macOS 分支：

- 使用显示器实际逻辑矩形，不使用当前额外 `+400/-200` 的补偿。
- 无边框、不可缩放、置顶。
- 启用 `visible_on_all_workspaces(true)`。
- 创建完成后使用 `set_simple_fullscreen(true)`，避免原生全屏为每个锁屏窗口创建新的 Space。
- 不依赖 `skip_taskbar`，该能力在 macOS 不受支持。
- 验证普通桌面、全屏应用、多个 Spaces 和 Stage Manager。
- 显示器配置在休息期间发生变化时，关闭失效窗口并按新显示器列表重建。

休息页仍不是安全边界：不拦截 Cmd+Tab、系统退出、切换用户或强制退出，因此不申请辅助功能权限。

### 6.5 休息调度移入 Rust

当前到期判断依赖主窗口 WebView 的 `setInterval`。Mac 主窗口隐藏到菜单栏后，WebKit 可能暂停定时器，不能作为核心调度源。

新增 `scheduler.rs`，作为 Windows 与 Mac 共用的休息调度服务：

- Rust 保存 `rest_enabled`、工作间隔、休息时长、下次到期时间和暂停状态。
- 后台任务按绝对截止时间判断，而不是依赖前端每秒递减。
- 到期后由 Rust 创建/显示休息窗口，并向前端发送状态事件。
- 前端倒计时只负责展示；即使主窗口隐藏，Rust 调度仍运行。
- 睡眠不计入连续使用时间。唤醒后重置下一次休息时间，避免用户开盖后立即进入休息页。
- 应用重启后从设置恢复用户参数，但重新开始一个工作周期。

迁移步骤：

1. 新增 Rust scheduler 和查询/更新 command。
2. 前端读取 scheduler 状态，保留现有 UI。
3. Windows 与 Mac 都通过后端事件触发休息。
4. 确认没有重复触发后，删除前端的到期判定 effect，只保留显示用时钟。

Windows 验收通过前，不删除现有调用接口。

### 6.6 设置持久化

新增 `settings.rs`，沿用项目已有 AppConfig JSON 读写方式，保存：

- 滤镜开关、强度、色温、预设。
- 休息开关、工作间隔、休息时长。
- 是否允许 ESC 退出。
- 开机自启状态由 autostart plugin 查询，避免只相信 JSON。

使用 `serde(default)` 保证旧用户没有设置文件时继续获得当前默认值。Windows 原有壁纸目录和壁纸索引不迁移、不改名。

### 6.7 故宫壁纸下载传输

保持共享顺序：

```text
reqwest（系统代理）
  -> reqwest（no_proxy 重试）
  -> 当前平台的系统命令回退
```

Windows：

- 保留 PowerShell `Invoke-WebRequest`。
- 保留 `curl.exe`。
- 只有 Windows 代码调用 `CommandExt::creation_flags`。

macOS：

- 不检测或调用 PowerShell。
- 使用固定存在的 `/usr/bin/curl`，不依赖 Finder 启动时的 shell PATH。
- curl 参数、Referer、Accept、User-Agent、超时和临时文件清理与 Windows 保持等价。

将当前写死的 Windows Chrome User-Agent 改为中性的桌面 User-Agent，避免平台判断错误；该修改不改变服务器请求字段的语义。

### 6.8 托盘、菜单栏与开机自启

接入官方 `tauri-plugin-autostart`：

- Windows 使用插件的 Windows 实现。
- macOS 使用 `MacosLauncher::LaunchAgent`。
- 前端复选框初始化时调用 `isEnabled()`。
- 用户切换时调用 `enable()`/`disable()`，失败时恢复 UI 状态并显示错误。

启动行为：

- 用户正常打开应用：显示并聚焦主窗口。
- 开机启动：传入 `--hidden`，只启动托盘/菜单栏和后台调度，不自动弹主窗口。
- 当前 `setup()` 中无条件 `show()` 主窗口的逻辑改为根据启动参数判断。

macOS 菜单栏：

- 增加适合深浅色菜单栏的单色模板图标。
- Mac 构建调用 `icon_as_template(true)`；Windows 继续使用现有彩色托盘图标。
- 菜单统一包含“显示主界面”“隐藏主界面”“立即休息”“恢复屏幕原始颜色”“退出”。
- macOS 左键默认展示菜单；Windows 保持现有左键显示窗口、右键菜单习惯。

### 6.9 Tauri 平台配置

保留 `src-tauri/tauri.conf.json` 作为共享配置，并新增：

```text
src-tauri/tauri.windows.conf.json
src-tauri/tauri.macos.conf.json
```

Windows 配置明确保留现有安装包：

```json
{
  "bundle": {
    "targets": ["nsis", "msi"]
  }
}
```

macOS 配置建议：

```json
{
  "bundle": {
    "targets": ["app", "dmg"],
    "macOS": {
      "minimumSystemVersion": "12.0",
      "category": "public.app-category.healthcare-fitness"
    }
  }
}
```

注意事项：

- 继续使用现有 `com.admin.huyanba` identifier 完成本次双平台适配，避免同时引入 Windows 数据目录变化。若未来更换为自有域名 Bundle ID，需要单独设计 Windows/Mac 数据迁移。
- 统一 `package.json`、`package-lock.json`、`Cargo.toml`、`Cargo.lock`、`tauri.conf.json` 和 README 的发布版本。
- Windows `.ico` 与 Mac `.icns` 均保留在共享 icon 列表。
- Mac 首期不启用 App Sandbox；当前自定义绝对壁纸目录在 App Store Sandbox 下需要额外的安全作用域书签。

### 6.10 安全与发布清理

这些不是 Mac API 适配本身，但应在公开发布 Mac 二进制前处理：

- 当前 `csp` 为 `null`。根据实际远程图片和 API 域名配置 CSP，而不是继续完全关闭。
- 当前二进制内置 Unsplash Access Key，可被安装包提取并消耗公共配额。正式发行前应决定使用用户自带 Key、受控服务端代理或接受公开客户端 Key 的配额风险。
- `authors = ["you"]`、通用 description 和 README 中的 2.1.0/Windows 路径需要更新。

## 7. 构建与 CI

### 7.1 本地开发环境

Windows：

- 保留当前 Node、Rust MSVC、WebView2 和 Visual C++ Build Tools 环境。

macOS：

- Xcode Command Line Tools。
- Rust stable。
- Node LTS 与 `npm ci`。
- Universal 构建需要两个 Rust target：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

当前检查所用 Mac 环境没有 Cargo，项目也没有安装 `node_modules`；实际实施的第一步需要补齐这些依赖后再建立真实编译结果。

### 7.2 构建命令

Windows x64：

```powershell
npm ci
npm run tauri build -- --target x86_64-pc-windows-msvc --bundles nsis,msi
```

macOS Universal：

```bash
npm ci
npm run tauri build -- --target universal-apple-darwin --bundles app,dmg
```

### 7.3 GitHub Actions

新增或扩展 `.github/workflows/release.yml`，使用两个独立 job：

| Job | Runner | Target | 产物 |
| --- | --- | --- | --- |
| `build-windows` | `windows-latest` | `x86_64-pc-windows-msvc` | `.exe`、`.msi` |
| `build-macos` | `macos-latest` | `universal-apple-darwin` | `.app`、`.dmg` |

两个 job 使用同一版本标签，并把产物上传到同一个 GitHub Release。Mac job 需要配置：

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- App Store Connect API Key 对应的 issuer、key id 和私钥，或 Apple ID 公证凭据

正式 DMG 使用 Developer ID Application 签名、Hardened Runtime、公证和 stapling。Windows 的现有发布方式不因 Mac job 改变。

## 8. 分阶段实施计划

### 阶段 1：双平台可编译结构（1～2 天）

- 拆分 target dependency 和 `cfg` import。
- 修复 `main.rs` Windows 专属属性。
- 建立 `platform` 模块和统一接口。
- 把现有 Windows gamma 原样移动到 Windows 文件。
- 分离 PowerShell、`curl.exe` 和 `/usr/bin/curl`。
- 分别完成 Windows `cargo check` 和 Mac `cargo check`。

交付结果：Windows 行为未变，Mac 后端能够编译，Mac 暂未要求滤镜功能完成。

### 阶段 2：macOS 滤蓝光（2～3 天）

- CoreGraphics gamma table 获取、应用和恢复。
- 多显示器状态管理。
- 正常退出、Cmd+Q、启动恢复。
- 显示器热插拔和睡眠唤醒后重放。
- 前端显示“不支持/部分显示器失败”等真实状态，不能仅在 console 中报错。

交付结果：Windows 使用原 Win32 实现，Mac 使用 CoreGraphics 实现，前端 command 不分叉。

### 阶段 3：调度与休息窗口（2～3 天）

- Rust scheduler 与设置持久化。
- Windows 继续按原用户配置触发休息。
- Mac 使用 simple fullscreen、all workspaces 和正确的显示器矩形。
- 睡眠/唤醒语义和动态显示器处理。

交付结果：两个平台在主窗口隐藏时都能可靠触发休息。

### 阶段 4：菜单栏、自启和平台体验（1～2 天）

- 官方 autostart plugin。
- `--hidden` 启动。
- Mac template 菜单栏图标与菜单习惯。
- 统一退出恢复路径。

### 阶段 5：打包、签名和发布（1～2 天）

- Windows/Mac 平台配置。
- Universal app 与 DMG。
- Developer ID 签名、公证、stapling。
- CI 双 job 与同一 Release 上传。
- 文档和版本统一。

### 阶段 6：双平台验收（1～2 天，可与前面阶段交叉）

- Windows 完整回归。
- Apple Silicon、Intel/Universal、多显示器和休眠测试。
- 全新 Mac 离线安装与 Gatekeeper 验证。

整体预计 8～12 个开发日，不包含申请 Apple Developer 账号、证书审批或采购 Intel/外接显示器测试设备的等待时间。

## 9. 验收清单

### 9.1 Windows 不回退

- [ ] Windows x64 能通过前端构建、`cargo check`、release build。
- [ ] NSIS `.exe` 和 MSI `.msi` 均正常生成和安装。
- [ ] 滤镜开启、调节、关闭和退出恢复与当前版本一致。
- [ ] 关闭主窗口仍隐藏到托盘，托盘显示/隐藏/退出正常。
- [ ] 多显示器休息页、倒计时、暂停、ESC 设置正常。
- [ ] Unsplash 和故宫壁纸下载正常。
- [ ] Windows 的 PowerShell/curl 回退仍可用。
- [ ] 原有壁纸目录、索引和设置数据可以继续读取。

### 9.2 macOS 新增能力

- [ ] Apple Silicon 原生运行。
- [ ] x86_64 构建可运行，Universal 二进制同时包含 arm64 与 x86_64。
- [ ] 内置屏和外接屏都能应用及恢复滤镜。
- [ ] 退出、Cmd+Q 和菜单栏退出都会恢复原始颜色。
- [ ] 睡眠唤醒、显示器插拔后滤镜状态正确。
- [ ] 休息窗口覆盖每台显示器，不额外创建不可控的全屏 Space。
- [ ] 在普通桌面、全屏应用、多个 Spaces 和 Stage Manager 中完成实测。
- [ ] 主窗口隐藏超过 30 分钟后仍准时触发休息。
- [ ] 开机启动不会自动弹出主窗口。
- [ ] 菜单栏图标在深色和浅色模式下清晰。
- [ ] 自定义壁纸目录、应用默认缓存目录和本地图片展示正常。
- [ ] 签名、公证和 stapling 成功，全新 Mac 不需要绕过 Gatekeeper。

### 9.3 共享功能

- [ ] 设置在应用重启后保留。
- [ ] 前端对滤镜应用失败显示明确错误，不出现 UI 显示开启但系统未生效。
- [ ] 两个平台使用相同 Tauri command 契约。
- [ ] 两个平台发布产物版本号一致。
- [ ] 同一个 GitHub Release 同时包含 Windows 和 macOS 下载文件。

## 10. 主要风险与处理

| 风险 | 具体表现 | 处理方式 |
| --- | --- | --- |
| Mac 显示器不接受 gamma table | HDR、部分外接屏无效果或返回错误 | 先做真机测量；仅对不支持显示器启用透明色层降级 |
| True Tone/Night Shift 与应用竞争 | 色温被系统重新覆盖 | 监听显示重配置/唤醒并重放；UI提示系统功能可能影响结果 |
| 隐藏 WebView 定时器暂停 | 30 分钟休息不触发 | Rust scheduler 成为唯一到期判断源 |
| macOS 原生全屏创建 Spaces | 多屏窗口切换动画或覆盖失败 | Mac 使用 simple fullscreen + visible on all workspaces |
| 非正常退出留下色温 | 用户桌面颜色未恢复 | 启动时恢复、正常退出全路径恢复、托盘提供手动恢复 |
| Mac 公证失败 | Gatekeeper 阻止或警告 | Developer ID、Hardened Runtime、时间戳、公证日志和 stapling 验证 |
| 平台拆分造成 Windows 回归 | Windows gamma/托盘/下载行为变化 | Windows 实现先原样迁移，每阶段执行 Windows 功能回归 |
| App Store Sandbox 限制目录 | 重启后失去自定义目录访问权 | 首版用 Developer ID DMG；App Store 另做安全作用域书签方案 |

## 11. 完成定义

只有同时满足以下条件，才视为“增加 Mac 支持”完成：

1. Windows 当前功能和安装包继续可用。
2. Mac 不是仅能打开界面，而是滤蓝光、后台休息调度、多显示器休息页、托盘和壁纸均可使用。
3. Mac 同时支持 Apple Silicon 和 Intel，产出 Universal DMG。
4. 正式 DMG 已签名、公证并通过全新 Mac 的 Gatekeeper 验证。
5. 同一源码、同一版本号、同一 Release 同时交付两个平台。

## 12. 官方参考

- Tauri macOS 开发环境：<https://v2.tauri.app/start/prerequisites/>
- Tauri Universal target 与 build CLI：<https://v2.tauri.app/reference/cli/#build>
- Tauri macOS App Bundle：<https://v2.tauri.app/distribute/macos-application-bundle/>
- Tauri DMG：<https://v2.tauri.app/distribute/dmg/>
- Tauri macOS 签名与公证：<https://v2.tauri.app/distribute/sign/macos/>
- Tauri Autostart：<https://v2.tauri.app/plugin/autostart/>
- Apple CoreGraphics display functions：<https://developer.apple.com/documentation/coregraphics/core-graphics-functions>
- Apple 显示重配置回调：<https://developer.apple.com/documentation/coregraphics/cgdisplayregisterreconfigurationcallback%28_%3A_%3A%29>
- Apple macOS 公证：<https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution>
