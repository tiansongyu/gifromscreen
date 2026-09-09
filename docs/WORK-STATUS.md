# 工作状态与 v0.1.0 发布范围

更新：2026-09-09。当前已公开的正式版为 **v0.1.0 · Linux x86_64**，功能实现基线为 `67f11e7`，实际发行源为 `b3cd1ff`。下载与实际包验收见[发行记录](RELEASE-0.1.0-QA.md)。
本轮已停止扩展功能；未完成的 schema 9 / Vector v2 改动已推送至独立归档分支，不在正式发布范围内。
发布后仅按用户反馈进行[图形启动兼容性修复](GRAPHICS-BACKEND-STARTUP.md)：`main` 的 0.1.1 构建为 X11 增加自动 Vulkan 回退，旧 v0.1.0 资产保持不变。这不代表恢复下方暂缓的功能扩展。
发布版仍使用 **工程 schema 8、VectorShape v1 作者与既有渲染契约**。
实际下载文件、构建身份和 SHA-256 以该版本的 [Release 资产与说明](https://github.com/tiansongyu/gifromscreen/releases)为准；本清单不代替包级验收回执。

“可用”表示已有实现和对应测试或有限原生验收，**不表示完整一比一复刻、所有平台通过或所有问题已解决**。
旧计划和 QA 文档保留各自版本、失败样本与证据；其中 Preview 下载、旧消息数和早期 schema 描述是历史记录，不覆盖本页的发布范围。

## 当前能做什么

| 类别 | 已交付能力 | 边界与证据 |
| --- | --- | --- |
| 屏幕录制与定位 | X11 显示器 / 窗口 / 区域录制，独立边框与控制器；数值 X/Y/W/H、微调、窗口吸附、点击或拖出选窗。开始前调整大小，录制 / 暂停时移动固定尺寸区域。Wayland 通过 Portal 授权来源，再调整来源内裁剪。 | X11 已有普通区域、1×1、全屏、暂停恢复及一次移动轨迹验收；不是任意 WM / 多屏 / DPI 的保证。[X11 记录](NATIVE-X11-SPLIT-QA-2026-09-07.md)、[拖选窗口](X11-DRAG-PICKER-QA-2026-09-08.md)。 |
| 录制时间与控制 | 可取消倒计时、开始、暂停、继续、定时或手动停止保存、明确丢弃；连续、周期、手动快照，以及 X11 交互触发采样。采样节奏和固定 / measured 播放时长分开，暂停不计入有效录制时间，原始采样时钟不因编辑延时而改写。 | 实际保留帧数取决于采样、变化过滤与处理能力；设定 FPS 不等于硬件达标承诺，定时停止也不等于固定播放 GIF 的总时长上限。GIF 延时量化到 10 ms。[计时契约](CAPTURE-PLAYBACK-TIMING.md)、[原生计时](NATIVE-PLAYBACK-QA-2026-09-07.md)。 |
| 内容来源与导入 | GIF、PNG/JPEG/BMP/WebP、图片序列和本地视频导入；新建空白动画；画板笔 / 荧光笔 / 橡皮与录制。摄像头枚举、预览和录制入口已实现。 | 视频导入需要系统 `ffmpeg` / `ffprobe`；物理摄像头及权限 / 模式验收尚未完成。导入支持不代表支持相应格式导出。[来源说明](LINUX-ITERATION-LIVE-SOURCES.md)、[视频范围](LINUX-ITERATION-VIDEO.md)。 |
| 逐帧编辑与预览 | 帧选择、排序、删除、剪切 / 复制 / 粘贴、剪贴板历史、延时调整、降帧、去重、反转 / Yoyo；Fit / 100% / 200%、滚动 / 平移、数值和直接裁剪；有序整帧 crop / resize / rotate / flip / effects。 | 100% 按图像像素与物理显示像素映射；超缓存 / 纹理尺寸明确报错，尚无任意大图的完整分块预览。`67f11e7` 的窄编辑器已在 680×760、150% 缩放下通过原生中英文切换、单页滚动、Add / Undo 和重开 GIF 对照；不代表首页和所有窄布局已完成。[预览裁剪](EDITOR-CANVAS-QA-2026-09-08.md)、[有序编辑](ORDERED-FRAME-EDITING.md)、[窄编辑器原生验收](NARROW-EDITOR-QA-2026-09-09.md)。 |
| 文字、图层与形状 | 字幕、保存文字组重编辑、标题帧、水印、旧线 / 箭头、普通自由绘制；独立多形状画布支持圆角矩形、椭圆、三角形、块状箭头及移动 / 缩放 / 旋转。图层可隐藏、显示、删除，保留帧归属和有序绘制阶段。 | 正式向量作者是 **v1**，其完整 WPF 像素对照仍存在已记录差异；不能把实验 WPF API 的 14 个精确案例归给正式默认形状。Apply 后也不等于所有对象都已有完整重编辑入口。[文字 / 标题原生链](TEXT-TITLE-QA-2026-09-09.md)、[向量可用性与差异](VECTOR-SHAPE-QA-2026-09-09.md)。 |
| 标注、效果与自动任务 | 手工 / 录制按键、点击、光标和进度标注；边框、阴影、模糊、马赛克、明暗、Fade / Slide；矩形冻结、自由笔迹 Cinemagraph 与平滑循环；六类自动任务及项目导出预设。 | 录制输入受后端及用户授权限制；标注参数、自由笔迹 / 擦除 / Boolean 几何和更广 WPF 数值一致性仍未全部完成。局部真实 Windows 对照不是全效果精度认证。[标注](ANNOTATIONS.md)、[自动任务](AUTOMATIC-TASKS.md)、[Cinemagraph 原生范围](NATIVE-CINEMAGRAPH-QA-2026-09-07.md)。 |
| 工程与 GIF | 增量 journal 保存、异常恢复与素材完整性报告、显式过期锁接管、Undo / Redo、另存独立工程副本和最近项目；后台导出全部 / 选定帧，支持调色板、抖动、透明、循环与差分帧。 | 多条真实 UI → 保存 / 重开 → CLI GIF 链已验证；I/O 同步失败可能要求重开确定 durable 状态，不能保证任意故障都无损。GIF 量化结果与原始 RGBA 对照分开判断。[恢复 / 保存](FRAME-OWNED-OVERLAYS-PLAN.md)、[GUI / CLI 导出记录](PREVIEW-EXPORT-QA-2026-09-09.md)。 |
| 语言与界面 | **848 条英文 / 简体中文消息**；覆盖导航、录制、快捷键、主要编辑、预览 / 裁剪 / 导出、高级效果 / 图层、水印和文字 / 标题。支持 System 加 **29 项可保存语言选择**，随包提供 CJK 回退字体。 | 另外 **27 个目标语言尚无译文，明确回退英语并保留所选项**。848 是已接入消息集，不是全 UI 完成率；部分导入 / 工具、共享工具包提示和原始后端诊断仍未完整本地化，也未宣称 RTL / IME 全通过。[本地化范围](LOCALIZATION-PLAN.md)、[848 条消息验收](TEXT-TITLE-LOCALIZATION.md)、[中英文录制](RECORDER-LOCALIZATION.md)。 |
| Linux 分发 | 原生 x86_64 `tar.gz`，包含桌面程序、CLI、校验和与许可材料；解压运行，可选用户级桌面入口安装，无需管理员权限。 | Ubuntu 22.04 / **glibc 2.35** 构建基线，仍依赖系统图形、X11 / Wayland、PipeWire / Portal 组件；不是静态或通用无依赖包。无代码签名；AppImage 开发产物不随本次正式版发布，Flatpak 未交付。[打包契约](PACKAGING.md)、[系统依赖](../packaging/linux/README.txt)。 |

## X11 与 Wayland 的明确差别

- **X11**：可选全局快捷键与输入元数据采集。边框 / 控制器分离和输入穿透已有私有 GNOME 实测；物理键盘布局、自动重复、不同窗口管理器 / 客户端装饰、多显示器与反复移动仍需扩充验收。快捷键默认关闭，注册失败可用按钮控制。[快捷键](GLOBAL-SHORTCUTS.md)、[窗口拾取](X11-WINDOW-PICKER-QA-2026-09-08.md)。
- **Wayland**：遵守 ScreenCast Portal 分享授权；固定大小裁剪只能在已授权来源内移动。没有通用全局透明取景框、窗口吸附接口或被动全局按键 / 点击采集。快捷键取决于 GlobalShortcuts Portal 的桌面支持与授权，不能以私有总线测试替代真实桌面验收。
- Wayland 录制显示器时，控制器需要放到录制区域外；不能保证自动排除自己，也不能强制被遮挡应用持续刷新。优先选窗口来源。现有 nested GNOME / 软件渲染验收不覆盖所有物理 GNOME、KDE、wlroots 或显卡驱动。[Wayland 原生记录](WAYLAND-NESTED-QA-RESULTS-2026-09-06.md)、[准备 / 控制器与中文验收](RECORDER-LOCALIZATION.md)。

## 明确未完成，后续另开迭代

1. **冻结 Vector v2 / schema 9 集成**：实验 WPF layout / brush / batch 已有 14 个真实 Windows 精确案例和有界资源实现，但未启用为正式作者。新的持久化 stage、版本迁移、物理布局预览、DesiredSize / RenderSize 手柄规则与完整用户流程仍需整体完成并重新验收。未完工作已推送到 [archive/schema9-vector-v2-20260909](https://github.com/tiansongyu/gifromscreen/tree/archive/schema9-vector-v2-20260909)，归档提交为 [`abe741b1ec6bd8fee83f24c21b23df919788e8c2`](https://github.com/tiansongyu/gifromscreen/commit/abe741b1ec6bd8fee83f24c21b23df919788e8c2)；不能按某个切片测试通过将其拼入本次发布。[实验结果](WPF-VECTOR-RENDERING.md)、[冻结接线计划](WPF-VECTOR-V2-INTEGRATION.md)。
2. **平台与长期录制**：物理 GNOME / KDE / wlroots、硬件渲染、多显示器 / 混合 DPI、实际输入设备、热插拔 / 休眠、长时间高分辨率与反复 retarget。已有存储压力测试不是实时屏幕录制达标证据。[当前 Linux 门禁](LINUX-STATUS.md)、[存储测量边界](FULL-RESOLUTION-STORE-QA-2026-09-07.md)。
3. **精度与作者参数**：继续 Cinemagraph 的真实 Stroke / Boolean / 擦除几何对齐，以及完整标注、文本、进度参数和更多透明 / 分数尺寸 / 旋转组合；保留历史失败与旧工程像素，不静默更换渲染版本。
4. **语言与可访问性**：完成余下应用 UI 的中英文迁移、共享控件提示、其余 27 套译文；独立验证字体区域字形、复杂文字 / RTL / IME、全键盘导航、离屏 Tab 自动滚动和辅助技术。语言可选择 / 字体有 glyph 不等于完整可用。
5. **预览与小窗口**：窄编辑器修复已通过该版本原生复验；但首页在 **680 像素宽、150% 缩放**时，两列启动卡片仍有文字重叠，属于已知显示限制。页头仍可用，暂可按 **Ctrl+0 恢复 100%** 查看正常首页布局。[原始观察与验收](NARROW-EDITOR-QA-2026-09-09.md)明确区分两者。更多大字体 / zoom / popup、离屏导航与超大画布分块预览仍待完成，不得缩减设置或改变保存像素来规避问题。
6. **分发与桌面集成**：AppImage 尚有对应源材料 / 许可、桌面注册和更广平台门禁；Flatpak 还需权限与持久项目 / 导出流程接线。签名、完整机器可验证 SBOM、自动更新、托盘 / 单实例等尚不能按功能目标表宣称交付。[AppImage 开发记录](APPIMAGE-RUNTIME-QA-2026-09-08.md)、[Flatpak 计划](FLATPAK-PLAN.md)。

## 范围外与隐私提醒

当前优先 Linux x86_64；**macOS 暂缓，未交付**。不录制音频，只导出 GIF；视频 / APNG / WebP / AVIF / PSD / 图片序列等其他导出、Windows 专属集成和上游品牌 / 界面像素级复制不在本次范围。[功能目标与排除项](FEATURE_MATRIX.md)中的里程碑与“平台可实现”不是完成标记。

按键 / 点击元数据默认不采集。开启后，`.gfsproj` 可能保留敏感原始输入和共享 replay pool；复制少量帧不保证删去整个共享历史。
分享导出的 GIF 可排除工程元数据，但已经画在图像上的文字、按键或其他信息仍会出现在 GIF 中。

发布之后继续按可复现问题、明确版本与证据迭代；不承诺“所有 BUG 已解决”，也不把有限 golden、模拟设备或软件桌面验收扩大为完整一比一完成。
