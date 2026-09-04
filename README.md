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
- 当前 Linux X11 已打通录制、动态移动取景框、项目编辑和 GIF 导出；Wayland Portal/PipeWire adapter 是后续发行门槛。macOS 工作暂缓。

## 当前可用流程

- 录制时主界面隐藏，只保留独立、置顶的取景框和控制条。
- 倒计时、开始、暂停、继续、录制中移动选区、停止和丢弃均由取景框控制。
- 停止后自动创建同名 `.gfsproj`，进入带虚拟胶片条、逐帧预览、选择、排序、删除、延时和 undo/redo 的编辑器。
- 编辑器可在后台导出全部或选中帧，支持颜色数、循环、调色板、量化、抖动、透明和差分矩形选项。
- 可打开已有 `.gfsproj`，或把 GIF 安全解码成新项目；静态 PNG/JPEG/BMP/WebP 的有界解码与项目持久化也已具备。

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
```
