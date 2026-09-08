# GifFromScreen

GifFromScreen 是一个使用 Rust 实现的本地 GIF 录制与逐帧编辑工具。当前先完成 Linux 版本；Linux 稳定交付后再讨论 macOS adapter。

当前仓库已经进入 Linux-first 实现阶段。

## 设计文档

- [产品与软件架构设计](docs/DESIGN.md)
- [ScreenToGif 功能对照表](docs/FEATURE_MATRIX.md)
- [ADR-0001：Rust 桌面技术栈](docs/ADR-0001-RUST-DESKTOP-STACK.md)
- [Linux 实现状态](docs/LINUX-STATUS.md)
- [Linux 性能证据](docs/LINUX-BENCHMARKS.md)
- [实际功能差距审计](docs/PARITY-AUDIT.md)
- [视频导入、工程插入与转场验收](docs/LINUX-ITERATION-VIDEO.md)
- [摄像头、画板、高级编辑与最新分发包验收](docs/LINUX-ITERATION-LIVE-SOURCES.md)
- [输入标注、自动任务与循环匹配验收](docs/LINUX-ITERATION-ANNOTATIONS.md)
- [进度与输入标注](docs/ANNOTATIONS.md)
- [自动编辑任务预设](docs/AUTOMATIC-TASKS.md)
- [标注作者范围与重编辑](docs/ANNOTATION-AUTHORING-SCOPE.md)
- [烘焙后的输入数据保护与旧工程确认](docs/CAPTURE-INPUT-BINDING.md)
- [隔离 GNOME / Portal / PipeWire 实测记录](docs/WAYLAND-NESTED-QA-RESULTS-2026-09-06.md)
- [Linux 预览包安装与构建](docs/PACKAGING.md)
- [Cinemagraph 笔迹编辑与原生验收](docs/NATIVE-CINEMAGRAPH-QA-2026-09-07.md)
- [独立 WPF 数值对照与剩余差异](docs/INK-REFERENCE-EVIDENCE.md)
- [下一轮录制控制与精确定位计划](docs/NEXT-LINUX-ITERATION.md)
- [全局录制快捷键](docs/GLOBAL-SHORTCUTS.md) 与 [X11 录制框原生验收](docs/X11-RECORDER-WINDOWS.md)

## 当前范围

- 对齐 ScreenToGif 2.43.2 的录制、逐帧编辑、项目管理和 GIF 导出能力。
- 最终输出只支持 GIF；不复制 ScreenToGif 的非 GIF 导出格式。
- 使用 Rust 原生实现，不复制 ScreenToGif 的品牌、图标或界面素材。部分底层数值算法按 MIT 许可移植自 dotnet/WPF，来源与许可见 [NOTICE](packaging/licenses/NOTICE.txt) 和 [原始许可](packaging/licenses/upstream/dotnet-wpf-MIT.txt)。
- X11 已打通“录制 → 可恢复项目 → 编辑 → GIF”桌面流程，使用与桌面物理坐标对齐的透明取景框。Wayland 遵循系统 Portal 授权，在专用控制器页面调整源内裁剪；隔离 GNOME 已实测窗口/显示器录制、暂停移动、停止导出及故障恢复。Wayland 显示器录制不能自动排除控制器，须把控制器移出选区或优先选择窗口来源。嵌套软件渲染测试不等于物理 GNOME/KDE、混合 DPI 或完整发行验收。macOS 工作暂缓。

## 当前可用流程

- X11 录制采用独立原生边框和控制面板，物理选区不再受工具条大小影响；面板优先放到选区外，全屏无空位时隐藏／最小化，恢复窗口后先暂停再显示控制。Wayland 使用独立源内裁剪控制器。[原生验收与剩余限制](docs/NATIVE-X11-SPLIT-QA-2026-09-07.md)
- 倒计时、开始、暂停、继续、录制中移动固定尺寸选区、停止和丢弃均由独立控制器完成；支持连续 FPS、按秒/分钟/小时周期快照及手动快照，不会重新打开 Portal 选择器。
- X11 控制器可聚焦“Move with arrow keys”后用方向键精确移动 1 个物理像素，Shift 移动 10 像素，Escape 退出；不会抢走文本框或其他应用的方向键。
- 可选全局录制快捷键：默认 Ctrl+Shift+F7 开框／开始／暂停／恢复，F8 停止，F9 手动快照（均带 Ctrl+Shift），可改键并保存。X11 已在隔离 Mutter 验证其他应用获焦时的完整控制序列；Wayland 使用独立 GlobalShortcuts Portal，实际可用按键以系统授权返回值为准，真实桌面验收仍待完成。快捷键默认关闭，不提供全局丢弃键。
- GIF 播放延时可独立于实际采样间隔设置；支持固定每帧延时和按实际有效录制时间计时，暂停不进入录制时间。
- 首帧到达后即流式写入内容寻址素材和同步 journal；异常失败会保留可恢复项目路径，停止后再完成 checkpoint。
- 停止后自动创建同名 `.gfsproj`，进入带虚拟胶片条、逐帧预览、选择、排序、删除、延时和 undo/redo 的编辑器。
- 编辑器可对选中帧进行精确裁剪、缩放、90° 旋转和水平/垂直翻转，操作会写入可恢复的项目历史。
- 可按毫秒时间范围选择、保留或删除片段，并可降帧、调整时长、生成 Yoyo 往返序列和清理最终渲染结果中的重复帧。
- 支持帧级 Cut/Copy/Paste、可选择/删除的有界剪贴板历史、折叠统计面板，以及把项目、GIF 或静态图片直接拖入窗口。
- 可为相邻帧创建 Fade、RGBA 颜色 Fade 和四方向 Slide；预览播放与导出共用中间帧和时间规则，暂停可保留转场位置。
- 编辑器可在后台导出全部或选中帧，支持颜色数、循环、局部/全局/自定义调色板、Wu 等量化器、完整抖动选项、透明和差分矩形。
- 可新建透明/实色空白动画，也可把一组有序、同尺寸的 PNG/JPEG/BMP/WebP 按自定义帧时长与循环策略导入项目。
- 可打开已有 `.gfsproj`，也可从界面或启动参数把 GIF、PNG、JPEG、BMP、WebP 安全解码成新项目。
- 统一 CPU 合成路径已支持定时栅格水印、线/箭头/矩形/椭圆和压力笔迹，作者工具、预览、转场端点与 GIF 导出使用一致的像素路径。
- 编辑器可在所选帧跨度上创建线、箭头、矩形或椭圆轨道，设置边界、描边/填充、透明度、混合模式和层级，并通过 Undo/Redo 管理。
- 可直接在当前帧预览上拖出一条有界自由笔迹，提交为带宽度、RGBA、透明度、混合模式和层级的帧级绘图；旧定时轨道保持原有语义。
- 可异步解码 PNG/JPEG/BMP/WebP 水印并设置位置、尺寸、双层透明度、混合模式和层级；素材注册与轨道创建可一起撤销。
- 可添加多语言字幕、重新编辑文字或插入标题帧；保存原文字参数与已排版像素，换机重开不因字体不同改变已有画面。
- 可用系统 FFmpeg/ffprobe 导入视频，设置起点、时长、FPS 和尺寸；后台流式落盘，取消后保留可恢复工程。
- 可将同画布的录制/导入工程插入当前时间线，连同源字幕和转场一起保留，并可完整撤销。
- 局部和全局调色板导出均使用有界像素工作集；全局策略通过重放分析与编码保持颜色质量，NeuQuant 必要时额外执行一次有界采样。
- 可在工程内保存、加载、更新、重命名和删除导出预设；预设不会恢复输出路径或覆盖文件授权。
- 原生文件选择器与手工路径输入均可用；从启动页可继续编辑仍然打开的工程。
- 可录制画板的画笔、荧光笔和橡皮操作，按 FPS 或完成笔划采样；摄像头入口支持显式预览、录制、暂停和保存，真实设备模式仍需硬件验收。
- Motion tools → Cinemagraph 可在首帧绘制动态区域，支持画笔、局部/整笔擦除、笔迹移动缩放及曲线拟合；后台原子应用到选中帧，保留选择间隙并支持撤销。局部擦除和连线边缘的精确 WPF 对齐仍有已记录差异。
- Rectangular freeze 是独立的当前帧矩形冻结扩展，保留原始帧和隐藏图层。Smooth loop 按首帧的精确像素相似比例、起始阈值和搜索方向寻找结尾并裁掉后续帧；追加淡化回首帧另称 Loop crossfade，均可撤销。
- X11 可显式开启按键/点击元数据采集，或保存可编辑光标；默认不采集输入，暂停确认后停止监听。Wayland 提供嵌入/隐藏光标和手工标注后备入口，不承诺被动全局输入监听。
- 可为选中帧创建、重编和保存进度条、帧号/时间标签、按键、点击和光标标注；生成结果随工程保存，并共用预览与 GIF 渲染路径。
- 可保存按顺序执行的自动编辑任务预设，在新录制/导入后应用延时、边框、阴影、进度及录制输入标注；整条任务链一次撤销，打开已有工程不会重复应用。
- 标注组保存完整作者范围，保持时间缩短后再延长也能覆盖原先未显示标注的帧。烘焙保留原始输入作为历史数据，同时阻断错误重放；旧工程需要明确、可撤销的确认，不能把未知或非录制帧默认为可信原始输入。
- 可另存工程副本且不切换当前编辑状态；最近项目列表提供缺失提示和移除入口，不会删除工程本身。

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
cargo run -p gif-from-screen -- --import-video /path/to/video.mp4
cargo test -p gif-from-screen-capture-linux --lib --no-default-features --features native-wayland,native-x11
```

视频输入需要可用的 `ffmpeg` 和 `ffprobe`。例如 Ubuntu/Debian 可安装系统 `ffmpeg` 包；没有该依赖时，屏幕录制、图片/GIF 编辑与内置 GIF 导出仍可使用。Linux 版仍处于持续开发与验收阶段，尚未宣称完整一比一复刻或零缺陷。
