# 护眼吧 (huyanba)

桌面护眼小软件：防蓝光过滤 + 定时休息锁屏。

## 功能概览
- 过滤蓝光：强度 + 色调调节，预设模式（智能/办公/影视/游戏）
- 定时休息：默认每 30 分钟休息 1 分钟
- 全屏休息锁屏：多显示器覆盖、倒计时显示
- 托盘控制：显示/隐藏/立即休息/退出

## 本地开发
```
cd D:\Ai\huyanba\huzamba
npm install
npm run tauri dev
```

## 打包（Windows 安装包）
```
cd D:\Ai\huyanba\huzamba
npm run tauri build
```

产物目录：
```
src-tauri\target\release\bundle
```

## 说明
- 过滤蓝光通过系统 gamma 曲线实现
- 锁屏使用全屏覆盖窗口（非系统锁屏）
