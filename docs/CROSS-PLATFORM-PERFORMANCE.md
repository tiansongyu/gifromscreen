---
title: GifFromScreen — performance budget and qualification protocol
status: proposed
date: 2026-10-23
code-baseline: b82dd6781f46e40dcca2bad6f5c088d959ce658e
---

# 性能预算与跨平台验收

本文是[跨平台设计](CROSS-PLATFORM-DESIGN.md)的测量协议。**下面的性能数值均为【建议】目标，尚无真实硬件数据证明达到。** 已完成的代码/测试调查单列在 §7；通过模拟、Xvfb 或普通 CI 不构成物理显示性能验收。

## 1. 测量口径与记录

每份结果必须绑定：commit、dirty diff/hash、release/debug、Rust/SDK/依赖版本、后端/feature、OS/内核、桌面/合成器/Portal/PipeWire、CPU、GPU/驱动/adapter、RAM、磁盘/文件系统/剩余空间、电源模式、显示器数量/分辨率/实际刷新率/VRR/缩放/方向、窗口位置、来源类型/尺寸、ROI、请求采集 FPS、预览设置、导出配置和场景。

主性能测试使用 `--release --locked` 的固定构建，关闭无关负载并记录温度/节流情况。保留 debug 和指标关闭的对照，量化采样开销。日志不逐帧同步写盘；采用有界环形 trace/计数器，按批次后台输出。测量器溢出要计数，不能以丢失的慢样本改善分位数。

### 1.1 三层延迟

| 层 | 时间点与指标 | 能证明什么 |
| --- | --- | --- |
| 应用 | 收到输入 → 原生位置请求/渲染提交；UI 开始/结束；native PTS → 收到/预览/存储 | 应用处理与排队开销，不能证明像素已经显示 |
| 窗口服务器/合成器 | 请求 → configure/geometry ACK；可用的平台 present trace | 提交和系统确认；X11 round-trip、Wayland configure 都不是 photon 时间 |
| 物理屏幕 | 可见输入标记/硬件触发 → 录制框实际位移 | 指定设备的端到端响应，需高速摄影或等价硬件与误差记录 |

跨时钟域只能在建立并说明映射后相减。记录`native_pts`、clock domain/quality/basis、映射误差、received、enqueued、preview submit、writer accepted、durable和control ACK；保留raw tick和转换参数，避免将wall clock的NTP跳变带入录制时长，避免旧适配器session-active时间被二次扣除暂停。

物理验证建议相机速率至少为显示刷新率的 4 倍，同时拍到输入触发标记与边框，报告相机曝光/采样误差和显示扫描影响。无此设备时可交付应用/server 层结果，但物理门槛保持“未测”，不能推算达标。

### 1.2 四种帧率及计数

| 名称 | 定义 | 不能混用的概念 |
| --- | --- | --- |
| UI FPS | 有脏状态时实际呈现/可观测帧间隔 | repaint 请求次数不是显示 FPS；空闲不必持续 60/144 FPS |
| Capture FPS | 有效采集样本数 / 活动采集时长；同时记录原生帧间隔 | 静止/idle 的低回调不等于丢帧；重复采同一动画帧不等于独特画面更多 |
| Preview FPS / age | 实际预览更新率、最新可见帧对应 PTS 的帧龄 | 替换旧预览不等于录制丢帧；禁止队列无限延长帧龄 |
| GIF FPS / duration | 文件延迟分布与总时长；播放器实际切换节奏另测 | 文件里平均 60 帧/秒不保证浏览器按 60 FPS 播放 |

【建议】每个 source generation 单独计数，记录来源帧、应用收到帧、预采样拒收、正常采样保留、重复帧合并、归档已接收、已持久化、预览替换、过载丢弃、暂停/代次过时拒收。序号缺口独立列出，可归因到应用生产者时才记已知应用丢帧；OS 未公开的内部损失为 unknown。

给每阶段定义输入/输出和终止时排空规则，以守恒检查验证：入口观察数 = 采样跳过 + 暂停/过时代次拒收 + 接收 + 已知入口失败；接收 = 去重合并 + 待写/写入 + 后续显式失败。分支共享同一帧不重复算采集数。静止来源的持续时间通过 PTS/stop 边界保存，不捏造原生帧。

## 2. 待验证预算

设显示周期 `Td = 1000 / refresh_hz` ms，采集周期 `Tc = 1000 / requested_fps` ms。60/120/144 Hz 的 Td 分别约 16.67/8.33/6.94 ms。以下是设计起始预算，阶段 A 以后可按证据修订，修改需保留原目标和理由。

| 指标 | 起始目标【建议】 | 测量与限定 |
| --- | --- | --- |
| 活跃 UI CPU 帧工作量 | P95 ≤ 0.5 Td，P99 ≤ Td | 不把 GPU/合成器等待算成 CPU 工作消失；另报完整帧时间 P50/P95/P99 |
| 活跃呈现帧间隔 | P95 ≤ 1.5 Td，P99 ≤ 2 Td | 拖动/连续动画时测，报告 >2 Td 的次数、最长连续停顿及 missed-vblank 证据 |
| 输入→原生 guide 提交 | P95 ≤ Td，P99 ≤ 2 Td | 应用层目标；final commit 必须可靠，不能通过只测第一条 move 隐藏积压 |
| 输入→物理可见位置 | P95 ≤ 2 Td，P99 ≤ 3 Td | 真实屏幕单独测；报告相机误差、平台限制与尾延迟，不将 server ACK 填入此列 |
| 1080p60 动态 fixture 有效采集 | 平均 ≥59 FPS；帧间隔 P95 ≤20 ms，P99 ≤33.4 ms | 仅来源确实持续更新且未暂停的窗口；记录 PTS、动画 frame id、重复/缺失和最长间隔 |
| 正常负载下应用归档丢帧 | 0 个静默丢失；基准运行不触发应用 overrun | 上游 jitter 和已知损失单列，达不到就失败；不能以 59 FPS 平均值豁免已知应用丢失 |
| 原生 callback 工作 | P99 ≤0.15 Tc（60 FPS 约2.5 ms） | 不做 I/O/编码/等待；像素复制和 lease 调度另列，异步 GPU 时间不能藏入回调外不报告 |
| 预览帧龄 | 1080p60 P95 ≤50 ms，P99 ≤100 ms | 来源持续变化且预览可见；暂停/静态源不要求新画面；CPU/兼容路径分别报告 |
| 控制请求→反馈/ACK | UI 下一帧显示意图；健康来源后端 ACK P95 ≤100 ms | 暂停/恢复/停止各测；同步 X11 round-trip 不视作已经硬实时有界 |
| 暂停/停止边界正确性 | ACK 后没有越界新帧混入；恢复没有旧缓冲帧 | 时间/代次断言优先于平均延迟；有原生 buffer 时需要 barrier/epoch |
| 停止→完成持久化 | 健康测试 SSD 初始目标 ≤2 s | 另报排空帧/字节、fsync 时间；慢盘必须有响应/进度，不能虚报“已保存” |
| 录制时间轴 | 暂停区间扣除正确；无负/倒退时长 | native→active 映射自洽；源时钟准确度与回调延迟分开，不承诺系统外部未测精度 |
| 时间戳偏差与漂移 | 记录offset、映射残差P50/P95/P99/max与漂移率；纯数值映射的累计舍入误差≤1项目时间tick | 用有理数/暂停并集oracle测试算法；相对fixture或外部参考的误差需时钟校准，无参考填unknown，不能从回调延迟推算源精度 |
| GIF 文件时间量化 | 可表达时间线累计误差 <10 ms | 原始时序用现有累计舍入验证；兼容重采样还须报告取帧误差和末尾时长，实播另验 |
| 长录制资源 | 帧队列不超 byte cap；无未解释线性资源增长 | 十分钟观察 RSS/VRAM/FD/线程/缓存；索引 O(n) 和项目文件增长单列，不假装项目不增长 |

60 Hz 与 120/144 Hz 的预算按 Td 缩放；如果某平台无法获取准确呈现时间，报告不可测及应用指标，最终显示验收仍需硬件。一个周期内多次立即 repaint 不等于多次真实呈现。

## 3. 数据量与资源预算

【计算】RGBA8 tight frame 为 `width × height × 4` 字节，未计 stride padding、额外副本、量化/纹理、metadata、I/O readback 和文件系统开销。

| 场景 | 单帧 | 60 FPS 数据率 | 600 秒全变化原始数据 | 12 帧像素槽 |
| --- | --- | --- | --- | --- |
| 1920×1080 | 8,294,400 B / 7.9102 MiB | 497,664,000 B/s / 474.6094 MiB/s | 298,598,400,000 B / 278.0914 GiB | 94.9219 MiB |
| 3840×2160 | 33,177,600 B / 31.6406 MiB | 1,990,656,000 B/s / 1898.4375 MiB/s | 1,194,393,600,000 B / 1112.3657 GiB | 379.6875 MiB |

512 MiB 的空缓冲只能吸收完全停写时约 1.079 秒的 1080p60，或 0.270 秒的 4K60。静态去重可以省空间，全变化内容不应假定有高压缩比。

【建议】初始 CPU 共用池 96 MiB/最多12槽；1080p tight frame 可12槽，4K可3槽。字节占用根据分配容量而非逻辑 crop 计算，所有持有者共享 permit。GPU 原生池3、读回槽最多3、最新待呈现预览最多2，均单列额外预算；不允许 CPU frame、writer 和 preview 各自拿到一套96 MiB 后仍宣称总预算96 MiB。

为原生/GPU surface、CPU 池、主预览缓存、缩略图、writer chunk、encoder、metadata 分别发布 allocated/resident/high-water 指标。初始正常1080p录制的额外常驻像素预算目标可先设256 MiB，总 RSS 必须实测，不把该数字当作整进程限制；编辑器缓存和并行导出需要单独预算。VRAM 不能只用 CPU 字节估计。

长录制在 warm-up 60秒后，扣除可解释的帧索引/项目元数据增长，再检查未解释 RSS 增长，起始警戒线为1 MiB/分钟。用空载对照、allocator 指标和多轮运行区分缓存平台期、OS 文件缓存和泄漏；超过警戒线需调查，不能只删异常样本。项目磁盘量按内容持续增长是正常行为，但增长率、剩余空间和容量上限必须可见。

### 3.1 存储候选实验

比较当前逐资产同步、受控 group commit、raw chunk 和候选无损压缩 chunk。每种都使用同一帧序列与磁盘，记录：输入/写入/校验读回字节、持续吞吐、CPU、内存高水位、压缩比、P99 append/flush、随机取帧延迟、checkpoint 停顿、stop 时间及 durable age。

至少使用静态UI、滚动文本、普通窗口动画、高熵/噪声变化。若压缩不能满足动态场景，不以静态压缩率宣传十分钟60FPS容量。记录插电/省电、SSD cache 耗尽后的持续速度；十分钟运行不能用几秒 burst 代替。

故障注入点包括chunk header/body/checksum/index、journal前后、flush/sync、checkpoint rename前后和Durable ACK前后。恢复至少包含所有已ACK的durable revision，也允许额外完整提交但ACK尚未送达的revision；截断、重复、乱序和损坏要可解释。进程强杀通过不等于真实断电/存储缓存持久化已证明；断电保证需另行限定设备与文件系统。

## 4. 可复现的测试流程

下面是拟建立的协议，不是现成性能工具的使用声明。`scripts/qa/capture_perf.py` 和统一 trace schema 尚未实现，属于主方案 A3；不得直接复制不存在的命令宣称完成测试。

1. 固定 commit/lockfile/toolchain，生成 release binary，记录 SHA-256、构建 feature 和本地修改。原生桌面登录启动，系统同意所选来源；不要用别人的会话或扩大授权来源。
2. 记录 §1 全部环境。预热60秒，确认磁盘容量、关闭额外导出，保存初始RSS/VRAM/FD/线程计数。必要时另做有后台负载对照。
3. 以显示器节奏绘制可见编号/时间标记的 fixture，分别运行静态、快速滚动、普通窗口拖动、连续动画；记录 fixture 实际呈现节奏，未达60时不能把它当作60FPS输入。
4. 每个短场景至少60秒、独立运行3次，报告每次和合并分布、全部异常，不只选最好一次。静态场景核对idle/重复和时长；动态场景对照编号检查采样与缺失。
5. 录制框拖动/缩放覆盖四角、对角线、反向、快速释放、跨屏；录制前允许缩放，录制中按固定画布+内部暂停规则测试移动。移动时不采集的区间必须显示并从活动时间排除，不能计成正常采集60FPS。
6. 每个拟支持后端/架构至少一次连续600秒动态录制，周期性暂停/继续另做一轮；记录1秒窗口指标与原始帧间隔/队列/flush事件。观察第1/5/10分钟而不是只看最后RSS。
7. 运行慢盘、空间不足、强杀、来源关闭、权限撤销、断屏、sleep/wake；检查停止/恢复及错误文字。分别验证已接收、未持久化和已持久化边界。
8. 打开项目，确认帧选择/删除/速度/undo、时间轴和预览一致，重新打开验证持久化。导出快速/高质量/原始时序，对比解码画面与总时长。
9. 在前台 Chrome/Firefox/Safari（适用平台）和系统查看器分别播放编号GIF，记录产品版本、平台、实际循环时长和短帧行为；后台标签节流另列。来源60FPS、GIF文件时间和实播时序分开报告。
10. 保存完整结果、原始 trace、错误/缺口日志、资源曲线、必要相机视频与测量误差。给每项门槛通过/失败/未测，不将未测归入通过。

另设录制期间并发导出/编辑负载实验，限制线程/内存争用。若MVP禁止并行操作，UI要明确禁用并说明原因；不能默认在完全空载结果上承诺并发性能。

### 4.1 结果格式草案

这是未来工具的结构示例，`null` 表示未测；不得用0或推算值伪装真实显示结果。

```json
{
  "schema": 1,
  "run_id": "platform-scenario-unique-id",
  "evidence": "proposed-format",
  "build": {"commit": "...", "dirty_hash": null, "binary_sha256": "...", "profile": "release", "rust": "1.88.0"},
  "environment": {
    "os": "...", "cpu": "...", "gpu_driver": "...", "desktop": "...",
    "backend": "...", "disk_filesystem": "...", "power_mode": "...",
    "displays": [{"pixels": [1920, 1080], "hz": 60, "scale": 1.0, "origin": [0, 0]}]
  },
  "scenario": {"name": "continuous-animation", "seconds": 600, "requested_capture_fps": 60, "preview_enabled": true},
  "clock": {"domain": "...", "quality": "native-or-estimated", "basis": "native-continuous-or-session-active", "mapping": "...", "mapping_error_ns": null},
  "counts": {
    "source_observed": null, "sampled": null, "duplicate_merged": null,
    "archive_accepted": null, "durable": null, "preview_replaced": null,
    "application_overrun": null, "stale_rejected": null, "native_loss": "unknown"
  },
  "milliseconds": {
    "ui_frame": {"p50": null, "p95": null, "p99": null},
    "input_to_submit": {"p50": null, "p95": null, "p99": null},
    "input_to_photon": null, "capture_interval": null, "preview_age": null,
    "pause_ack": null, "resume_ack": null, "stop_ack": null, "durable_complete": null
  },
  "resources": {"cpu": null, "gpu": null, "rss_bytes": null, "vram_bytes": null, "queue_high_water_bytes": null, "disk_bytes_per_second": null},
  "gates": [{"name": "physical-drag", "status": "not-measured", "reason": "requires display measurement"}],
  "attachments": []
}
```

实现工具时将聚合值、逐秒采样和每帧/控制事件分开存放，定义单位与 histogram 算法。所有阶段耗时记录 count/P50/P95/P99/max：capture/native copy、normalize、retention/dedup、preview render/upload、writer wait/append/sync/checkpoint、quantize/encode/publish。记录测量探针本身 dropped events。

## 5. 平台与交互验收矩阵

下表全部是未来门槛。当前原生测试覆盖参见 [Research §3](CROSS-PLATFORM-RESEARCH.md#3-当前验证与-ci-边界)。

| 组别 | 必测组合/行为 | 通过依据 |
| --- | --- | --- |
| 基础原生桌面 | Linux X11；GNOME Wayland；KDE Wayland；Windows11 x64；macOS14+ arm64及x86_64各自 | 每组来源/控制/编辑/导出/保存恢复完整闭环；记录系统版本；无真实设备则未测 |
| 来源与边界 | monitor、window、source-local region；遮挡/最小化；源关闭/替换；权限拒绝/撤销/系统停止 | 不扩大授权；正确停止/重选；X11只保证可见root像素，限制明确；恢复已提交内容 |
| 录制框排除 | 边框/工具栏/菜单/提示；窗口捕获与整屏捕获分别试 | 实际捕获像素中不出现自有UI；API返回成功不足以通过；fallback隐藏/区域外流程可完成 |
| DPI与布局 | 100/125/150/200%；EN/ZH/长路径/长错误；窄窗/最小尺寸 | 按可用缩放组合逐项测试；文字不遮挡、控件可操作、焦点可见；macOS原生缩放加UI zoom覆盖指定比例 |
| 桌面几何 | 负原点、混合DPI、竖屏、跨屏、热插拔、刷新率切换 | source-local crop 与 fixture 的像素边界一致；旧变换失效不会录错屏；固定canvas规则正确 |
| 交互回归 | 原生拖动、反向/释放/捕获丢失/取消；快捷键冲突；状态和预览变化 | 最终位置ACK可靠，无历史移动积压；取消不误继续；冲突可重绑；全部关键命令有反馈 |
| 首页尺寸 | 自定义首页大小→录制→编辑→回首页，取消/错误路径也覆盖 | 恢复首页尺寸且不继承编辑器min-size；DPI变更后按明确逻辑尺寸策略处理 |
| 性能 | 1080p60、四场景、每组至少600秒；60Hz基础，120/144Hz专项；另测10/20/30FPS采集与10/20/30FPS导出设置下的UI刷新 | §2预算与§4协议；低采集/导出FPS不降低活跃UI/guide目标；4K/高刷新采集只按实际结果提升支持等级 |
| 资源/恢复 | 慢盘/ENOSPC/强杀/损坏、GPU丢失、断屏、sleep/wake；隐藏/最小化/停止预览后再次录制 | 有界队列、可解释缺口、durable边界可恢复；completion泵继续、native lease归还、资源回到合理idle预算；无UI阻塞或虚假保存成功 |
| GIF实播 | 10/20/30/50FPS兼容配置、原始60时序；多查看器前台/后台 | 记录文件延迟和实播分开；无总时长累积漂移；palette/delta/disposal解码等价 |
| 包与升级 | Linux N→N+1/失败回退/修改文件；Windows portable/MSIX；mac两架构下载隔离启动 | 应用可启动；签名/公证按发行级别验证；卸载/回退保留项目；schema回退限制明确 |

Windows ARM64/Linux ARM64 先列 L2 编译等级，有硬件后执行同一 L4/L5矩阵。不能把mac arm64的成功计作Intel通过，也不能把GNOME结果计作KDE通过。HDR、4K、多GPU/跨adapter是独立专项，MVP SDR限制不隐去这些待验项。

## 6. 现有回归命令与新增门槛

以下命令的测试名/target在基线已核对存在；除§7列出的命令，本轮未重新执行。它们验证逻辑，不测真实显示FPS。

```sh
cargo test --locked -p gif-from-screen-workflow --all-targets
cargo test --locked -p gif-from-screen-application --lib recording_project::tests
cargo test --locked -p gif-from-screen-application --lib project_export::tests
cargo test --locked -p gif-from-screen-project --lib
cargo test --locked -p gif-from-screen-gif --test roundtrip
cargo test --locked -p gif-from-screen-gif --lib timing::tests
cargo test --locked -p gif-from-screen-capture-linux --lib --no-default-features --features native-wayland,native-x11
cargo test --locked -p gif-from-screen recorder_geometry::tests
cargo test --locked -p gif-from-screen retarget::tests
cargo test --locked -p gif-from-screen main_window_size_tests
cargo test --locked -p gif-from-screen page_text_layout_audit
```

需要新增的确定性测试：byte permit多消费者释放、入口/归档计数守恒、过载停止与flush失败、pause epoch/barrier、重叠暂停并集、旧frame/ACK拒绝、clock reset、普通/paused/idle stop尾帧封口、截断chunk/checksum/index恢复、后台preview revision取消、native lease fence后释放、30/50FPS重采样与尾帧。相关原生硬件集成另列，不能以mock通过替代。

所有 native ignored 测试只在明确拥有的专用桌面/隔离server执行，避免误操作用户桌面。延续现有 CI 的 private-Xvfb 隔离方式；需要真实Portal/GUI的测试由登录会话执行，不能用Xvfb伪造Wayland权限与呈现结论。

## 7. 本轮设计交付验证

**核对日期2026-10-23，代码基线b82dd67，Linux6.8.0-106-generic；本机libpipewire0.3.48，仅安装x86_64-unknown-linux-gnu Rust target。** 这些环境事实不能证明当前登录桌面、显示刷新率或GPU性能。

【验证】实际完成：

- `cargo test --locked -p gif-from-screen-workflow --all-targets`：26个单元测试、42个集成测试通过，0失败。覆盖控制、暂停时长、sink-only、尾帧封口等逻辑。
- `cargo test --locked -p gif-from-screen-gif timing::tests -- --nocapture`：2个时间量化单元测试通过，其他测试被过滤；验证累计舍入，不验证浏览器实播。
- `cargo test --locked --workspace --all-targets --all-features -- --list`：测试发现成功；它只是清单，**不算全量测试通过**。
- 查询并核对基线对应的Linux CI和portable成功记录，见 [Research §3](CROSS-PLATFORM-RESEARCH.md#3-当前验证与-ci-边界)；不作为本轮执行全量Rust测试的声明。
- 全部44个原有修改/未跟踪文件按SHA-256核对未被本轮更改。没有修改应用代码、依赖、旧文档或项目格式。

本轮文档交付检查：解析153个Markdown链接，相对路径与章节锚点有效；70个去重外部资料链接请求成功；Mermaid11.12.0解析三张图通过；JSON示例、数据量计算通过。附件11节及10项输出逐项审核，并完成Windows、macOS和管线三个方向的复核；发现的尾帧现状、错误排空、durable顺序、QPC单位及GPU资源释放问题已修正。

未执行的Windows/macOS构建、native采集、真实显示测量、600秒性能、签名、公证和播放端测试均保持【待验】，属于后续实施门槛，不是本轮设计任务的已达成果。

设计完成的依据是：要求已映射到具体契约、路径、阶段、实验与验收；不是把未来平台实现或性能目标提前判为完成。
