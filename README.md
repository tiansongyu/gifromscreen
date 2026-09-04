# GifFromScreen

GifFromScreen 是一个使用 Rust 实现的本地 GIF 录制与逐帧编辑工具。当前先完成 Linux 版本；Linux 稳定交付后再讨论 macOS adapter。

当前仓库已经进入 Linux-first 实现阶段。

## 设计文档

- [产品与软件架构设计](docs/DESIGN.md)
- [ScreenToGif 功能对照表](docs/FEATURE_MATRIX.md)
- [ADR-0001：Rust 桌面技术栈](docs/ADR-0001-RUST-DESKTOP-STACK.md)
- [Linux 实现状态](docs/LINUX-STATUS.md)

## 当前范围

- 对齐 ScreenToGif 2.43.2 的录制、逐帧编辑、项目管理和 GIF 导出能力。
- 最终输出只支持 GIF；不复制 ScreenToGif 的非 GIF 导出格式。
- 采用 clean-room 行为兼容方式，不复制上游名称、图标、界面素材或 C#/WPF 源码。
- 当前 Linux X11 已打通录制、动态移动取景框、项目编辑和 GIF 导出。Wayland Portal/PipeWire 原生后端已能协商并消费帧；桌面流程接线以及 GNOME/KDE 实机验收仍是发行门槛。macOS 工作暂缓。

## 当前可用流程

- 录制时主界面隐藏，只保留独立、置顶的取景框和控制条。
- 倒计时、开始、暂停、继续、录制中移动选区、停止和丢弃均由取景框控制。
- 首帧到达后即流式写入内容寻址素材和同步 journal；异常失败会保留可恢复项目路径，停止后再完成 checkpoint。
- 停止后自动创建同名 `.gfsproj`，进入带虚拟胶片条、逐帧预览、选择、排序、删除、延时和 undo/redo 的编辑器。
- 编辑器可对选中帧进行精确裁剪、缩放、90° 旋转和水平/垂直翻转，操作会写入可恢复的项目历史。
- 可按毫秒时间范围选择、保留或删除片段，并可降帧、调整时长、生成 Yoyo 往返序列和清理最终渲染结果中的重复帧。
- 支持帧级 Cut/Copy/Paste、折叠统计面板，以及把项目、GIF 或静态图片直接拖入窗口。
- 可为相邻帧创建 Fade、RGBA 颜色 Fade 和四方向 Slide，导出时按指定步数与总时长确定性生成中间帧。
- 编辑器可在后台导出全部或选中帧，支持颜色数、循环、调色板、量化、抖动、透明和差分矩形选项。
- 可打开已有 `.gfsproj`，也可从界面或启动参数把 GIF、PNG、JPEG、BMP、WebP 安全解码成新项目。

## 开发命令

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo run -p gif-from-screen-cli -- doctor
cargo run -p gif-from-screen-cli -- sources-x11
cargo run -p gif-from-screen-cli -- record-x11 capture.gif 3000 10 0 0 640 480
cargo run -p gif-from-screen-cli -- export animation.gfsproj output.gif '1,3-5,9-7'
cargo run -p gif-from-screen
cargo run -p gif-from-screen -- --project /path/to/animation.gfsproj
cargo run -p gif-from-screen -- --import-gif /path/to/animation.gif
cargo run -p gif-from-screen -- --import-image /path/to/image.png
cargo test -p gif-from-screen-capture-linux --lib --no-default-features --features native-wayland,native-x11
```
