# 形状工具：固定上游行为与兼容边界

日期：2026-09-09。本文是只读源码审计与下一轮建议，不是新功能完成、原生验收或发行声明。
参考为 ScreenToGif 2.43.2，已核本地 checkout 的 HEAD 精确为
`a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`。WPF 参数解释另引用 Microsoft 文档与 WPF 9.0.0 源码，不能把框架具备的属性等同于 ScreenToGif 已开放的控件。

## 结论与优先级

下一轮优先 **Triangle、rounded rectangle、Shape 直接操纵**。
保留本项目已有 Line 和 Arrow 行为；不要为追赶上游改坏已有工程。
Dash 可以作为额外增强，但不是此固定版本已经接通的 GUI 功能。

| 项目 | 固定上游实际行为 | 当前本项目 |
| --- | --- | --- |
| 形状种类 | 面板提供 Rectangle、Ellipse、Triangle、Arrow | Line、Arrow、Rectangle、Ellipse；缺 Triangle |
| 描边、填充 | 描边宽度、描边颜色、填充颜色；颜色含透明度 | 整数物理像素描边、RGBA、可选填充；另有轨道不透明度与混合模式 |
| 圆角 | 一个 Radius，0..100，仅给 Rectangle 的 RadiusX/RadiusY | 无圆角字段或渲染路径 |
| 虚线 | 控件类有 StrokeDashArray，但面板未绑定/提供编辑入口，默认空数组 | 无虚线；不能据此认定缺少上游已开放的虚线 UI |
| 直接操作 | 拖出形状；选择、框选/Ctrl 多选、移动、八方向缩放、旋转、删除 | 数字边界后 Add shape；自由笔迹另属 Drawing，不能替代 Shape 操纵 |
| 应用之后 | 光栅化到所选原始帧，清掉临时形状；保存 Undo 状态 | 新形状保持 frame-owned 内容和独立绘制阶段，可撤销，不应改成破坏性烘焙 |

形状列表与参数的直接证据是
[Editor.xaml:2806–2897](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml#L2806)，
类型映射及属性传递见
[Editor.xaml.cs:2399–2440](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L2399)。

## 确实存在的参数，而非推测

- 描边宽度和 Radius 均使用 `DoubleUpDown`，范围 0..100。其 `DoubleBox` 基类默认保留两位小数，步进默认 1，并在赋值时舍入；不是仅支持整数的控件。
  [Editor.xaml:2869–2879](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml#L2869)、
  [DoubleBox.cs:24–40、124–135](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DoubleBox.cs#L24)。
- Editor 设置的默认值是笔宽 4、黑色描边、Radius 0、透明填充；不要误用控件类的笔宽默认值 2。若调整新草稿默认值，不能修改已有工程中的样式。
  [Settings.xaml:334–338](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Resources/Settings.xaml#L334)。
- Radius 同时赋给矩形的两个轴；Ellipse 没有使用它，Triangle 的 Radius 代码被注释，Arrow 也不使用。修改属性会更新当前选中的形状。
  [DrawingCanvas.cs:459–514](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L459)、
  [同文件:649–662](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L649)。
- Rectangle 是 WPF 圆角矩形，不是模糊矩形或把整张图蒙成圆角。WPF 先将笔画中心线矩形内缩半个笔宽；半径在几何生成时分别夹到该中心线矩形宽/高的一半。两个输入半径相等，不代表宽高悬殊时两个有效半径仍相等。
  [WPF Rectangle.cs:154–166、221–224](https://github.com/dotnet/wpf/blob/v9.0.0/src/Microsoft.DotNet.Wpf/src/PresentationFramework/System/Windows/Shapes/Rectangle.cs#L154)、
  [RectangleGeometry.cs:434–443](https://github.com/dotnet/wpf/blob/v9.0.0/src/Microsoft.DotNet.Wpf/src/PresentationCore/System/Windows/Media/RectangleGeometry.cs#L434)。
- Triangle 是上方居中的顶点和下方两角构成的封闭三角形，顶点位置考虑半个笔宽；不是任意三顶点编辑器。
  [Triangle.cs:9–11](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/Shapes/Triangle.cs#L9)。
- 上游 Arrow 实际定义的是向右的封闭块状多边形；`X1/Y1/X2/Y2` 对方向的旋转代码被注释。它并不等同于本项目当前从边界左上到右下的线段箭头。若补块状箭头，必须作为新样式/种类，不能重解释旧 `ShapeKind::Arrow`。
  [Arrow.cs:74–104](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/Shapes/Arrow.cs#L74)、
  [当前 arrow_shape_pixel](../crates/render/src/overlay.rs#L995)、[当前端点/箭头尺寸](../crates/render/src/overlay.rs#L1220)。
- `DrawingCanvas.Shapes` 虽包含 Line，Editor 列表没有 Line，`RenderShape` 也没有 Line 分支。本项目 Line 是可保留的扩展，不应删除。
  [DrawingCanvas.cs:29–36](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L29)。

## 虚线：区分类能力、选中框与可用面板

`DrawingCanvas.StrokeDashArray` 是默认空 `DoubleCollection` 的依赖属性；创建四种形状及更新选中形状时都会转交给 WPF Shape。
但 Editor 的 DrawingCanvas 声明未设置它，`ShapeProperties_Changed` 也不赋它，整个 Shapes 面板没有 dash 控件。
`ShapesDashes` 仅找到 UserSettings 属性与默认资源值 1，没有使用点。
[DrawingCanvas.cs:98–108](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L98)、
[Editor.xaml:1526–1529](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml#L1526)、
[UserSettings.cs:2284](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif.Util/Settings/UserSettings.cs#L2284)、
[Settings.xaml:334–337](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Resources/Settings.xaml#L334)。

画面里可见的虚线选中边框来自 `ElementAdorner` 的 `{5}`，不是导出的形状描边。
Apply 会先 DeselectAll，再生成形状图像；不能把截图中的选中框当作 dash 功能证据。
[ElementAdorner.cs:310–336](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L310)。

若以后开放虚线，WPF 的 dash/gap 数字是**相对笔宽的倍数**，不是绝对像素：例如 `[2,1]` 配 3 px 笔宽，对应名义 6 px 划段、3 px 间隔。实际可见长度还受端帽影响。奇数数组会重复一次组成偶数序列；负值按绝对值解释。这些是框架行为，不是该上游面板的额外选项。
[Shape.StrokeDashArray](https://learn.microsoft.com/en-us/dotnet/api/system.windows.shapes.shape.strokedasharray?view=windowsdesktop-9.0)、
[DashStyle.Dashes](https://learn.microsoft.com/en-us/dotnet/api/system.windows.media.dashstyle.dashes?view=windowsdesktop-9.0)。
新功能须显式定义空/全零数组、零笔宽、端帽、闭合轮廓连续性、偏移及资源上限；不要直接把上述数组当作像素长度，也不要假定 Pen 文档的默认端帽就是 Shape 的默认端帽。

## 坐标和像素单位

上游交互用 WPF 元素坐标：`MouseEventArgs.GetPosition` 得到布局坐标，而非持久化桌面物理坐标。
最终形状通过 `GetScaledRender(ScaleDiff, ImageDpi, GetImageSize())` 变成指定像素尺寸的 PBGRA32 图像；该 helper 将绘制区域乘 `scale`，再使用目标 DPI 光栅化。
`ScaleDiff = UI scale / ImageScale`，`ImageDpi = ImageScale * 96`；忽略边界舍入时，局部长度到输出像素的合成比例为 UI scale，不是编辑器 Zoom。
因此仅在 96-DPI/1× 对照空间中，形状数值才可直接视为相同数量的输出像素。已有非 96-DPI 边界不能凭公式推导就宣布验收完成。
[DrawingCanvas.cs:209–279](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L209)、
[ZoomBox.cs:350–375](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ZoomBox.cs#L350)、
[ImageMethods.cs:2104–2161](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/ImageUtil/ImageMethods.cs#L2104)。

Linux 的新作者接口应统一存**当前绘制阶段的图像物理像素**，界面缩放仅影响映射与手柄大小。
采用有版本、有限且有界的数值表示；描边/圆角至少保留上游数值输入的两位小数。拖拽亚像素量的量化精度必须显式规定和测试，不能把任意浮点数、UI DPI 或缩放倍数写成未经定义的项目几何。
不要将拖拽得到的整数包围盒当成圆角半径，也不要在缩放视图时连带修改笔宽。

## 直接操纵的真实范围

- 插入模式从 Down/Move/Up 建立临时 Shape。实现有 `-0.6` 的矩形收缩和过小形状移除规则；它们是参考实现细节，应进入对照案例而非凭视觉猜一个偏移。
- 选择模式支持点击形状、几何框选、Ctrl 累加/取消选择；选中的形状会被提到前方，同时保留组内顺序。
- `ElementAdorner` 开启 move/resize/rotate，提供四角和四边共八个缩放手柄。移动/缩放受父画布约束；缩放处理含 10 个布局单位的最小尺寸，不是我们的物理像素存储约束。
- 旋转中心为元素中心，拖旋转柄得到整数角度；Alt/Ctrl/Shift 加滚轮分别旋转 90°/1°/20°。有重置旋转和移除菜单、Delete/Backspace 删除；Ctrl+C/V 在该类中只是 TODO，不能算已实现。
- 参数更改作用于当前选中的临时形状；多选操纵会向其他已选形状传播位置、尺寸和角度增量。

点击命中使用 `RenderedGeometry.FillContains`，不等于检查最终非透明像素；透明填充的闭合形状也可能从内部选中。新工具必须明确命中策略，不能以可见像素碰撞或矩形包围盒冒充所有 Shape 几何。Delete 的事件处理还必须有本应用自己的输入范围测试，保证编辑草稿时不会误删时间线帧；不能从上游这个 handler 的存在就推定全局快捷键已经安全隔离。

证据：[DrawingCanvas.cs:209–365](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L209)、
[选中与层级:536–606](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L536)、
[组操纵:665–702](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/DrawingCanvas.cs#L665)、
[ElementAdorner.cs:116–145](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L116)、
[移动/缩放:396–483](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L396)、
[旋转:684–712](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/ElementAdorner.cs#L684)。

Apply 需要至少一个形状和选中帧；先保存 Undo 状态，取消选中手柄，再光栅化、删除所有临时形状并将图像叠到选中帧的 PNG 上。
上游这里没有把可重编辑 Shape 对象写入项目；Apply 后重编辑不是已证明的基线能力。
[Editor.xaml.cs:2443–2473](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L2443)、
[OverlayAsync:5591–5628](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5591)。

本项目的手绘拖拽只生成 `OverlayContent::Drawing` 的点序列，不能当作形状对象的直接操纵。
可复用 [drawing_preview](../apps/desktop/src/drawing_preview.rs) 与 [editor_canvas](../apps/desktop/src/editor_canvas.rs) 的实际绘制矩形映射、原始事件顺序、失焦/布局变化/语言切换后的手势中止规则；不可直接复用点序列、笔压、绘图提交或 Shape hit-test。
Shape 需要独立的有界多对象草稿、稳定对象 ID、选中集合、命中测试和基于手势初始副本的变换。

## 当前数据、渲染与兼容契约

[OverlayContent::Shape](../crates/domain/src/model.rs#L386) 只有 `kind / bounds / stroke_width / stroke / fill`；
`bounds` 为非空整数物理像素矩形，笔宽为 `u16`，现有 `ShapeKind` 四个值已经被保存。
[shape UI](../apps/desktop/src/editor_ui.rs) 的 `show_shape_overlay_toolbar` 只编辑这些数值并创建新轨道，没有 Shape 专用画布操纵草稿。
[CPU shape 路径](../crates/render/src/overlay.rs#L873) 使用像素中心测试；矩形和椭圆是已有的内侧描边路径，没有圆角、虚线、三角形或对象旋转字段。现有 WPF PBGRA8/PNG 阶段精度不等于这些几何已经逐像素对齐 WPF。

建议采用**新的有版本向量形状内容**，或等价的显式新渲染版本分支，承载 Triangle、圆角、细分笔宽和对象局部变换；不要重解释旧 `Shape` 的五个字段。
独立的新内容变体优于同时存互相矛盾的整数笔宽与浮点覆盖值。未来 dash 样式也应明确是笔宽倍数。
这是方案建议，尚未新增任何 domain variant 或 schema。

实现时必须满足：

1. 旧 `type=shape`、四种 `kind`、`bounds`、`stroke_width`、RGBA 和 `fill=None`/`Some(alpha=0)` 序列化语义不变，旧像素路径逐字节不变。保留 Line 和当前 Arrow 扩展。新圆角设为 0 也不能偷偷把旧对象切到新抗锯齿算法。
2. 新几何产生新的最低 schema 要求；当前版本为 7。若仅追加可忽略的可选字段却不提高版本，旧读取器可能把圆角/虚线静默画成旧实线方角，必须避免。`required_schema_version` 应检查 timed items 和 frame cells 内的全部内容，包括隐藏轨道。
3. [OverlayTrack](../crates/domain/src/model.rs#L473) 的 ID、名称、visible、opacity、blend、items 与 frame_cells 区别保持；没有 frame_cells 仍是旧 timed 轨道，不自动迁移。
4. [FrameOverlayCell/Mark](../crates/domain/src/frame_overlay.rs#L177) 的 owner frame ID、mark ID、stage、z-index、scopes、input_replay，以及旧 annotation scope 都不能因打开新工具而改写。新增组仍经 [author_frame_owned_track](../crates/editor/src/paint_stage.rs#L29) 建独立绘制阶段；Normal 与其它 blend 的精度策略沿用已有规则。
5. 对象旋转是对象局部几何，不得使用旋转整帧的 `FrameRenderStep::Rotate` 来冒充。既有 stage→crop/resize/effect→新 stage 的顺序不变；预览、转场端点、导出共用同一个新几何执行器。
6. 下一轮先完成 Apply 前的多对象直接操纵与一次原子提交；如果增加对已保存组的重编辑，应显式定义整组/选中帧范围、保留身份，并拒绝无法确定来源阶段的混合组，不能根据当前可见帧猜原作者意图。
7. 草稿、选中对象、曲线/轮廓工作量和生成 marks 均有界。沿用 40,000 cells、100,000 marks、16 MiB 作者命令元数据等现有上限；取消、失效 anchor 或预检失败不得部分写入 journal。对任意 dash 数组还需独立段数/最短周期预算，防止零周期死循环。

## 最小下一轮验收

1. Triangle：填充-only、描边-only、半透明混合、细/粗笔宽；对照实际 WPF Triangle 路径。旧四种 Shape 的像素 golden 和旧项目保存重开保持不变。
2. Rounded rectangle：Radius 0、0.25、普通值、超过半宽/半高的值；窄高/宽扁矩形、笔宽 0/0.5/2.25、透明填充。确认圆角只影响矩形，不是整帧圆角蒙版。
3. Direct manipulation：四个拖动方向、单选/多选、八柄缩放、移动、删除、旋转与重置；每次 gesture 从初始副本计算，控件不进入导出画面。未经 Apply 不改工程，Apply 一次 Undo/Redo 完整。
4. 坐标安全：Fit/100%/200%、UI zoom 1/1.25/2、滚动/平移、布局变化后同批 Up、Down/Up 后又移动、失焦/PointerGone/Escape；同一物理几何得到相同持久值，不因语言变化缩放笔宽或移动对象。
5. 所有权/顺序：非连续选中帧、已有半透明旧层、形状→resize/rotate/effect→新形状、隐藏层、复制后删原→Paste/Yoyo/工程插入→Save As/reopen/GIF；保持原始 metadata/clock，不伪造输入关联。
6. 资源/失败：非法空尺寸、溢出/非有限参数、被删除目标、过期草稿、取消、超 marks/命令预算，失败前后 manifest/journal 一致。
7. 新 WPF 参考生成器使用公开 Shape/RenderTargetBitmap/WIC API，新增真实 Triangle/rounded fixtures 和来源版本哈希；96-DPI 物理像素算术先闭合，再单列非 96-DPI。现有其它图像效果 golden 不能冒充这些新几何已通过。
8. Dash 留作独立可选增强：空/奇数/零段数组、笔宽变化时比例、闭合接缝、端帽及取消/预算。若只实现可控子集，要明确界限，不标为已复刻上游 GUI 虚线选项。
