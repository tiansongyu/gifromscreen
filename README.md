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
- Linux 以 Wayland 为第一优先级，同时提供 X11 后端；macOS 工作暂缓。

## 开发命令

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo run -p gif-from-screen-cli -- doctor
cargo run -p gif-from-screen
```
