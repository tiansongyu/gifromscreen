# ScreenToGif 功能对照表

状态：设计基线 0.1  
参考版本：ScreenToGif 2.43.2，commit `a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`（2026-07-28）

## 1. 对齐口径

本项目中的“功能一致”定义为：

1. 保留 ScreenToGif 的屏幕、摄像头、画板三种内容来源。
2. 保留其逐帧时间线、结构编辑、图像处理、叠加、转场、自动任务、项目恢复和 GIF 参数控制。
3. 最终产物只支持 GIF。ScreenToGif 的 APNG、WebP、BPG、AVIF、AVI、MKV、MOV、MP4、PSD、图片序列等导出不在范围内。
4. 允许导入静态图片、GIF 和视频；视频导入属于兼容输入，不代表支持视频导出。
5. 不要求复刻 Windows/WPF 外观、Windows 任务栏集成或 Microsoft Store 行为。
6. 不复制上游品牌、图标、文本、翻译或源代码；只做可验证的行为级兼容。

里程碑标记：

- `S0`：架构风险验证。
- `M1`：首个可用版本，打通“录制 → 基础编辑 → GIF”。
- `M2`：核心编辑器对齐。
- `M3`：三种录制器和高级功能对齐。
- `M4`：发行、可访问性、自动化和生态完善。
- `OUT`：因“只生成 GIF”的产品定位明确排除。

平台标记：

- `完整`：平台 API 能实现。
- `受限`：功能存在，但交互方式或元数据受操作系统安全模型限制。
- `不适用`：不属于该平台或已排除。

这些标记描述平台能力和设计目标，不代表已完成全部原生验收。当前开发优先 Linux，macOS 尚未进入本轮实现；本轮计时／录制时钟的实现及自动化证据见 [计时契约](CAPTURE-PLAYBACK-TIMING.md)，原生验收范围与未完成硬件门禁见 [下一轮工作](NEXT-LINUX-ITERATION.md)。

## 2. 启动器与应用级能力

| 功能 | 目标 | macOS | Linux Wayland | Linux X11 | 说明 |
|---|---:|---|---|---|---|
| 屏幕录制入口 | M1 | 完整 | 完整 | 完整 | 启动器、快捷键、菜单栏/托盘入口 |
| 摄像头录制入口 | M3 | 完整 | 完整 | 完整 | 单独生成摄像头 GIF，不含音频 |
| 画板录制入口 | M3 | 完整 | 完整 | 完整 | 笔迹按时间生成帧 |
| 编辑器/打开项目入口 | M1 | 完整 | 完整 | 完整 | 打开项目、GIF、图片或视频 |
| 单实例 | M4 | 完整 | 完整 | 完整 | 第二实例将文件转交给首实例 |
| 启动最小化、关闭行为 | M4 | 完整 | 受限 | 完整 | Wayland 托盘取决于桌面环境 |
| 菜单栏/系统托盘 | M4 | 完整 | 受限 | 完整 | 不能作为停止录制的唯一入口 |
| 主题与系统深浅色 | M2 | 完整 | 完整 | 完整 | 自有视觉，不复刻上游资源 |
| 国际化框架 | M2 | 完整 | 完整 | 完整 | 首发中文、英文；其余语言后续社区化 |
| 自动更新 | M4 | 完整 | 受限 | 受限 | Flatpak 由商店更新，其余使用签名更新清单 |
| 日志、诊断、崩溃报告导出 | M4 | 完整 | 完整 | 完整 | 默认不上传；用户主动导出 |

## 3. 屏幕录制器

| 功能 | 目标 | macOS | Linux Wayland | Linux X11 | 说明 |
|---|---:|---|---|---|---|
| 显示器录制 | M1 | 完整 | 完整 | 完整 | 多显示器、旋转、负坐标 |
| 窗口录制 | M2 | 完整 | 受限 | 完整 | Wayland 必须由系统 Portal 选择器确认 |
| 任意矩形区域录制 | M1 | 完整 | 受限 | 完整 | Wayland 先选显示器/窗口，再在冻结预览中裁剪 |
| 重新选择区域/窗口/显示器 | M2 | 完整 | 受限 | 完整 | Wayland 可能再次显示系统确认框 |
| 选区拖动、尺寸输入、窗口吸附 | M2 | 完整 | 受限 | 完整 | X11 已支持录制前移动/缩放及录制中固定画布移动；Wayland 无通用窗口几何接口，吸附只在可得信息范围内工作 |
| 可调 FPS | M1 | 完整 | 完整 | 完整 | 内部按单调时钟采样，不依赖显示刷新率 |
| 固定帧率（播放时长） | M1 | 完整 | 完整 | 完整 | Linux 已分离采样与播放策略：固定模式每个保留帧使用同一延迟，丢帧／略过未变帧不累计固定时长、不补造画面；按实际时间延长属于 measured 模式。自动化覆盖保存／重开／GIF 时长，另有 [原生手动固定链验收](NATIVE-PLAYBACK-QA-2026-09-07.md)，不代表所有频率或硬件已验收 |
| 仅捕获变化 | M2 | 完整 | 完整 | 完整 | damage 元数据优先，像素差分回退 |
| 手动快照模式 | M2 | 完整 | 完整 | 完整 | 每次确认成功的触发保留一帧，包括相同画面；Linux 固定播放默认每帧 1,000 ms，可显式选择 measured 时间；原始点击间隔不改写 |
| 交互触发模式 | M3 | 完整 | 受限 | 完整 | Wayland 无通用被动全局输入事件 |
| 每秒/分钟/小时频率 | M2 | 完整 | 完整 | 完整 | 采样间隔与 GIF 播放独立；Linux 周期采样默认固定播放 66 ms/保留帧，可选 measured；每秒固定播放采用整数 1,000/FPS ms，详见 [上游对照](CAPTURE-PLAYBACK-TIMING.md) |
| 倒计时 | M1 | 完整 | 完整 | 完整 | 可取消 |
| 开始、暂停、继续、停止、丢弃 | M1 | 完整 | 完整 | 完整 | 显式状态机 |
| 录制浮动控制条 | M1 | 完整 | 完整 | 完整 | 不依赖托盘 |
| 全局快捷键 | M2 | 完整 | 受限 | 完整 | Wayland 使用 GlobalShortcuts Portal，后端不支持时提示 |
| 录制鼠标指针 | M1 | 完整 | 完整 | 完整 | embedded 或 metadata cursor |
| 指针位置/形状可后期编辑 | M3 | 完整 | 受限 | 完整 | 取决于 Wayland compositor 是否提供 metadata cursor |
| 自动记录鼠标按键 | M3 | 完整 | 受限 | 完整 | Wayland 标准接口不提供被动点击监听 |
| 自动记录键盘按键 | M3 | 完整 | 受限 | 完整 | Wayland 默认改为手工添加按键标注 |
| 指针跟随录制区域 | M3 | 完整 | 受限 | 完整 | Wayland 需有指针元数据 |
| 三分线、十字线 | M2 | 完整 | 完整 | 完整 | 只显示在选择覆盖层，不进入 GIF |
| 选区放大镜 | M2 | 完整 | 完整 | 完整 | 多 DPI 坐标必须正确 |
| 录制边框动画 | M2 | 完整 | 受限 | 完整 | Wayland layer/overlay 能力差异 |
| 内存缓存/磁盘缓存选择 | M2 | 完整 | 完整 | 完整 | 实际实现为统一的有界 FrameStore 策略 |
| 多显示器混合 DPI | M1 | 完整 | 完整 | 完整 | 强类型 logical/physical 坐标 |
| 捕获源消失、休眠、热插拔恢复 | M4 | 完整 | 完整 | 完整 | 不损坏已经录到的帧 |
| CLI 指定选区、频率、时限、自动开始 | M4 | 完整 | 受限 | 完整 | Wayland 仍不能绕过 Portal 授权 |

## 4. 摄像头与画板录制

| 功能 | 目标 | macOS | Linux | 说明 |
|---|---:|---|---|---|
| 摄像头设备枚举和刷新 | M3 | 完整 | 完整 | AVFoundation / V4L2 或 Camera Portal |
| 摄像头预览、分辨率、缩放 | M3 | 完整 | 完整 | 能力不支持时禁用对应选项 |
| 摄像头 FPS | M3 | 受限 | 完整 | 部分 AVFoundation Rust 封装不能准确设置 FPS，需原生 adapter 验证 |
| 摄像头开始/暂停/停止/丢弃 | M3 | 完整 | 完整 | 与屏幕录制共用状态机 |
| 画板尺寸与背景色 | M3 | 完整 | 完整 | 可创建透明或纯色画布 |
| 画笔、荧光笔、橡皮擦 | M3 | 完整 | 完整 | 记录矢量笔迹与时间戳 |
| 笔尖形状、宽高、平滑 | M3 | 完整 | 完整 | 导出时确定性栅格化 |
| 画板自动录制/按操作录制 | M3 | 完整 | 完整 | 复用 capture cadence 聚合器 |

## 5. 项目、导入和恢复

| 功能 | 目标 | 状态/说明 |
|---|---:|---|
| 新建空白动画 | M2 | 指定宽、高、背景和首帧时长 |
| 从屏幕/摄像头/画板创建项目 | M1/M3 | 屏幕 M1，其余 M3 |
| 向当前项目追加三种录制 | M3 | 保持当前画布尺寸，提供填充/缩放策略 |
| 导入静态/动画图片 | M2/M3 | M2：PNG、JPEG、BMP、GIF；M3：APNG、AVIF、WebP。GIF 必须正确处理 disposal、透明度、局部帧和循环信息 |
| 导入视频 | M3 | 可选 FFmpeg sidecar；只用于输入 |
| 拖放导入 | M2 | 文件、文件夹和多文件排序 |
| 从剪贴板粘贴图片/帧 | M2 | 支持粘贴位置策略 |
| 最近项目 | M2 | 最近列表、缺失文件提示 |
| 项目保存/另存 | M1 | 版本化 `.gfsproj`；保存过程原子化 |
| 自动保存和崩溃恢复 | M1 | snapshot + append-only journal |
| 临时缓存清理策略 | M2 | 启动扫描、按天清理、手动清理 |
| schema 迁移 | M1 | 项目格式版本独立于应用版本 |
| 导入/打开 ScreenToGif `.stg` | M4 | 可选兼容功能；先做格式与许可验证 |

## 6. 编辑器通用操作

| 功能 | 目标 | 说明 |
|---|---:|---|
| 逐帧胶片/时间线 | M1 | 虚拟化，不能因 5 万帧而创建 5 万个完整控件 |
| 多选、范围选择、全选、反选、取消 | M1 | 键盘和鼠标均可操作 |
| 跳转到帧 | M1 | 帧号和时间均可输入 |
| 第一/上一/播放/下一/最后 | M1 | 可循环播放 |
| 预览时落后则跳帧 | M2 | 仅影响预览，不改项目 |
| 100% 缩放、适应窗口、适应内容 | M1 | 保持像素与 DPI 语义清晰 |
| 剪切、复制、粘贴 | M1 | 帧引用为 copy-on-write |
| 应用内多条剪贴板历史 | M2 | 可预览、选择、删除历史条目；默认有容量上限 |
| Undo、Redo、Reset | M1 | 所有可见编辑都必须可撤销；支持历史上限 |
| 打开单帧/定位缓存 | M4 | 诊断功能 |
| 帧数、尺寸、DPI、当前时间、总时长统计 | M2 | 状态栏实时显示 |

## 7. 帧结构与时序编辑

| 功能 | 目标 | 说明 |
|---|---:|---|
| 删除选中帧 | M1 | 不能删除到非法空状态而无提示 |
| 删除之前/之后帧 | M2 | 原子命令 |
| 删除重复帧 | M2 | 相似度、保留第一/最后、时长合并策略 |
| 减少帧数 | M2 | factor/count；时长不调、合到前帧、均匀分配 |
| 平滑循环 | M3 | 相似度、起点阈值和来源方向 |
| 反转 | M1 | 保持各帧自身 duration |
| Yoyo/往返 | M2 | 端点是否重复由参数决定 |
| 左移/右移选中帧 | M1 | 稳定多选顺序 |
| 覆盖帧延时 | M1 | 毫秒级领域模型 |
| 增减帧延时 | M1 | 结果必须大于零 |
| 比例缩放帧延时 | M2 | 总时长可预览 |

## 8. 图像、叠加与转场

| 功能 | 目标 | 说明 |
|---|---:|---|
| 调整尺寸 | M1 | 像素/百分比、保持比例、多种插值 |
| 裁剪 | M1 | 交互选框和精确数值输入 |
| 水平/垂直翻转、90° 旋转 | M2 | 可作用于选择范围 |
| Caption | M2 | 字体、字号、颜色、描边、对齐、边距 |
| 自由文本 | M2 | 背景、对齐、装饰、阴影、位置 |
| 标题帧 | M2 | 背景、时长、字体和对齐 |
| 按键标注 | M3 | 自动事件或手工事件；过滤、翻译、合并、持续时间 |
| 自由绘制 | M2 | 画笔/高亮/橡皮/选择 |
| 形状 | M2 | 线、箭头、矩形、椭圆；轮廓、填充、虚线、圆角 |
| 鼠标事件高亮 | M3 | 指针光环及各按键颜色 |
| 水印 | M2 | 图片、透明度、尺寸、位置 |
| 边框 | M2 | 四边独立宽度与颜色 |
| 阴影 | M2 | 方向、深度、模糊、透明度和背景 |
| 马赛克 | M2 | 像素尺寸 |
| 模糊 | M2 | 强度和平滑参数 |
| 加深/减淡区域 | M2 | 范围遮罩 |
| Cinemagraph | M3 | 用遮罩冻结/放行动画区域 |
| 进度条/帧号/日期时间文本 | M3 | 水平/垂直、格式、精度、位置 |
| Fade 转场 | M2 | 到下一帧或指定颜色，长度与延时 |
| Slide 转场 | M2 | 方向、长度与延时 |

## 9. 自动任务

ScreenToGif 2.43.2 的 `TaskTypes` 枚举声明了更多类型，但编辑器当前实际自动执行路径只覆盖 MouseEvents、KeyStrokes、Delay、Progress、Border 和 Shadow。表中其余项目不作为严格 parity 的完成条件。

| 功能 | 目标 | 说明 |
|---|---:|---|
| 录制完成后自动执行任务链 | M3 | 有序、可启停、失败可定位 |
| 鼠标事件 | M3 | 受平台采集能力限制 |
| 按键标注 | M3 | 受平台采集能力限制 |
| 时长修改 | M3 | 复用编辑命令 |
| 进度覆盖 | M3 | 复用 overlay track |
| 边框、阴影 | M3 | 复用 effect graph |
| 删除重复帧 | M4-enhancement | 上游枚举中存在，但 2.43.2 的自动任务运行路径没有执行它，不计入严格 parity |
| 水印、标题帧、调整尺寸 | M4-enhancement | 同上；可作为本项目增强能力，而非上游实际行为对齐项 |

## 10. GIF 导出

| 功能 | 目标 | 说明 |
|---|---:|---|
| 保存路径、命名、覆盖策略 | M1 | 先写 `.partial`，fsync 后原子替换 |
| 导出全部或选定范围 | M1 | 保持项目不变 |
| 帧表达式、帧范围、时间范围、当前选择 | M2 | 包含逆序范围，解析失败要给出精确位置 |
| 循环次数/无限循环 | M1 | GIF Netscape loop extension |
| 导出时 Ping-pong | M3 | 与结构化 Yoyo 分开；不修改原项目 |
| 逐帧可变时长 | M1 | 项目用微秒；导出时量化到 10ms 并分配累计误差 |
| 最大颜色数 | M1 | 2–256 |
| 全局/局部调色板 | M2 | 按内容和预设选择 |
| 量化：神经/Octree/Median Cut/Wu/灰度/高频色/预定义或自定义调色板 | M2/M3 | 可用等价算法，不要求字节级复刻；Wu 与预定义调色板在 M3 |
| 抖动：无/Bayer、Dotted、Blue Noise、Atkinson、Burkes、Floyd–Steinberg、Jarvis–Judice–Ninke、Sierra 系列、Stevenson–Arce、Stucki、随机/交错噪声 | M2/M3 | M2 先覆盖无/Bayer/Floyd/Sierra，其余 M3 |
| 透明阈值/透明色/色键 | M2 | 正确保留 disposal 语义 |
| 完全重复帧合并 | M1 | 合并 duration |
| 差分矩形和裁边 | M2 | 导出后回解码验证合成结果 |
| 相似帧有损合并 | M3 | 明确标记 lossy，并可预览 |
| 质量/速度预设 | M1 | 快速、均衡、高质量 |
| 导出大小估算 | M3 | 估算值必须标注误差范围 |
| 后台任务、进度、取消、重试 | M1 | 不阻塞 UI；失败不破坏现有文件 |
| 导出后复制、打开、定位 | M2 | 桌面平台服务 |
| 多个编码任务队列 | M4 | 录制和交互优先于编码 |
| 可插拔高质量 Gifski 后端 | M3 | AGPL-3+ 或商业授权，默认发行策略另行决定 |
| 可选 FFmpeg GIF 后端 | M4 | 主要用于兼容测试，不作为默认真值 |
| Imgur/自定义 HTTP 上传与历史 | M4 | 可选、显式联网、默认关闭 |
| 导出后复制文件/路径/链接、打开目录 | M2 | 不执行 shell |
| 导出后自定义命令 | M4-optional | 高风险能力；默认关闭，参数数组调用且不经 shell 拼接 |

## 11. 明确排除

| 功能 | 状态 | 原因 |
|---|---|---|
| APNG/WebP/BPG/AVIF 导出 | OUT | 产品只生成 GIF |
| AVI/MKV/MOV/MP4/WebM 导出 | OUT | 产品只生成 GIF，且不处理音频 |
| PSD、BMP、JPEG、PNG 序列导出 | OUT | 非 GIF 产物 |
| Windows Desktop Duplication、WPF、任务栏 ThumbButton | OUT | 平台特有 |
| ScreenToGif 名称、Logo、矢量资源和 UI 像素级复刻 | OUT | 品牌与许可边界 |

## 12. 不能虚假承诺的 Linux 限制

Wayland ScreenCast Portal 标准源类型只有显示器、窗口和虚拟显示器，选择动作通常由系统对话框完成。它可以提供嵌入式或 metadata cursor，但没有用于普通录屏的被动全局按键/点击事件接口。InputCapture Portal 会接管输入，并且由 compositor 决定何时激活，不等价于键盘记录器。

因此正式承诺应写成：

> macOS 和 Linux X11 在获得用户授权后提供完整录制元数据；Linux Wayland 提供完整画面录制与编辑能力，并按 compositor 能力提供光标元数据。不可用的全局按键/点击元数据将明确显示原因，并允许用户在编辑器中手工添加。

不采用默认读取 `/dev/input`、要求用户加入 `input` 组或常驻 root helper 的方案。它会扩大键盘记录与提权风险，也会破坏 Flatpak 发行模型。

## 13. 主要上游证据

- [ScreenToGif README](https://github.com/NickeManarin/ScreenToGif/tree/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd)
- [Editor 命令与界面](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml)
- [编辑器命令定义](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Util/Commands.cs)
- [Recorder 界面](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/NewRecorder.xaml)
- [用户设置项](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif.Util/Settings/UserSettings.cs)
- [项目与帧模型](https://github.com/NickeManarin/ScreenToGif/tree/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Model)
- [GIF 导出预设](https://github.com/NickeManarin/ScreenToGif/tree/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif.ViewModel/ExportPresets/AnimatedImage/Gif)
- [XDG ScreenCast Portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [XDG InputCapture Portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)
