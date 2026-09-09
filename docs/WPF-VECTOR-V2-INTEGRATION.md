# WPF Vector v2：持久化与编辑器接线审计

日期：2026-09-09。状态：**实施设计与验收清单，不是功能完成报告**。
审计基线为 `232d56a` 至 `a4a8004`；同期 ROI / work-meter 优化仍由独立任务推进。
本文件不改变工程格式、生产 dispatch 或既有像素。

## 已有证据与尚未完成的部分

独立 WPF candidate 已在真实 Windows 的 14 个完整向量案例中逐通道精确匹配，覆盖四种形状、旋转、圆角、描边和极端 layout clip。
这证明的是对应输入的 **layout → brush paths → coverage → PM canvas**；不证明任意输入、任意 DPI，也不证明用户已能创建、保存、复制、重开和导出 v2 工程。
早一轮 `/tmp/gfs-wpf-vector-candidate-4e28a35/vector-candidate.json` 为 **13/14**，不能误引为最终 14/14 报告。
两形状案例的残差由去掉无 clip 形状的多余独立 PM 分组解决，不是放宽容差或改黄金数据。
最终 measured-250M 对照在 Rust 1.98 / 1.88 的报告分别为
`/tmp/gfs-wpf-vector-meter250-1_98.tIeuV4/vector-candidate.json` 与
`/tmp/gfs-wpf-vector-meter250-1_88.q39Y4V/vector-candidate.json`，本次只读复核两者 SHA-256 均为
`90cacb08b8a46e3bd8db529c23146de8d9f31b54f0e84efc4e7d3e90112f0c9f`。
完整来源及规模证据见 [WPF vector rendering](WPF-VECTOR-RENDERING.md)；这些 Windows artifacts 的实际运行时为 .NET 9.0.20，不能把审查的 v9.0.0 源码 tag 写成实际已加载 runtime 版本。
后续 ROI 实验正在独立收尾，本设计不冻结其新的性能数字。

目前生产作者仍创建 `version: 1`，使用 `VectorCanvasPbgra8PngV1`；新 WPF API 是显式 candidate，未替换这个保存契约。
上游范围以 [Shape parity 审查](SHAPE-PARITY-2026-09-09.md) 为基础：固定 ScreenToGif `a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`、WPF 9.0.0 / `a04736acb8edb533756131d3d5fc55f15cd03d6a`，96-DPI 参考空间。

## 1. 显式版本与 schema 9

建议新增 `CompositePrecision::VectorCanvasPbgra8PngV2`，wire 名称 `vector_canvas_pbgra8_png_v2`。
不能复用 V1 stage：V1 的 tiny-skia coverage、layout、单 mark 内部分组与新 WPF 路径不同。
也不能根据当前可见内容自动选择新算法，否则 Hide/Show 会改变别的 mark 的像素契约。

| 位置 | 最小改动 |
| --- | --- |
| [domain/vector_shape.rs](../crates/domain/src/vector_shape.rs) | `validate()` 仅接受版本 1、2；数值范围不变。增加版本相关 `required_schema_version()`：1→8、2→9。保留 `VectorShape::default()` / 旧版本常量为 1，另加显式 v2 作者构造入口。 |
| [domain/frame_geometry.rs](../crates/domain/src/frame_geometry.rs) | 新 precision 与其 schema 9 要求；首个 Composite 仍必须 Legacy。即使新 stage 暂无 marks，仍要求 schema 9。 |
| [domain/model.rs](../crates/domain/src/model.rs) | `OverlayContent::required_schema_version()` 不再把所有 VectorShape 固定映射为 8；验证诊断里的固定 schema 8 也同步。扩充 stage/content/version 校验。 |
| [domain/lib.rs](../crates/domain/src/lib.rs) | `CURRENT_SCHEMA_VERSION = 9`；不重写旧工程。 |
| [domain/command.rs](../crates/domain/src/command.rs) | 审核 `EditCommand::required_schema_version()` 对 tracks、frames、Compound、inverse 的递归传播；沿用原子 apply 与 sticky schema upgrade。 |
| [project/schema_migration.rs](../crates/project/src/schema_migration.rs) | 增加 schema 9 的 checkpoint-before-journal、recovery、undo 后版本不降级测试。 |

持久化合法性矩阵：

| 内容 / 位置 | 策略 |
| --- | --- |
| 旧 `OverlayContent::Shape`、Vector v1 的原合法 timed / tail / stage 组合 | 保持原序列化、原绘制与原合法范围。 |
| Vector v2 | 仅 frame-owned，且 `cell.stage = Some(id)` 指向该 owner 的 V2 stage，Normal blending。 |
| V2 stage | 仅允许 Vector v2 marks；拒绝 v1、旧 Shape、raster、text 等，即使 hidden / opacity 0。空 cell / 空 stage 合法。 |
| v2 timed item、`stage=None` tail、旧 precision stage | 明确拒绝，不隐式转换、不借用 legacy 的时间采样。 |
| 同一帧先 V1 stage、后 V2 stage | 支持；各阶段独立保留，几何 / effects 的先后顺序不变。 |
| 同一个新作者 batch 混合 v1/v2 | 拒绝。用户要求转换须是未来明确命令，不能在 Apply 时猜测。 |

双向检查都必要：检查 V2 stage 内的内容，也检查每个 v2 mark 是否真的绑定 V2 stage。
`model.rs` 当前遍历 cell 的验证能覆盖隐藏层；不能只依赖 renderer 的 active-layer 列表，因为它已过滤 hidden / opacity 0。
旧 schema 8 / version 1 fixtures 应显式固定为旧值，不能通过批量改默认值让兼容测试失去意义。

## 2. 明确定义绘制、顺序与 post-mark opacity

新版本仍在阶段内按现有 `(z_index, track_index, item_index)` 稳定顺序绘制；跨阶段按 `render_steps` 顺序。
一次 V2 stage 先建立透明 PM canvas，画完全部 active marks，再把 canvas 一次 source-over 到前序帧，最后执行一次原有 WIC-compatible unpremultiply 边界。
不得按 track 重新分组或排序，也不得合并相邻 V1/V2 stages。

单 mark 规则必须成为 v2 contract，而不只是优化：

1. **无 layout clip 且 track opacity = 255**：fill 后 stroke 直接写同一个 stage PM canvas。不能先把每个形状画到独立透明 PM surface 再叠加；多一个 8-bit 分组会改变重叠舍入。
2. **有 layout clip，或 track opacity < 255**：在有界临时 visual 中完成 fill / stroke；如有 clip，将其 coverage 作用于这个完成的 PM visual；然后对完成的 mark **一次**施加 track opacity，最后 over 到 stage canvas。
3. track opacity 是本项目已有的 **post-mark alpha** 扩展，不是新增 WPF `Shape.Opacity` 属性；同一轨道多个 marks 仍逐 mark 应用，不改成一次整轨 opacity。
4. 不允许把 track opacity 分别乘入 fill、stroke 两个 brush；例如两者 alpha 128、track opacity 128，重叠区会产生不同结果。保留原先 brush 的 premultiply 与 C64 coverage 舍入，完成后再用现有 `mul_byte` 缩放 PM 的四个通道。

clip 与 opacity 的整数运算顺序可差 1 LSB；本契约明确选择 **clip 完成后再 post-mark opacity**。
这不声称与另一个未暴露的 WPF Visual.Opacity 属性在所有边缘逐字节相同。
layout clip 是完成 visual 的单独 PM coverage pass，不是各 primitive 与 clip 的几何交集，也没有中间 PNG 反预乘。

只要 stage 有 active marks，未触达的半透明 base pixels 也经过 stage 的整面 PM/PNG 边界；不能用 ROI 优化省掉这部分转换。
完全没有 active marks（例如全 hidden / track opacity 0）则沿用不量化 base 的行为。
“存在 active mark 但 paint alpha 全零”不应被偷偷改成另一种 stage 边界规则。

具体接线位置：

- [render/overlay.rs](../crates/render/src/overlay.rs)：`stage_overlay_layers` 做新的安全 dispatch 校验；`composite_stage` 添加独立 V2 分支；`composite_vector_canvas` 的 V1 分支保持原样。`OverlayRenderPlan` 的 detached layers 保留 stage、opacity 和已解析的稳定次序；不得要求重新查完整项目来恢复顺序。
- [vector_shape/wpf_surface.rs](../crates/render/src/vector_shape/wpf_surface.rs)：在 `paint_visual` 承接上述 post-mark opacity。可添加内部借用 entry（shape 引用 + opacity），candidate 的现有 `&[VectorShape]` API 作为 opacity 255 wrapper，不复制整个 geometry。
- compositor 给 batch 的预算应先扣除仍存活的 destination，再计 PM canvas、geometry 实际 capacity、临时 visual、coverage、scanner 与最终合成循环。复用实际 work meter，不能按形状数量死分配各阶段配额或逐 mark 重置总额。
- direct clip render、detached plan、editor preview、transitions、Motion bake、streaming/GIF 必须到同一个 stage 分支；缺失或错误 stage 必须显式失败，不以 V1 fallback 掩盖。

## 3. 作者、几何 dispatch 与递归陷阱

[editor/paint_stage.rs](../crates/editor/src/paint_stage.rs) 的 `author_vector_shape_track` 当前固定选择 V1 precision。
它应先验证整个新 batch 版本一致，再选择 V1 或 V2，继续复用 `author_with_precision`、`author_owner`、`seal_existing_tails`。
通用 `author_frame_owned_track` 不改变默认 precision；不能令 text / drawing / annotation 一并进入新向量契约。

[desktop/editor_vector_shapes.rs](../apps/desktop/src/editor_vector_shapes.rs) 的 `apply_vector_shapes` 保留：

- `OverlaySelectionAnchor` 的 project / revision / selection 校验、reference frame 与真实 authoring-stage canvas 校验；
- `selected_frame_cells` 的完整 selection 检查与不连续范围，mark 顺序和唯一 ID；
- 独立 stage 与上一批 tail 封存，一次 Compound / 一次 Undo；
- 256 对象、总 cell / mark、16 MiB command metadata 等原预算，不截短输入；
- raw capture metadata、capture binding / clock、输入 replay pool、前序 pixels 均不改写。

[vector_shapes/draft.rs](../apps/desktop/src/vector_shapes/draft.rs) 的新插入必须显式创建 v2，修改样式、移动或重编辑已有对象则保留对象原 version。
`ShapeStyle` 默认不应意外升级全部旧 fixtures；view / no-input / locale 切换不得写项目或改版本。
原草稿的稳定对象 ID、开始时 snapshot、映射变化取消、Ready 数据保留与 Delete 输入范围保护继续存在。

**必须先拆递归，再加公共 dispatch**：
`wpf_vector_shape_geometry` 在 [vector_shape/wpf.rs](../crates/render/src/vector_shape/wpf.rs) 的 Triangle / BlockArrow 分支清零 local origin / rotation 后调用 `vector_shape_geometry(&local)`。
若后者根据 `local.version == 2` 再进入 WPF 几何，便无限递归；`wpf_brush::stroke_spine` 也经过这条链。
优先抽出不做版本 dispatch 的私有 raw custom-contour helper，分别供两版本复用。
最小临时替代是仅在局部副本强制 `version = 1` 后调用旧 raw 路径；不能修改保存值，也不能绕过入口 v2 验证。
新 dispatch 必须有四种形状的真实调用测试，而不只测独立 WPF API。

## 4. Preview：先物理 layout，后显示缩放

[vector_shapes/preview.rs](../apps/desktop/src/vector_shapes/preview.rs) 当前 `RasterPreview::request` 调用 V1 `render_vector_shapes_preview(canvas, output, ...)`，保存一份 generation / canvas / output key，并有 16 MiB texture-worker 预算。
新路径不能直接把 `render_wpf_vector_shapes` 的物理 `size` 换成缩小后的 output，更不能先缩放 hundredths bounds 再做 WPF layout rounding。
正确顺序是 **当前 authoring stage 的物理请求 → WPF layout / geometry → 共享像素执行器 → 预览显示缩放**。

100% 的 CPU 合成应与同一 stage 保存后渲染相等；Fit / 200% 允许明确的显示采样，但不会更改保存插值、几何或笔宽。
若使用 ROI / scaled projection 避免巨大中间图，layout 和裁剪参考空间仍必须是物理空间；这需要独立对照，不能凭小纹理的视觉相似认定完成。
PM overlay 上传到 egui 时的转换与 GPU 合成也不能冒充最终 WIC 字节一致；若要求准确 composite preview，应在 CPU 用真实 base 和 stage 组合后展示，而非只比较透明 overlay。

4K 单张 PM canvas 已约 31.6 MiB（3840×2160×4），不能机械沿用 16 MiB 旧 preview 上限后声称 4K 可用；更大常见画布及 destination / scratch 还需另计。
明确区分输出纹理上限、aggregate working memory 和工作量上限，复用已在推进的 ROI / measured-budget 实现。
超限显示可恢复错误，不回退 V1、不发布半张图、不清空用户草稿。

保留一个 background task slot、取消过期结果，仅 key 匹配时发布；version / renderer contract 必须参与缓存有效性。
实际绘制矩形、PPP、scroll、zoom 的现有映射保护不移除；任意显示缩放不修改物理请求值。

## 5. 选择、手柄与缩放不是同一个几何契约

[vector_shapes/geometry.rs](../apps/desktop/src/vector_shapes/geometry.rs) 的 `GeometryCache::ensure` 应通过版本 dispatch 获取几何。
当前 `handles()` 从 requested bounds 计算位置；直接照搬到 v2 会在分数尺寸、粗笔宽导致 RenderSize 增长时偏位。
建议由 render 提供只读 layout metrics（offset、requested size、desired size、render size、local clip），UI 不另造 WPF 排版公式。

### Point 与 marquee

上游 point selection 枚举 Shape 并调用 `RenderedGeometry.FillContains`，不是测试最终 alpha；透明填充的闭合内部也能被选中。
上游 marquee 却调用 `VisualTreeHelper.HitTest` + `GeometryHitTestParameters`，是视觉几何查询，不能自动宣称与 point 的 fill-only 查询相同。
见 [point 路径](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L209) 和 [marquee 路径](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L536)。

V2 应分别定义 point fill-containment 与 marquee visual-intersection；不以 AABB 或 paint-alpha 替代。
待原生确认：clip 外但仍处于 RenderedGeometry 内的点、仅碰描边的框选、None 与透明 fill、退化形状、重叠对象的选择次序。
当前 Linux point 反向 paint-order 查询与上游 `FirstOrDefault` 枚举也不能未经确认就称完全相同；不借 v2 接线暗改 v1 用户选择规则。

### 已确认的 DesiredSize / RenderSize 分工

| 操作 | 上游实际使用值 |
| --- | --- |
| 手柄 / 选中框 Arrange | `ElementAdorner.ArrangeOverride` 使用 **adorner 自己的 DesiredSize**。函数虽读取 adorned.DesiredSize，但那两个局部变量未用于布局。 |
| adorner 默认 Measure | WPF `Adorner.MeasureOverride` 返回 adorned **RenderSize**，因此该固定布局下手柄依据 RenderSize，而非请求尺寸。 |
| 八个 resize handler | 从 adorned **DesiredSize** 加减拖动增量；上 / 左同时据此调整 Canvas.Left / Top；随后写显式 Width / Height，最小 10 个布局单位，并有父画布边界规则。 |
| move 边界 | DesiredSize 对照 parent.ActualWidth / Height（源码还含 ±1 容差）。 |
| 旋转中心 | `ActualWidth / ActualHeight × RenderTransformOrigin` 经 TranslatePoint 映射到父元素；固定 origin .5/.5 时为实际 RenderSize 中心。 |

证据：[ElementAdorner Arrange](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L158)、[WPF Adorner.MeasureOverride](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/PresentationFramework/System/Windows/Documents/Adorner.cs#L64)、[move / resize](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L396)、[rotation](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L684)。

因此不能把 handles、resize 增量和旋转统一成 requested center，也不能统一改为 RenderSize。
Linux 手势仍应从 gesture-start snapshot 计算，避免逐帧积累舍入；若不同于上游逐次 WPF layout 的结果，需明确产品策略并用原生轨迹验证。
至少测 requested 1×2、stroke 10（矩形/椭圆可增长为 10×10；三角形/箭头的实际增长另有已测差异，均须考虑局部 clip）、分数尺寸 ties-even、旋转、父边界以及多选传播。
10 单位的交互最小值不是 domain 最小尺寸；不能因此拒绝已存的更小合法对象。
完整原生手柄位置、连续 resize 与重排时序仍是待验收项，源码确认不等于实际 egui 行为已经完成。

## 6. 持久化 / 用户流程验收矩阵

Vector 不增加 asset 文件种类；但 generic clone / remap 的存在不是免测理由。
[frame_clipboard.rs](../crates/editor/src/frame_clipboard.rs)、[editor_insert.rs](../apps/desktop/src/editor_insert.rs)、[project_copy.rs](../crates/application/src/project_copy.rs) 必须保留版本、steps、scope、mark 内容及正确的 owner / stage 对应关系，不重新生成形状或改变 raw metadata。

| 门禁 | 必须观察的结果 |
| --- | --- |
| 旧兼容 | 固定旧 Shape 与 Vector v1 各 precision / blend fixtures；decode→encode wire 不变，preview / direct / plan / GIF 解码结果保持原测试。打开、查看、SaveAs 不自动升级到 v2。 |
| schema / hidden | schema 8 携 v2 或新 precision 拒绝；未来版本拒绝；hidden、opacity 0、空 scope / 空 stage、错 owner / stage、v1/v2 混合非法组合完整覆盖。 |
| 新作者真实 Apply | 新建四形状 v2、改样式 / 排序、非连续选帧；一次 revision / Undo。实际 manifest 中版本 2、schema 9、新 precision，不只直接调用 candidate。 |
| 数值 / opacity / order | 透明 RGB、alpha 0/1/128/255；fill + stroke + track 128；有 / 无 clip；跨 track 交错 z；两个 V2 stages 不合并。旧 Multiply / Screen 保留，V2 非 Normal 原子拒绝。 |
| 前后几何 / effects | crop / resize 后 author v2，再 rotate / blur，再 author v2；先前 V1 与新 V2 共存；清除 / 替换既有 geometry 的原合法性规则保留，输入空间取所属 stage。 |
| Preview | 100% 与保存后 CPU render 比较；Fit / 200%、PPP 1/1.25/2、不同 source / final canvas、不规则 clip；view 操作无项目写入，stale worker 不发布。 |
| 手势 / 选择 | point 与 marquee 单独测；八 handles、rotation、move、fractional / narrow cases；layout / focus / locale 改变只取消未完成 gesture，Ready / 已确认草稿不丢失。 |
| Undo / Redo / journal reopen | Apply、样式 / visibility / opacity、几何前后步骤的撤销重做像素与 payload 对等；未 checkpoint 的成功回执能恢复。schema 升级按现有规则保持 9，不降回 8。 |
| Clipboard / Yoyo | 复制部分 owner → 删除原帧 / 原层 → 粘贴 → Undo / Redo / reopen；保留版本、相对绘制顺序和正确 stage 映射；不为向量新增 input replay，不更改 capture clock。已有 shared pool 仍遵循原引用保留契约，不能宣称少帧复制会裁净历史。 |
| 跨项目 insert | v1 来源与 v2 来源分别插入另一工程；schema 需求传播，原目标 legacy 排除插入区域规则不变；混合内容分属正确阶段。 |
| SaveAs | 保存到新路径后关闭 / 重开；原工程不变；复制所需原资源、input replay pools / snapshots 的原通路继续有效；v2 无多余烘焙资源。 |
| GIF / transitions / Motion | 从保存工程走真实 direct 与 detached plan、preview / transition、GUI 与 CLI export；相同 options 下导出一致，尺寸 / 帧数 / 时长正确。GIF 有量化，WPF 原始 RGBA 精确测试与 GIF 对照分开记录。 |
| 规模 / 失败 | 1080p、4K、极窄 / 大 overscan、256 objects、累计 budget 边界与 cancellation；既要能处理常规多形状，也不能跳过完整 batch / 未覆盖像素成本。 |

是否支持已 Apply 图层的对象级 re-edit，应由实际入口决定：现有 `apply_vector_shapes` 主要创建新 group，不能把可复制的持久化 marks 描述成已经拥有完整重编辑 UI。
若补该入口，必须保留原 version 与 authoring stage；显式转换才允许把 v1 改为 v2，并需单独像素变化确认。

## 7. 实施顺序与原子失败

1. **Domain / schema**：版本常量与显式构造、schema 9、双向 hidden-stage 校验、command / migration 回归。旧 fixtures 全部保留。
2. **Renderer**：先拆 raw-contour 递归，再加独立 V2 stage；接 measured batch / ROI 和 post-mark opacity；candidate 14 exact、旧 V1 bytes、direct / plan 和预算测试同时通过。
3. **作者**：`author_vector_shape_track` 同版本选择，draft 新建 v2，原有 stage sealing / Compound；在真实 workspace 完成 Apply、Undo / journal reopen。
4. **Preview / 交互**：物理 layout 后缩放、共享 geometry / layout metrics、独立 point / marquee、正确 handles / resize 参考值；真实 egui 原始事件与 native 窄尺寸验证，不以只测试 pure helper 代替。
5. **持久化完整链**：Clipboard / Yoyo / insert / SaveAs / reopen / GIF / Motion，以及混合旧工程；然后才能把作者默认 v2 作为完整用户功能发布。

metadata、version、stage、selection / revision anchor、数值与 command-size 的错误都应在执行 Compound 前拒绝；失败保留 draft、工程 revision、journal 与资源不变。
必须在提交前完成实际需要的几何 / 预算准备，或提供同等有界 preflight；不能先清草稿 / 提交，再因本来可预知的不支持路径报失败。
渲染取消 / 内存 / work 超限只丢弃私有未完成结果，不发布半成品、不改变保存内容；导出继续沿用现有临时产物提交策略。
持久化 schema upgrade 与 journal append 的 durable 边界沿用现有项目实现，失败不能只恢复内存而留不可恢复的成功回执。
整个接线不得通过隐藏旧对象、静默 bake、降级 V1 或更改 golden tolerance 来获得表面成功。
