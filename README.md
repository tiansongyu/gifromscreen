<p align="center">
  <img src="packaging/linux/io.github.tiansongyu.gifromscreen.svg" width="88" height="88" alt="GifFromScreen 图标">
</p>

# GifFromScreen

[![Linux CI](https://github.com/tiansongyu/gifromscreen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/tiansongyu/gifromscreen/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tiansongyu/gifromscreen)](https://github.com/tiansongyu/gifromscreen/releases/latest)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-93450a)](Cargo.toml)
[![MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#参与和许可)

在 Linux 上录制一块屏幕，逐帧编辑，再导出 GIF。

使用 Rust 编写，录制、编辑和导出均在本地完成。以 [ScreenToGif](https://github.com/NickeManarin/ScreenToGif) 的工作方式为参考，专注 GIF；这是独立项目，macOS 尚未提供。

[English](README.en.md) · [下载](https://github.com/tiansongyu/gifromscreen/releases/latest) · [安装说明](docs/PACKAGING.md) · [已完成与待办](docs/WORK-STATUS.md) · [反馈问题](https://github.com/tiansongyu/gifromscreen/issues)

## 获取与运行

**[下载 Linux x86_64 正式版](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0/gifromscreen-0.1.0-linux-x86_64.tar.gz)** · [SHA-256 校验文件](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0/gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256) · [版本说明](docs/releases/v0.1.0.md)

当前正式版为 **v0.1.0**。无需登录 GitHub 即可下载；这是冻结当前已验收功能的首个正式版本，不代表完整复刻或零缺陷。后续测试构建与正式版分开，见 [CI Artifacts](https://github.com/tiansongyu/gifromscreen/actions/workflows/portable.yml)（需要登录，保留 30 天）。

**X11 / NVIDIA 启动提示：** v0.1.0 若报 `incompatible_surface_backends: Backends(GL)`，可临时用 `WGPU_BACKEND=vulkan ./bin/gif-from-screen` 启动。`main` 的 0.1.1 修复构建已支持自动选择兼容后端；该修复不在旧的 v0.1.0 下载包中。[修复与验证](docs/GRAPHICS-BACKEND-STARTUP.md)。

便携包以 **Ubuntu 22.04 / glibc 2.35** 为构建基线，包含桌面程序和命令行工具。下载压缩包与校验文件后，在同一目录运行：

```sh
sha256sum --check gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf gifromscreen-0.1.0-linux-x86_64.tar.gz
cd gifromscreen-0.1.0-linux-x86_64
sha256sum --check SHA256SUMS
./bin/gif-from-screen
```

无需安装，也无需管理员权限。可选的 `./install.sh` 会添加用户级桌面入口；[卸载说明](docs/PACKAGING.md#installation-and-removal)列出了对应操作。包尚未签名，校验和用于检查完整性，不是独立身份证明。AppImage 不随本版发布，Flatpak 尚未交付。

## 录制 → 编辑 → GIF

![实际 X11 操作：打开录制框、暂停移动选区、继续录制并进入编辑器](docs/assets/record-and-retarget.gif)

真实 X11 窗口操作，包含暂停后拖动录制框；演示录自早期版本，正式版的部分文字与布局有所更新。[演示来源与验证](docs/assets/README.md)。

1. **框选并录制。** 打开“屏幕录制”，选择来源和区域。X11 使用独立边框与控制面板；Wayland 先通过系统分享对话框授权，再调整来源内的裁剪区域。开始前可调整尺寸，录制或暂停时可移动固定尺寸区域。
2. **逐帧整理。** 停止后进入编辑器，删除多余帧、调整播放时间、裁剪画面，或添加文字、箭头和点击提示。编辑支持撤销与重做。
3. **导出 GIF。** 选择全部或部分帧，设置颜色、循环、透明与抖动后导出；保留 `.gfsproj` 工程，之后仍可继续编辑。

### 主要功能

- **控制录制：** 倒计时、开始、暂停、继续、停止保存和明确丢弃；连续录制、周期快照、手动快照，以及 X11 桌面交互触发快照。
- **精确定位：** X11 数字坐标、方向键微调、窗口吸附和拖选窗口；录制位置与控制面板分离。GIF 每帧播放时长可独立于采样间隔设置。
- **编辑动画：** 帧选择、排序、剪切/复制/粘贴、延时、降帧、去重、往返循环；裁剪、缩放、旋转、翻转，以及淡入淡出和滑动转场。
- **添加说明：** 字幕、标题帧、水印、形状、自由绘图、边框、阴影和局部动态效果；可编辑的图层、进度、按键、点击与光标标注。
- **复用素材：** 导入 GIF、PNG/JPEG/BMP/WebP、图片序列和视频；新建空白动画、录制画板，以及摄像头录制入口。
- **保留工作：** 录制时增量保存工程，异常后可恢复已写入内容；支持另存工程副本、最近项目和自动编辑/导出预设。

本版也包含[多图形画布](docs/VECTOR-SHAPE-CANVAS.md)：三角形、圆角矩形、椭圆、块状箭头，多选、拖动、缩放和旋转。使用已验收的 Vector v1 / 工程 schema 8；未完成的新版渲染集成不包含在正式版内。

`main` 的 0.1.2 维护构建新增[醒目的录制框拖动把手](docs/RECORDER-DRAG-HANDLE.md)，不必再对准细边框；它不在旧 v0.1.0 下载包中。

以[发布工作总结](docs/WORK-STATUS.md)为当前完成状态入口；[功能对照](docs/FEATURE_MATRIX.md)保留 ScreenToGif 目标与差距，[开发记录](docs/DEVELOPMENT-STATUS.md)保留历史证据。

## X11 与 Wayland

| | X11 | Wayland |
| --- | --- | --- |
| 选取区域 | 桌面独立边框、坐标微调、窗口拖选与吸附 | 系统 Portal 选择来源，再在预览中裁剪 |
| 录制中移动 | 移动固定尺寸的桌面区域 | 移动已授权来源内的固定尺寸裁剪 |
| 控制面板 | 与选区分离，优先放在区域外 | 紧凑控制器；录制显示器时需自行移出选区 |
| 全局快捷键 | 可选启用，支持自定义并保存 | 通过 GlobalShortcuts Portal，取决于桌面支持与授权 |
| 输入与光标标注 | 可选采集按键、点击及可编辑光标 | 嵌入/隐藏光标；可使用手工标注 |

快捷键默认关闭；默认组合为 Ctrl+Shift+F7 开框/开始/暂停/继续、Ctrl+Shift+F8 停止、Ctrl+Shift+F9 手动快照。注册失败时仍可用按钮控制。详见[快捷键说明](docs/GLOBAL-SHORTCUTS.md)。

Wayland 不提供通用的全局透明取景框，也不能保证自动排除控制器或强制被遮挡的应用刷新画面。优先选择窗口来源，或确保控制器不在显示器录制区域内。

## 界面语言

默认跟随机器语言，也可在“语言 / Language”中修改并保存，重启后保留。正式版接入 **848 条英文 / 简体中文消息**，覆盖导航、录制、主要编辑、预览、裁剪、导出、效果、图层、水印和文字 / 标题等界面。已迁移的提示随切换更新；部分表单、共享控件和后端诊断尚未完整翻译。

提供 **System + 29 项语言选择**；其中另外 **27 个目标语言尚无译文，会明确回退英语**，仍保存用户选择。可选择不等于已翻译。详见[本地化范围](docs/LOCALIZATION-PLAN.md)。

![真实界面即时切换中英文并自动保存语言选择](docs/assets/language-switch.gif)

## 运行要求与当前边界

- Linux x86_64、X11 或 Wayland 桌面，以及可用的 OpenGL ES / Vulkan 图形驱动。便携包不是静态链接程序，仍需要[系统运行库](packaging/linux/README.txt)。
- Wayland 屏幕录制需要 PipeWire 和适配桌面的 xdg-desktop-portal 后端，遵循系统分享授权。
- 视频导入额外需要系统 `ffmpeg` 和 `ffprobe`；普通屏幕录制、图片/GIF 编辑和内置 GIF 导出不需要它们。
- 目前仅导出 GIF，不录制音频。摄像头入口已实现，但物理设备验收尚未完成。
- 已有隔离 GNOME/X11 的端到端录制与导出验证；物理 GNOME/KDE、混合 DPI、多显示器、长时间高分辨率录制和部分效果精度仍需继续验收。本项目尚未宣称完整一比一复刻或零缺陷。
- 窄窗口放大至 150% 时，首页启动卡片可能文字重叠；恢复 100% 或放大窗口可缓解。编辑器已完成单页滚动与关键按钮可见性修复。

按键/点击元数据采集默认关闭。开启后可能记录敏感输入，分享 `.gfsproj` 前请检查工程内容；GIF 不携带工程输入元数据，但已绘制的标注会出现在画面中。

## 从源码运行

需要 Rust 1.88 或更新版本，以及 [CI 列出的 Linux 开发库](.github/workflows/portable.yml)。在仓库根目录运行：

```sh
cargo run --locked -p gif-from-screen
cargo run --locked -p gif-from-screen-cli -- doctor
```

开发检查：

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
```

## 参与和许可

欢迎通过 [Issues](https://github.com/tiansongyu/gifromscreen/issues) 提交复现步骤，注明发行版、X11/Wayland、版本和是否涉及多显示器；提交日志或工程前请移除个人信息。见[贡献指南](CONTRIBUTING.md)、[版本记录](CHANGELOG.md)和[设计文档](docs/DESIGN.md)。当前已停止扩大本次发布范围；未完成事项集中保留在[工作总结](docs/WORK-STATUS.md)。

本项目采用 [MIT](packaging/licenses/LICENSE-MIT) 或 [Apache-2.0](packaging/licenses/LICENSE-APACHE) 许可。它是独立实现，不使用 ScreenToGif 的品牌素材；部分数值算法按 dotnet/WPF 的 MIT 许可移植，第三方来源与许可见 [NOTICE](packaging/licenses/NOTICE.txt)。
