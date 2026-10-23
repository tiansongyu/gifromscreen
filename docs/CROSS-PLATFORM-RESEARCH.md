---
title: GifFromScreen — source audit and platform research
status: proposed
date: 2026-10-23
code-baseline: b82dd6781f46e40dcca2bad6f5c088d959ce658e
---

# 代码审查与平台研究

核对日期：**2026-10-23**。本文件支撑[推荐架构](CROSS-PLATFORM-DESIGN.md)与[性能验收](CROSS-PLATFORM-PERFORMANCE.md)，所有路径/行号针对 b82dd67。调查包含源码、固定版本依赖、官方 API、发布清单和明确的测试/CI 结果；没有 Windows/macOS 构建、GPU 互操作或真实刷新率测量证据。

标签沿用主方案：【实现】代码事实；【验证】范围明确的测试/查询；【官方】API或维护者源码；【建议】方案；【待验】需要实验。用户现有未提交文档可能描述历史计划，不能覆盖当前代码证据。本次保留原工作区，未把这些修改纳入设计提交。

## 1. 技术栈和移植边界

【实现】workspace 0.1.6、edition2024、MSRV1.88、固定toolchain1.88.0，共14个成员。直接声明 eframe/egui0.32、wgpu25；锁文件具体为eframe/egui0.32.3、winit0.30.13、wgpu25.0.2、gif0.14.2、ashpd0.13.13、pipewire0.6.0。`windows0.58.0` 与多个objc2版本作为传递依赖存在，不证明原生应用可运行。

| 证据路径 | 已检查事实 | 对方案的影响 |
| --- | --- | --- |
| [Cargo.toml](../Cargo.toml)、[Cargo.lock](../Cargo.lock)、[rust-toolchain.toml](../rust-toolchain.toml) | wgpu关闭默认feature，仅GLES/Vulkan；工具链固定1.88.0 | Windows添加DX12、mac添加Metal；检查feature合并与MSRV，不直接升级最新版 |
| [desktop/Cargo.toml](../apps/desktop/Cargo.toml)、[cli/Cargo.toml](../apps/cli/Cargo.toml) | capture-linux为无条件依赖；desktop启用native-wayland/native-x11，CLI启用native-x11；desktop直接rustix fs/process | target-specific依赖和工厂是移植第一步，整个workspace需按目标检查 |
| [capture/backend.rs](../crates/capture/src/backend.rs) | Backend:Send+Sync、Session:Send；来源geometry可None；Region定义为源内物理裁剪；会话固定输出尺寸 | 扩展现有接口，原生对象放owner队列，通过代理满足契约 |
| [capture/capabilities.rs](../crates/capture/src/capabilities.rs)、[error.rs](../crates/capture/src/error.rs) | 能力已有四态，错误含恢复提示 | 保留权限/受限差异，勿通过布尔“支持”抹平平台限制 |
| [capture/frame.rs](../crates/capture/src/frame.rs):249 | Arc CPU像素、sequence、时间、stride、格式、damage/cursor/input | 增加clock质量和代次；GPU租约不要塞进序列化模型 |
| [preferences/store.rs](../apps/desktop/src/preferences/store.rs):3 | Unix MetadataExt、rustix openat/no-follow、UID/mode和原子提交 | 平台安全存储适配；文件已说明I/O在调用方worker，不能误称这里必然阻塞UI |
| [capture_source_job.rs](../apps/desktop/src/capture_source_job.rs)、[graphics_backend.rs](../apps/desktop/src/graphics_backend.rs)、[main.rs](../apps/desktop/src/main.rs):116 | 直接使用LinuxDisplayServer/Backend；main含Linux app-id和session检测 | 抽取Linux factory，保留NVIDIA/GL/Vulkan选择逻辑，其他平台独立实现 |
| [recent_projects.rs](../apps/desktop/src/recent_projects.rs):275、preferences与shortcuts测试 | 含Unix symlink专属测试 | cfg隔离并补Windows reparse/ACL等价测试，不简单删除安全约束 |
| [domain/lib.rs](../crates/domain/src/lib.rs):60、[project/schema_migration.rs](../crates/project/src/schema_migration.rs) | 当前schema8，有旧版/未知版本迁移与拒绝路径 | 新spool/manifest显式版本，旧数据不能被静默破坏 |
| [project/atomic_file.rs](../crates/project/src/atomic_file.rs):62、[private_fs.rs](../crates/project/src/private_fs.rs)、[application/project_copy.rs](../crates/application/src/project_copy.rs):140 | sync_directory直接File::open目录并sync_all；private_fs的权限设置仅cfg(unix)，其他平台没有显式ACL等价实现 | 不只是desktop配置文件需要适配；Windows目录handle/flush与原子替换、mac文件系统持久化及权限均需native验证，这里是运行时风险而非已测编译错误 |

【建议】保留domain/editor/project/gif/render/localization/text/media的共享逻辑，但仍需各目标编译/运行验证；“没有明显平台API”只是移植候选，不是已通过平台认证。

## 2. 录制和预览热路径

### 2.1 实际调用链

【实现】[main.rs](../apps/desktop/src/main.rs):3045启动录制worker，3046用无界mpsc传进度；6643/6808附近创建incremental project sink并调用workflow，6695附近Wayland使用prepared session走相同sink-only路径。采集不在UI上，但worker内部仍串行执行：

```text
session.poll_frame
  → collect_session_to_sink / observe_frame
  → normalize / retention / 上帧时长修正
  → append_sink_frame
  → IncrementalRecordingProject
  → AssetStore hash + atomic write + 完整读回校验
  → journal sync / 定期checkpoint
  → 下一次poll
```

| 环节 / 路径 | 代码事实 | 影响与处理建议 |
| --- | --- | --- |
| [workflow/collect.rs](../crates/workflow/src/collect.rs):692、746、834 | observe_frame/sink同步；先修正上一帧时长再追加当前帧 | 盘/校验阻塞采集和控制；异步writer需有顺序、封口和durable ACK |
| collect.rs:1468 | DurationReached按精确limit封口；普通stop用tail_duration加observed/retained差；Fixed时长单独处理 | 按真实stop边界封口是建议改进，不是当前普通stop已有保证 |
| collect.rs:1071、1321 | tight RGBA仍逐行新分配复制；sink再clone为RgbaFrame；BGRA逐像素转换 | 共用不可变像素与池，规范格式只在必要时转换 |
| [application/recording_project.rs](../crates/application/src/recording_project.rs):312、348、352 | raw RGBA资产写入并journal；默认checkpoint间隔512帧 | 像素RAM有界但磁盘量与metadata增长；60FPS名义约8.53秒一次checkpoint |
| [project/asset_store.rs](../crates/project/src/asset_store.rs):38、55 | 写前hash，写后完整读回/再hash | 增加内存带宽与I/O；优化不能直接删完整性保证 |
| [project/atomic_file.rs](../crates/project/src/atomic_file.rs):25、[journal.rs](../crates/project/src/journal.rs):168 | 资产write/sync/rename/目录sync，journal sync_data | durable语义有价值，批次策略须明确恢复尾部 |
| [project/active.rs](../crates/project/src/active.rs):405、530、677 | 时长修正追加日志；checkpoint校验和全manifest序列化/替换 | 后台不可变revision准备与有序提交；不能让旧checkpoint覆盖新journal |
| [workflow/control.rs](../crates/workflow/src/control.rs):286 | 控制channel无界，但已有target coalescing、snapshot burst限制等 | 改为命令容量/保留终态槽与最新快照，不笼统断言当前无限堆帧 |

【实现】桌面使用 `collect_controlled_to_sink / collect_prestarted_controlled_to_sink`，像素工作集主要是当前/上一帧；一般collect API才可能保留整个RGBA序列。**不能把当前桌面描绘为把十分钟所有像素保存在RAM。** 帧索引、manifest、frame-id等metadata会随长度增长，仍需计量。

### 2.2 X11、Wayland 与录制框

| 路径 | 【实现】实际行为 | 风险 / 建议 |
| --- | --- | --- |
| [capture-linux/x11.rs](../crates/capture-linux/src/x11.rs):289–350 | core GetImage读取root可见像素，decode新RGBA，越界padding可再分配；非MIT-SHM/XDamage | 先测捕获/转换/落盘占比，SHM独立实验；window选择不等于获取被遮挡窗口backing内容 |
| x11.rs:694、786、801 | deadline跳过错过的槽；采集前active Instant估计；暂停时间扣除 | 非compositor同步PTS；poll timeout不证明后续同步X11请求一定及时返回 |
| [recorder_guide/native.rs](../crates/capture-linux/src/recorder_guide/native.rs):155、[direct_drag.rs](../crates/capture-linux/src/recorder_guide/native/direct_drag.rs):16 | 独立worker合并native事件并直接呈现拖动 | 已有快路径，不能把新增16ms repaint当作解决方案 |
| [retarget.rs](../apps/desktop/src/retarget.rs):36、[x11_recorder.rs](../apps/desktop/src/x11_recorder.rs):983 | 一次在途、latest pending；录制中移动暂停→retarget/排除ACK→恢复 | 跟手与移动时连续采集分别验收；固定canvas保留 |
| [wayland_pipewire.rs](../crates/capture-linux/src/wayland_pipewire.rs):269、1053 | sync_channel(3)；映射原生buffer→CPU copy/crop/convert→guard释放 | 已有有限内存，但复制与CPU时间仍需优化 |
| wayland_pipewire.rs:1102、1126、1175 | sequence在尝试交付前递增；满队列丢**新帧**，Dropped分支未统计 | 老帧留队可能增加年龄；补delivery loss、过载终态；不能称无损录制或latest-preview策略 |
| wayland_pipewire.rs:1154、1288、1416 | callback active Instant而非SPA PTS；支持MemPtr/MemFd，拒DmaBuf；新Vec逐行转换 | 原生PTS/quality与CPU池；GPU路径单独测 |
| wayland_pipewire.rs:499、764；[wayland_native_buffer.rs](../crates/capture-linux/src/wayland_native_buffer.rs):105 | 恢复清旧队列、ActiveClock扣暂停，native guard回收buffer | 保留新帧边界，GPU租约改变后也不能耗尽原生pool |
| [wayland_prepare_job.rs](../apps/desktop/src/wayland_prepare_job.rs):59、515 | 来源预览是首帧冻结图，native session暂停后裁剪 | 不能把它当作已实现低延迟实时录制预览 |

### 2.3 UI 与 GIF 已有能力

- 【实现】[main.rs](../apps/desktop/src/main.rs):4357 → [editor_preview.rs](../apps/desktop/src/editor_preview.rs):380、585、628，在cache miss时同步读资产、CPU渲染、缩放和texture上传；默认16项/64MiB缓存，render limit512MiB。建议后台latest-request/revision取消。
- 【实现】[thumbnail_cache.rs](../apps/desktop/src/thumbnail_cache.rs):85已有有界请求worker与detached immutable plan，可复用；不是所有预览都尚未异步。
- 【实现】[editor_workspace.rs](../apps/desktop/src/editor_workspace.rs):666、1047，部分编辑/持久化路径同步执行；[editor_ui.rs](../apps/desktop/src/editor_ui.rs):1743为调用例。需要测量后迁移成有序job，保留undo/redo语义。
- 【实现】[export_job.rs](../apps/desktop/src/export_job.rs):98、171已有后台可取消不可变快照，UI drop不join；[project_export.rs](../crates/application/src/project_export.rs):732与[streaming.rs](../crates/application/src/project_export/streaming.rs):153按当前帧/transition端点流式渲染，global palette可replay。无需从零重写导出。
- 【实现】[gif/encoder.rs](../crates/gif/src/encoder.rs):260重复RGBA合并，708变化矩形，727透明/disposal；默认FullFrames，desktop可选delta。Encoder/quantizer额外buffers不在render预算内，不能把render cap称为总RSS。
- 【实现】[gif/timing.rs](../crates/gif/src/timing.rs):1累计u128舍入，16,667µs可分配2/1/2 tick；长序列量化误差测试已存在。见§6播放器限制。
- 【实现】[project_export.rs](../crates/application/src/project_export.rs):1067取消复查、sync和原子发布；失败/取消保留已有输出是重构不变量。

## 3. 当前验证与 CI 边界

【验证】2026-10-23查询GitHub，以下结果绑定具体commit，未来run不自动继承：

| 记录 | 结果与证明范围 |
| --- | --- |
| [Linux CI 39915589966](https://github.com/tiansongyu/gifromscreen/actions/runs/39915589966)，b82dd67 | success；Ubuntu24.04 fmt/clippy/tests，Ubuntu22.04 native feature检查与隔离Xvfb测试；非物理60FPS |
| [Portable 39915589938](https://github.com/tiansongyu/gifromscreen/actions/runs/39915589938)，b82dd67 | success；Linux包构建、重复打包、安装测试、包内应用的Xvfb smoke；非Wayland权限或实际显示验收 |
| [Pages 39915589958](https://github.com/tiansongyu/gifromscreen/actions/runs/39915589958)，b82dd67 | success；只证明站点部署 |
| [WPF pixel reference 39910681006](https://github.com/tiansongyu/gifromscreen/actions/runs/39910681006)，fd48baa | failure；是C#像素参考生成/比较，**不是Rust Windows应用测试**；本方案不声称所有workflow全绿 |

工作流依据：[ci.yml](../.github/workflows/ci.yml)、[portable.yml](../.github/workflows/portable.yml)、[wpf-reference.yml](../.github/workflows/wpf-reference.yml)、[cinemagraph-probe.yml](../.github/workflows/cinemagraph-probe.yml)。当前没有Windows Rust桌面和macOS native构建/采集流程。

【验证】本轮本机执行workflow 26 unit +42 integration通过，GIF timing 2 unit通过；workspace `--list`成功但不是全量测试执行。仅安装Linux x86_64 Rust target，PipeWire pkg-config为0.3.48。重现命令和局限见 [Performance §6–7](CROSS-PLATFORM-PERFORMANCE.md#6-现有回归命令与新增门槛)。

【实现】有意义的既有测试包括：

| 位置 | 覆盖与边界 |
| --- | --- |
| [workflow/tests/roundtrip.rs](../crates/workflow/tests/roundtrip.rs) | padded BGRA/PTS、去重保持时长、sink-only、pause时长、stalled manual控制、stop尾帧/discard；不是硬件采集速度 |
| [workflow/control.rs](../crates/workflow/src/control.rs) | pause/target/resume ACK顺序、target合并、终态竞争、snapshot burst cap |
| [application/recording_project.rs](../crates/application/src/recording_project.rs) | drop恢复、dedup、checkpoint、10,000帧有界replay尾部；不等于1080p十分钟性能 |
| [application/project_export.rs](../crates/application/src/project_export.rs) | streaming/local/global、累计>512MiB像素在12MiB render预算下处理、取消/文件变化/原子输出；累计处理量不是同时驻留量 |
| [gif/tests/roundtrip.rs](../crates/gif/tests/roundtrip.rs) | 解码、重复帧时长、delta/透明清除、取消和buffer limit |
| [x11.rs](../crates/capture-linux/src/x11.rs)、[wayland_pipewire.rs](../crates/capture-linux/src/wayland_pipewire.rs) | cadence跳槽/固定canvas/pause；resume/snapshot新帧边界；ignored native测试只部分由隔离CI执行 |
| [recorder_geometry.rs](../apps/desktop/src/recorder_geometry.rs)、[retarget.rs](../apps/desktop/src/retarget.rs)、[main.rs](../apps/desktop/src/main.rs) | 物理几何、极端原点、固定尺寸、latest pending、首页大小恢复；不是跨平台所有DPI已测 |
| [editor_shell_layout_tests.rs](../apps/desktop/src/editor_shell_layout_tests.rs):686 | page_text_layout_audit覆盖EN/ZH路由/宽度/缩放文字检查；保留并补三平台真实字体与窗口验证 |

## 4. Windows

以下是【官方】资料及【建议】落地边界，核对日期2026-10-23。产品建议先验收Windows11 x64；ARM64另立等级。没有native build/采集/性能结果。

| 主题 | 官方事实 / 设计影响 | 来源 |
| --- | --- | --- |
| 选择器与最低API | WGC/Picker从Win10 1803；桌面picker先关联owner HWND，取消为正常流程；先查IsSupported | [Picker](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturepicker?view=winrt-26100)、[desktop ownership](https://learn.microsoft.com/en-us/windows/apps/develop/ui/display-ui-objects) |
| 线程与帧池 | CreateFreeThreaded从1809，无DispatcherQueue依赖，FrameArrived在内部worker；回调只做有界handoff | [CreateFreeThreaded](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded?view=winrt-26100) |
| monitor/window来源 | HWND/HMONITOR interop从1903；只在用户明确选择来源后使用 | [CreateForWindow](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow)、[CreateForMonitor](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createformonitor) |
| 排除 | WDA_EXCLUDEFROMCAPTURE从2004，own-process top-level HWND/DWM；旧系统表现为WDA_MONITOR；返回值与实拍都需核验 | [SetWindowDisplayAffinity](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity) |
| GPU/内容范围 | WGC D3D11，按ContentSize使用有效区域；Recreate会丢旧pool帧；BGRA8 SDR不保证HDR正确，HDR需独立转换 | [Screen capture](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture) |
| 时钟/关闭 | SystemRelativeTime基于QPC但以TimeSpan的100ns单位暴露，不能再按QPC frequency除；Closed可能因HWND替换触发，关闭/设备错误保留数据并重选 | [SystemRelativeTime](https://learn.microsoft.com/sv-se/uwp/api/windows.graphics.capture.direct3d11captureframe.systemrelativetime?view=winrt-17763)、[TimeSpan](https://learn.microsoft.com/en-us/uwp/api/windows.foundation.timespan?view=winrt-26100)、[Closed](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscaptureitem.closed?view=winrt-26100) |
| 输入与DPI | SetCapture有前台/线程约束；WM_CAPTURECHANGED终止手势；PerMonitorV2/WM_DPICHANGED需处理 | [SetCapture](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setcapture)、[capture changed](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-capturechanged)、[DPI](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows) |
| 快捷键 | RegisterHotKey/MOD_NOREPEAT，报告冲突并unregister；不同于记录全部键盘输入 | [RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey) |
| D3D互操作 | D3D11 capture表面不直接等于DX12 texture；shared resource/fence/device/adapter需独立实验 | [D3D12 interop](https://learn.microsoft.com/en-us/windows/win32/direct3d12/direct3d-12-with-direct3d-11--direct-2d-and-gdi) |
| bindings/MSRV | 已锁windows0.58.0 manifest要求Rust1.70；最新GitHub release **tag74**内windows0.100.0要求1.95，windows-sys0.100.0要求1.88 | [0.58 manifest](https://github.com/microsoft/windows-rs/blob/0.58.0/crates/libs/windows/Cargo.toml)、[release74](https://github.com/microsoft/windows-rs/releases/tag/74)、[windows](https://github.com/microsoft/windows-rs/blob/74/crates/libs/windows/Cargo.toml)、[sys](https://github.com/microsoft/windows-rs/blob/74/crates/libs/sys/Cargo.toml) |

【实现】locked winit0.30.13的Windows路径使用OleInitialize/STA。不能把UI线程强制改MTA；picker/owner HWND与capture worker的COM初始化/清理分别管理。**声明MSRV兼容只是绑定候选依据，完整feature图尚未编译。**

【建议】pool初始3帧、复用staging ring、有界owned纹理/CPU租约；native frame归还前完成必要拷贝或严格保证引用有效。no-activate overlay与SetCapture不能未经原型就承诺完整拖动。系统capture border先保留，borderless不是MVP前提。

## 5. macOS

【官方】【建议】SCK基础API从macOS12.3；picker、filter的contentRect/pointPixelScale及现代AppKit display link从14.0，建议产品14+。Apple当前示例要求15/Xcode16不等于全部API最低要求。两架构必须各自native验证。[WWDC2023](https://developer.apple.com/videos/play/wwdc2023/10136/)、[picker](https://developer.apple.com/documentation/screencapturekit/sccontentsharingpicker)、[示例](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)。

| 主题 | 官方事实 / 设计影响 | 来源 |
| --- | --- | --- |
| 回调与节奏 | output queue回调验证sample/status；显式设minimumFrameInterval=1/fps、queueDepth先3，不超过Apple建议8；间隔约束不保证独特画面数 | [output](https://developer.apple.com/documentation/screencapturekit/scstreamoutput/stream(_:didoutputsamplebuffer:of:))、[interval](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/minimumframeinterval)、[queue](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/queuedepth) |
| 状态与时钟 | complete是有效新像素，idle是未变化，blank/suspended/stopped需区分；CMSampleBuffer PTS保留有理数，displayTime非物理显示证明 | [status](https://developer.apple.com/documentation/screencapturekit/scframestatus)、[frame info](https://developer.apple.com/documentation/screencapturekit/scstreamframeinfo)、[PTS](https://developer.apple.com/documentation/coremedia/cmsamplebuffergetpresentationtimestamp(_:)) |
| 几何 | contentRect为点，pointPixelScale描述像素换算；AppKit/CG原点和源内裁剪分别转换，不硬编码Retina2倍 | [contentRect](https://developer.apple.com/documentation/screencapturekit/sccontentfilter/contentrect)、[pointPixelScale](https://developer.apple.com/documentation/screencapturekit/sccontentfilter/pointpixelscale) |
| 更新边界 | configuration/filter异步更新；completion不应未经验证当作首帧代次证明，需要PTS/metadata检查或暂停重建边界 | [update configuration](https://developer.apple.com/documentation/screencapturekit/scstream/updateconfiguration(_:completionhandler:))、[update filter](https://developer.apple.com/documentation/screencapturekit/scstream/updatecontentfilter(_:completionhandler:)) |
| 权限与选择器 | picker session授权不等于任意枚举权限；取消picker不自动停止已有流；回调分别处理选择/取消/失败 | [picker observer](https://developer.apple.com/documentation/screencapturekit/sccontentsharingpickerobserver)、[WWDC授权流程](https://developer.apple.com/videos/play/wwdc2023/10053/) |
| 排除与停止 | SCContentFilter可排除显示器中的指定窗口/应用；picker不可选项不等于像素排除；didStopWithError保留已录数据 | [exclude windows](https://developer.apple.com/documentation/screencapturekit/sccontentfilter/init(display:excludingwindows:))、[exclude apps](https://developer.apple.com/documentation/screencapturekit/sccontentfilter/init(display:excludingapplications:exceptingwindows:))、[stop](https://developer.apple.com/documentation/screencapturekit/scstreamdelegate/stream(_:didstopwitherror:)) |
| GPU生命周期 | CVPixelBuffer/IOSurface→CVMetalTextureCache候选；CVMetalTexture必须持有至GPU完成，仅retain MTLTexture不足以证明lease安全 | [texture cache](https://developer.apple.com/documentation/corevideo/cvmetaltexturecachecreatetexturefromimage(_:_:_:_:_:_:_:_:_:)) |
| 原生拖动与主线程 | performDrag将移动交给WindowServer、可立即返回，mouse-up不保证；AppKit操作留主线程 | [performDrag](https://developer.apple.com/documentation/appkit/nswindow/performdrag(with:))、[thread safety](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/ThreadSafetySummary/ThreadSafetySummary.html) |
| 刷新与快捷键 | display link关联显示器/窗口；被动NSEvent全局键盘监控要求Accessibility信任，与普通注册组合键区分 | [display link](https://developer.apple.com/documentation/appkit/nsscreen/displaylink(target:selector:))、[global monitor](https://developer.apple.com/documentation/appkit/nsevent/addglobalmonitorforevents(matching:handler:)) |
| bindings/MSRV | objc2 0.6.4、objc2-screen-capture-kit0.3.2均声明Rust1.71；不混用不同wrapper版本传对象；完整构建待验 | [objc2清单](https://crates.io/api/v1/crates/objc2/0.6.4)、[SCK清单](https://crates.io/api/v1/crates/objc2-screen-capture-kit/0.3.2) |

【待验】Apple网页称单window捕获忽略sourceRect；SDK生成的绑定注释描述子区域时又提及independent window，默认interval/queueDepth描述也有差异。**这是文档歧义，不是窗口裁剪可用的证据。** 显式设值，以目标SDK/OS实验为门槛，保留应用侧crop。[Apple sourceRect](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/sourcerect)、[固定0.3.2生成源码](https://docs.rs/objc2-screen-capture-kit/0.3.2/src/objc2_screen_capture_kit/generated/SCStream.rs.html)。

【建议】MVP普通组合快捷键；若使用global-hotkey绑定，先固定并审查版本。维护者当前源码普通键走Carbon、媒体键走event tap，不能对整库承诺无权限要求。[维护者实现](https://github.com/tauri-apps/global-hotkey/blob/dev/src/platform_impl/macos/mod.rs)。

## 6. 渲染、GIF 与上游版本

| 研究对象 | 已核对事实与边界 | 官方/维护者来源 |
| --- | --- | --- |
| wgpu25.0.2呈现 | FIFO支持所有后端、可能等待surface；AutoVsync/AutoNoVsync有fallback顺序，模式可用性依平台；不等于端到端延迟保证 | [固定版本PresentMode源码](https://github.com/gfx-rs/wgpu/blob/v25.0.2/wgpu-types/src/lib.rs) |
| HAL导入 | create_texture_from_hal是unsafe，要求相同底层device、初始化与descriptor兼容；Metal texture_from_raw仍要遵守资源生命周期 | [Device API](https://docs.rs/wgpu/25.0.2/wgpu/struct.Device.html#method.create_texture_from_hal)、[Metal固定源码](https://github.com/gfx-rs/wgpu/blob/v25.0.2/wgpu-hal/src/metal/device.rs#L308) |
| GPU完成回调 | on_submitted_work_done需通过后续submit/poll等驱动回调；预览隐藏/停止时仍需completion泵，避免lease不释放耗尽原生pool | [Queue回调](https://docs.rs/wgpu/25.0.2/wgpu/struct.Queue.html#method.on_submitted_work_done) |
| GIF89a | Graphic Control Extension延迟单位百分之一秒，不能均匀表达16.666…ms | [规范§23](https://www.w3.org/Graphics/GIF/spec-gif89a.txt) |
| Chromium当前源码 | DeferredImageDecoder::FrameDurationAtIndex将≤10ms延长到100ms；核对日2026-10-23，非稳定版实播结果 | [官方源码](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/platform/graphics/deferred_image_decoder.cc) |
| Firefox当前源码 | FrameTimeout::FromRawMilliseconds将0–10ms改为100ms；同样只是源码事实 | [官方源码](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/image/FrameTimeout.h) |

【建议】30/50FPS兼容配置做显式时间重采样，保留原始PTS和专家原始时序配置。Safari/系统查看器需实际播放验证，不从Chromium/Firefox源码推断其他实现。文件累计误差、重采样误差、播放器延迟和后台节流分别报告。

## 7. Linux 协议与发布资料

| 研究对象 | 【官方】事实 / 【建议】约束 | 来源 |
| --- | --- | --- |
| MIT-SHM | 可避免通过X11 IPC通道传输大图像，需查询扩展、服务器支持与内存布局；不可假设远程/所有server可用 | [X.Org MIT-SHM](https://www.x.org/releases/current/doc/xextproto/shm.html) |
| Portal ScreenCast | CreateSession→SelectSources→Start→OpenPipeWireRemote；用户授权来源，position/size属compositor坐标不必等于物理像素；当前文档v6不等于旧系统v6 | [ScreenCast协议](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html) |
| Portal恢复与stream身份 | restore token单次使用，新session返回新token；persist模式需协商；v6新增pipewire-serial/旧node-id targeting弃用，旧Portal应feature-detect | [ScreenCast lifecycle/persistence/streams](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html) |
| PipeWire元数据 | spa_meta_header包含pts/seq，PTS为纳秒；媒体/元数据是否存在与旧0.3.48支持需验证；缺失时标记估计质量 | [SPA header](https://docs.pipewire.org/structspa__meta__header.html) |
| Wayland快捷键 | GlobalShortcuts通过session/bind/activated信号，支持依Portal后端；不请求全局键盘监听替代；不可用时允许应用焦点内快捷键/工具栏 | [GlobalShortcuts协议](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html) |
| 当前Linux安装 | HTTPS脚本、包内installer、升级/rollback和保护修改已经存在，应测试而非重写为“首次支持升级” | [installer.sh](../scripts/gifromscreen-installer.sh)、[installer.py](../packaging/linux/installer.py)、[README安装说明](../README.md#install) |
| Windows分发 | MSIX需签名并在设备受信任，timestamp签名；App Installer可设置更新但系统/企业策略可覆盖；包内文件不按portable直接替换 | [MSIX签名](https://learn.microsoft.com/en-us/windows/msix/package/signing-package-overview)、[App Installer](https://learn.microsoft.com/en-us/windows/msix/app-installer/auto-update-and-repair--overview)、[包文件隔离](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes) |
| macOS分发 | DeveloperID、hardened runtime、适当entitlements、notarization/staple；deployment target和bundle最低版本一致；需账号凭据，unsigned开发包不是正式发行验收 | [DeveloperID](https://developer.apple.com/developer-id/)、[公证](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)、[Rust1.88 target配置](https://github.com/rust-lang/rust/blob/1.88.0/src/doc/rustc/src/platform-support/apple-darwin.md)、[LSMinimumSystemVersion](https://developer.apple.com/documentation/bundleresources/information-property-list/lsminimumsystemversion) |
| CI机器边界 | 托管runner的OS/架构/GPU/交互权限有限，Windows Server runner不等于Windows11物理桌面 | [GitHub runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) |

【待验】本文件不为尚未实现的spool压缩器、额外性能工具或updater指定未经研究的库。选择这些组件时，沿用同一方法核对固定版本、MSRV、线程/生命周期、失败语义、许可和native构建，并用代表性输入证明收益。
