# ADR-0001：Rust 桌面技术栈

状态：Proposed  
日期：2026-09-04

## Context

GifFromScreen 需要在 macOS 与 Linux 上提供：

- 高频屏幕帧预览。
- 多窗口与透明录制覆盖层。
- 自定义逐帧时间线、裁剪框、绘图画布和复杂属性面板。
- 对 ScreenCaptureKit、XDG Portal、PipeWire、X11 和摄像头 API 的原生访问。
- 长时间运行、严格内存上限、后台 GIF 编码与取消。
- 以 Rust 作为主要且尽可能唯一的应用实现语言。

## Decision

### UI 与窗口

选择 `egui + eframe`，默认使用 wgpu backend。录制覆盖层若受 eframe viewport 能力限制，则只把该窗口下沉为 `winit + egui-winit + egui-wgpu`，不改变应用核心。

约束：

- 所有 egui 类型只存在于 `ui-egui`。
- 锁定经过 S0 验证的 minor version，升级必须通过 UI 与多窗口回归测试。
- 不追求系统原生控件外观，建立自己的工具型设计系统。
- UI 只发出 intent/command，不直接调用捕获、存储或编码器。

### 图形与渲染

- wgpu：UI 和预览纹理。
- `image + fast_image_resize + tiny-skia + cosmic-text`：确定性 CPU export renderer。
- GPU 输出不是导出语义真值。

### 平台捕获

- 自有 `CaptureBackend`/`CaptureSession` contract。
- macOS：`screencapturekit` 封装 ScreenCaptureKit；必要时在 adapter 内下沉 `objc2`。
- Wayland：`ashpd` 管理 XDG Portal；`pipewire-rs` 在独立线程消费帧。
- X11：`x11rb`，启用 MIT-SHM、Damage、Fixes、RandR、XInput 等所需扩展。
- 摄像头：自有 `CameraBackend`；首选 `nokhwa`，但以真实设备验证决定是否保留。

### GIF

- `GifEncoder` 是应用 port。
- 默认实现以 `image-gif`/`gif` 的 GIF89a/LZW 能力为底座，自行实现 palette、quantization、dither、delta、disposal 与 timing pipeline。
- Gifski 是可选 adapter，不进入默认发行，直到许可证决策完成。
- FFmpeg 是可选 sidecar，主要用于视频导入和兼容对照。

### 并发

- winit/egui 固定 main thread。
- 平台 capture callback/thread 只生产 frame packet。
- 有界 channel 提供背压。
- 单 writer actor 串行提交项目。
- Tokio 处理 Portal 与异步编排。
- Rayon 处理 CPU 密集工作。

## Alternatives

### iced

优点：纯 Rust、Elm 模型、wgpu、异步语义清晰。  
不选原因：高度定制的时间线和多录制浮层需要更多自定义 widget/message 样板；作为第二选择保留。

### Slint

优点：稳定的声明式 UI、桌面体验精致。  
不选原因：引入 `.slint` DSL；GPL/royalty-free/commercial 许可会提前约束产品发行；定制像素编辑器不是最强项。

### Tauri 2

优点：成熟 Web UI 生态和打包体验。  
不选原因：前端不是 Rust；系统 WebView 在 macOS/Linux 不同；高频帧跨 IPC/WebView、透明覆盖层和像素一致性增加风险。

### raw winit + wgpu

优点：控制力最大。  
不选原因：需要自行实现控件、文本、IME、可访问性和完整 UI shell。只对确有需要的覆盖层局部使用。

### 单一跨平台捕获 crate

优点：原型快。  
不选原因：完整 parity 需要 portal restore token、cursor metadata、damage、区域/DPI、权限状态与输入事件等细粒度控制。可用 `scap` 做 S0 对照，但不能让其类型进入核心接口。

### GStreamer 作为统一媒体层

优点：平台和格式广。  
不选原因：macOS/Linux runtime 与插件打包体积、许可矩阵和部署复杂度偏高；本产品只输出 GIF。仅在摄像头或视频导入的 S0 验证失败时重新评估。

## Consequences

正面结果：

- 产品主体保持 Rust。
- 核心可以通过 fake adapter 做确定性测试。
- macOS、Wayland 和 X11 可独立演进。
- GIF 编码许可证和实现可替换。
- UI 框架变化不污染领域模型。

代价与风险：

- egui 不提供系统原生外观，需要自行维护视觉和 a11y。
- Linux 必须维护 Wayland 与 X11 两条捕获路径。
- image-gif 只提供容器基础，高质量 palette/delta pipeline 工作量较大。
- 确定性 CPU renderer 与 GPU preview 需要严格一致性测试。
- macOS 权限、Linux Portal/backend 差异必须依赖真机 CI。

## Validation gates

ADR 只有在 S0 全部通过后才转为 Accepted：

1. macOS 区域/窗口捕获、Retina 和排除自身窗口可用。
2. GNOME 与 KDE Wayland 的 Portal/PipeWire 路径可用。
3. X11 MIT-SHM 与无 SHM fallback 可用。
4. egui 多 viewport 覆盖层可选择区域且不出现在捕获中。
5. 50,000 帧虚拟时间线仍可交互。
6. 4K@15 长录制没有无界内存增长。
7. 内置 GIF encoder 的 timing、透明和 disposal 通过回解码测试。
8. built-in/Gifski/FFmpeg corpus 结果足以确定默认质量策略。

## Primary references

- [egui](https://github.com/emilk/egui)
- [winit](https://github.com/rust-windowing/winit)
- [wgpu](https://wgpu.rs/)
- [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)
- [screencapturekit Rust crate](https://docs.rs/screencapturekit/latest/screencapturekit/)
- [ashpd](https://github.com/bilelmoussaoui/ashpd)
- [XDG ScreenCast Portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [pipewire-rs](https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/index.html)
- [x11rb](https://github.com/psychon/x11rb)
- [image-gif](https://github.com/image-rs/image-gif)
- [Gifski licensing](https://github.com/ImageOptim/gifski#license)

