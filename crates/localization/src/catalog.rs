//! Auditable initial UI messages. This is not the complete application catalog.
//!
//! Future embedded resource catalogs must preserve these IDs and named argument
//! contracts; callers need not switch to English-string lookup. This slice uses
//! count labels rather than pretending to implement every language's plural
//! rules. Catalog loading, full translation and shaping remain separate gates.

use thiserror::Error;

use crate::Language;

/// Maximum formatted-message size; values are preserved whole or rejected.
pub const MAX_FORMATTED_MESSAGE_BYTES: usize = 64 * 1024;

struct Entry {
    id: &'static str,
    parameters: &'static [&'static str],
    english: &'static str,
    chinese: &'static str,
}

impl Entry {
    fn translation(&self, language: &str) -> Option<(&'static str, &'static str)> {
        let (text, tag) = match language {
            "en" => (self.english, "en"),
            "zh" => (self.chinese, "zh"),
            _ => return None,
        };
        (!text.is_empty()).then_some((text, tag))
    }

    fn resolve(&self, language: &str) -> LocalizedText {
        if let Some((text, language_tag)) = self.translation(language) {
            LocalizedText {
                text,
                language_tag,
                source: CatalogSource::RequestedCatalog,
            }
        } else {
            LocalizedText {
                text: self.english,
                language_tag: "en",
                source: CatalogSource::EnglishFallback,
            }
        }
    }
}

macro_rules! messages {
    ($( $variant:ident => ($id:literal, [$($parameter:literal),*], $english:literal, $chinese:literal) ),* $(,)?) => {
        /// Stable typed identities for this initial UI message set.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum Message {
            $(#[doc = concat!("Message `", $id, "`.")] $variant,)*
        }

        /// All initial message keys, for coverage and parameter-parity checks.
        pub const ALL_MESSAGES: &[Message] = &[$(Message::$variant,)*];

        impl Message {
            const fn entry(self) -> Entry {
                match self {
                    $(Self::$variant => Entry {
                        id: $id,
                        parameters: &[$($parameter),*],
                        english: $english,
                        chinese: $chinese,
                    },)*
                }
            }

            /// Stable catalog identifier, never a translated English-string key.
            pub const fn id(self) -> &'static str {
                self.entry().id
            }

            /// Required named arguments. Values are opaque user/application data.
            pub const fn parameters(self) -> &'static [&'static str] {
                self.entry().parameters
            }
        }
    };
}

// Each row requires both translations and one shared named-parameter contract.
// Keep IDs stable when wording changes. The names/paths supplied as arguments
// are never looked up as messages or recursively interpreted as templates.
messages! {
    LanguageSettingsTitle => ("language-settings-title", [], "Language", "语言"),
    LanguageChoice => ("language-choice", [], "Application language", "应用语言"),
    LanguageSystem => ("language-system", [], "System default", "跟随系统"),
    LanguageEffective => ("language-effective", ["language"], "Current language: {language}", "当前语言：{language}"),
    LanguageUnavailable => ("language-unavailable", ["language"], "The {language} catalog is not available; English is in use.", "{language} 的译文尚未提供；当前使用英语。"),
    LanguageCoverage => ("language-coverage", ["translated", "total"], "Translated messages in this initial set: {translated}/{total}. Full-interface translation is not complete.", "本轮消息集已翻译：{translated}/{total}。整个界面的翻译尚未完成。"),
    LanguageSaveHint => ("language-save-hint", [], "Changes apply immediately and are saved automatically.", "更改会立即生效并自动保存。"),
    SettingsLoading => ("settings-loading", [], "Loading preferences…", "正在读取偏好设置…"),
    SettingsSaving => ("settings-saving", [], "Saving…", "保存中…"),
    SettingsSaved => ("settings-saved", [], "Saved", "已保存"),
    SettingsUnsaved => ("settings-unsaved", [], "Unsaved changes", "有未保存的更改"),
    SettingsLoadFailed => ("settings-load-failed", ["reason"], "Could not load preferences: {reason}", "无法读取偏好设置：{reason}"),
    SettingsSaveFailed => ("settings-save-failed", ["reason"], "Could not save preferences: {reason}", "无法保存偏好设置：{reason}"),
    SettingsRetry => ("settings-retry", [], "Retry", "重试"),
    SettingsReload => ("settings-reload", [], "Reload saved preferences (discard unsaved changes)", "重新读取已保存的偏好设置（丢弃未保存的更改）"),
    SettingsUnavailable => ("settings-unavailable", [], "Preferences cannot be saved in this environment.", "当前环境无法保存偏好设置。"),
    SettingsNewerChoiceKept => ("settings-newer-choice-kept", [], "Saved preferences loaded; your newer choice was kept.", "已读取保存的偏好设置，并保留了您刚刚作出的选择。"),
    HeaderPreview => ("header-preview", [], "Linux capture preview", "Linux 录制预览版"),
    HomeTitle => ("home-title", [], "Create an animated GIF", "创建 GIF 动画"),
    HomeDescription => ("home-description", [], "Capture, edit frame by frame, and export locally.", "录制画面、逐帧编辑并在本地导出。"),
    HomeScreenRecorder => ("home-screen-recorder", [], "Screen recorder", "屏幕录制"),
    HomeScreenDescription => ("home-screen-description", [], "Record a Linux monitor, window, or physical-pixel region.", "录制 Linux 显示器、窗口或按物理像素指定的区域。"),
    HomeCameraRecorder => ("home-camera-recorder", [], "Camera recorder", "摄像头录制"),
    HomeCameraDescription => ("home-camera-description", [], "Preview and record a local camera. Audio is not captured.", "预览并录制本地摄像头，不录制音频。"),
    HomeBoardRecorder => ("home-board-recorder", [], "Drawing board", "画板"),
    HomeBoardDescription => ("home-board-description", [], "Record a canvas with pen, highlighter, and eraser tools.", "使用画笔、荧光笔和橡皮擦绘制并录制画板。"),
    HomeOpenProject => ("home-open-project", [], "Open project", "打开工程"),
    HomeOpenDescription => ("home-open-description", [], "Open an existing editable .gfsproj directory.", "打开已有的可编辑 .gfsproj 工程目录。"),
    HomeNewAnimation => ("home-new-animation", [], "New blank animation", "新建空白动画"),
    HomeNewDescription => ("home-new-description", [], "Start with a transparent or solid-color canvas.", "从透明或纯色画布开始创建动画。"),
    HomeImportGif => ("home-import-gif", [], "Import GIF", "导入 GIF"),
    HomeGifDescription => ("home-gif-description", [], "Decode a GIF safely into a new editable project.", "安全解码 GIF，创建可编辑的新工程。"),
    HomeImportImage => ("home-import-image", [], "Import image", "导入图片"),
    HomeImageDescription => ("home-image-description", [], "Import PNG, JPEG, BMP, or WebP as a one-frame project.", "将 PNG、JPEG、BMP 或 WebP 图片导入为单帧工程。"),
    HomeImportSequence => ("home-import-sequence", [], "Import image sequence", "导入图片序列"),
    HomeSequenceDescription => ("home-sequence-description", [], "Build an animation from ordered PNG, JPEG, BMP, or WebP files.", "使用有序的 PNG、JPEG、BMP 或 WebP 图片创建动画。"),
    HomeImportVideo => ("home-import-video", [], "Import video", "导入视频"),
    HomeVideoDescription => ("home-video-description", [], "Trim a local video into an editable GIF project with FFmpeg.", "使用 FFmpeg 截取本地视频，创建可编辑的 GIF 工程。"),
    HomeAutomaticTasks => ("home-automatic-tasks", [], "Automatic editing tasks…", "自动编辑任务…"),
    HomeRecentProjects => ("home-recent-projects", [], "Recent projects", "最近的工程"),
    RecentRefresh => ("recent-refresh", [], "Refresh", "刷新"),
    RecentEmpty => ("recent-empty", [], "Projects you open or create appear here. Removing an entry never deletes the project.", "打开或创建的工程会显示在这里。从列表移除不会删除工程。"),
    RecentMissing => ("recent-missing", [], "Missing or moved", "不存在或已移动"),
    RecentRemove => ("recent-remove", [], "Remove from list", "从列表移除"),
    HomeContinueEditing => ("home-continue-editing", ["name"], "Continue editing {name}", "继续编辑 {name}"),
    BackToHome => ("back-to-home", [], "Back to home", "返回首页"),
    SettingsTitle => ("settings-title", [], "Settings", "设置"),
    OpenButton => ("open-button", [], "Open", "打开"),
    BrowseButton => ("browse-button", [], "Browse…", "浏览…"),
    CancelButton => ("cancel-button", [], "Cancel", "取消"),
    CloseButton => ("close-button", [], "Close", "关闭"),
    RecorderTitle => ("recorder-title", [], "Screen recorder", "屏幕录制"),
    RecorderOpenFrame => ("recorder-open-frame", [], "Open recorder frame", "打开录制框"),
    RecorderStart => ("recorder-start", [], "Start", "开始"),
    RecorderPause => ("recorder-pause", [], "Pause", "暂停"),
    RecorderResume => ("recorder-resume", [], "Resume", "继续"),
    RecorderStop => ("recorder-stop", [], "Stop and save", "停止并保存"),
    RecorderStopShort => ("recorder-stop-short", [], "Stop", "停止"),
    RecorderDiscard => ("recorder-discard", [], "Discard recording", "丢弃录制"),
    RecorderReady => ("recorder-ready", [], "Ready", "准备就绪"),
    RecorderCountdown => ("recorder-countdown", ["seconds"], "Starting in {seconds}…", "{seconds} 秒后开始…"),
    RecorderRecording => ("recorder-recording", [], "Recording", "录制中"),
    RecorderPaused => ("recorder-paused", [], "Paused", "已暂停"),
    RecorderFinalizing => ("recorder-finalizing", [], "Finishing recording…", "正在完成录制…"),
    RecorderFrameCount => ("recorder-frame-count", ["count"], "Frames: {count}", "帧数：{count}"),
    RecorderRegion => ("recorder-region", [], "Recording region", "录制区域"),
    RecorderDuration => ("recorder-duration", [], "Duration", "时长"),
    RecorderFrameRate => ("recorder-frame-rate", [], "Frame rate", "帧率"),
    RecorderWaylandTitle => ("recorder-wayland-title", [], "Wayland screen recorder", "Wayland 屏幕录制"),
    RecorderX11Title => ("recorder-x11-title", [], "X11 screen recorder", "X11 屏幕录制"),
    RecorderLinuxTitle => ("recorder-linux-title", [], "Linux screen recorder", "Linux 屏幕录制"),
    RecorderBackgroundWork => ("recorder-background-work", [], "Capture and durable project creation run on a background worker.", "录制与可恢复工程创建在后台进行。"),
    RecorderGlobalShortcuts => ("recorder-global-shortcuts", [], "Global recorder shortcuts", "全局录制快捷键"),
    RecorderCaptureSource => ("recorder-capture-source", [], "Capture source", "录制来源"),
    RecorderNoCaptureSource => ("recorder-no-capture-source", [], "No capture source", "没有录制来源"),
    RecorderOutputGif => ("recorder-output-gif", [], "Output GIF", "输出 GIF"),
    RecorderMaximumDurationManualStopMs => ("recorder-maximum-duration-manual-stop-ms", [], "Maximum capture duration (ms, 0 = manual stop)", "最长录制时长（ms，0 表示手动停止）"),
    RecorderCountdownSeconds => ("recorder-countdown-seconds", [], "Start countdown (seconds)", "开始倒计时（秒）"),
    RecorderCaptureRegion => ("recorder-capture-region", [], "Capture a region", "录制指定区域"),
    RecorderPhysicalRectangle => ("recorder-physical-rectangle", [], "Use physical-pixel rectangle", "使用物理像素矩形"),
    RecorderWidth => ("recorder-width", [], "Width", "宽度"),
    RecorderHeight => ("recorder-height", [], "Height", "高度"),
    RecorderSelectRegion => ("recorder-select-region", [], "Select region visually", "在预览中选择区域"),
    RecorderWaylandPrivateGeometry => ("recorder-wayland-private-geometry", [], "Wayland keeps source geometry private. The system chooser will open in the background, then the first PipeWire frame will provide a frozen preview.", "Wayland 不公开来源的桌面坐标。系统选择器将在后台打开，随后使用首个 PipeWire 帧生成静态预览。"),
    RecorderSelectRegionTitle => ("recorder-select-region-title", [], "Select capture region", "选择录制区域"),
    RecorderSelectRegionHint => ("recorder-select-region-hint", [], "Drag over the preview, then apply the physical-pixel rectangle.", "在预览上拖出选区，然后应用物理像素矩形。"),
    RecorderNoRegionSelected => ("recorder-no-region-selected", [], "No region selected", "尚未选择区域"),
    ApplyButton => ("apply-button", [], "Apply", "应用"),
    RecorderRegionUpdated => ("recorder-region-updated", [], "Capture region updated from preview.", "已根据预览更新录制区域。"),
    RecorderWaylandPreparation => ("recorder-wayland-preparation", [], "Wayland source preparation", "准备 Wayland 录制来源"),
    RecorderSourceLocalHint => ("recorder-source-local-hint", [], "This is a source-local preview, not a window positioned over global desktop coordinates.", "此预览使用来源内坐标，并非覆盖在桌面全局坐标上的窗口。"),
    RecorderWaylandPreviewUnavailable => ("recorder-wayland-preview-unavailable", [], "Wayland preview is no longer available.", "Wayland 预览已不可用。"),
    RecorderExactRegionOutside => ("recorder-exact-region-outside", [], "The exact region must stay inside the frozen source frame.", "精确选区必须位于静态来源帧内。"),
    RecorderWaylandRegionApplied => ("recorder-wayland-region-applied", [], "Exact Wayland source-local region applied.", "已应用精确的 Wayland 来源内选区。"),
    RecorderPreparationCancellationRequested => ("recorder-preparation-cancellation-requested", [], "Cancellation requested. Closing the source request and its portal session in the background.", "已请求取消，正在后台关闭来源请求及其 Portal 会话。"),
    RecorderOpenSourceController => ("recorder-open-source-controller", [], "Open source-local recorder controller", "打开来源内录制控制器"),
    RecorderCancelPreparation => ("recorder-cancel-preparation", [], "Cancel preparation", "取消准备"),
    RecorderApplyExactRegion => ("recorder-apply-exact-region", [], "Apply exact region", "应用精确选区"),
    RecorderWaylandSourceNotReady => ("recorder-wayland-source-not-ready", [], "The Wayland source is not ready yet.", "Wayland 录制来源尚未准备就绪。"),
    RecorderFrozenPreviewUnavailable => ("recorder-frozen-preview-unavailable", [], "The frozen Wayland preview is no longer available.", "Wayland 静态预览已不可用。"),
    RecorderSourceControllerActive => ("recorder-source-controller-active", [], "Recorder controls are active in this window; other pages are hidden. The preview rectangle controls source-local cropping, not a physical desktop frame.", "录制控件已在此窗口中启用，其他页面暂时隐藏。预览矩形用于调整来源内裁剪，并非桌面上的物理取景框。"),
    RecorderWindowTitle => ("recorder-window-title", [], "GifFromScreen — Recorder", "GifFromScreen — 录制"),
    RecorderCountdownCancelled => ("recorder-countdown-cancelled", [], "Recording countdown cancelled.", "已取消录制倒计时。"),
    RecorderPauseRequested => ("recorder-pause-requested", [], "Pause requested. Wait for the paused state before typing sensitive information.", "已请求暂停。请等状态变为已暂停后再输入敏感信息。"),
    RecorderResumeRequested => ("recorder-resume-requested", [], "Resume requested.", "已请求继续录制。"),
    RecorderSnapshotRequested => ("recorder-snapshot-requested", [], "Manual snapshot requested…", "已请求手动快照…"),
    RecorderStoppingProject => ("recorder-stopping-project", [], "Stopping and finalizing recoverable project…", "正在停止录制并完成可恢复工程…"),
    RecorderDiscarding => ("recorder-discarding", [], "Discarding recording…", "正在丢弃录制…"),
    RecorderClosingAndSaving => ("recorder-closing-and-saving", [], "Closing the recorder: stopping and saving the captured project…", "正在关闭录制器：停止录制并保存已录制的工程…"),
    RecorderRefreshAlreadyRunning => ("recorder-refresh-already-running", [], "Capture-source refresh is already running.", "录制来源已在刷新中。"),
    RecorderRefreshingSources => ("recorder-refreshing-sources", [], "Refreshing Linux capture sources…", "正在刷新 Linux 录制来源…"),
    RecorderFinalizingProject => ("recorder-finalizing-project", [], "Finalizing recoverable project…", "正在完成可恢复工程…"),
    RecorderDiscarded => ("recorder-discarded", [], "Recording discarded.", "已丢弃录制。"),
    RecorderFrozenControllerHint => ("recorder-frozen-controller-hint", [], "Frozen source-local preview — this controller window is not a physical desktop frame.", "来源内静态预览——此控制器窗口并非桌面上的物理取景框。"),
    RecorderMonitorIncludesController => ("recorder-monitor-includes-controller", [], "Monitor capture includes this controller if it overlaps the crop. Move it outside the recorded area before Start, or choose a window source. This app cannot exclude itself from monitor capture.", "录制显示器时，若控制器与裁剪区域重叠，它也会被录入。开始前请将其移出录制区域，或选择窗口来源。本应用无法在显示器录制中自动排除自身。"),
    RecorderKeepWindowVisible => ("recorder-keep-window-visible", [], "Keep the selected window visible. Some applications stop redrawing when fully covered or minimized; new capture timestamps do not guarantee updated source pixels.", "请保持所选窗口可见。部分应用在完全被遮挡或最小化时会停止重绘；新的录制时间戳并不保证来源画面已经更新。"),
    RecorderCancelCountdown => ("recorder-cancel-countdown", [], "Cancel countdown", "取消倒计时"),
    RecorderTakeSnapshot => ("recorder-take-snapshot", [], "Take snapshot", "拍摄快照"),
    RecorderDiscardShort => ("recorder-discard-short", [], "Discard", "丢弃"),
    RecorderPreviewResizeHint => ("recorder-preview-resize-hint", [], "Drag the preview to resize before recording.", "录制前可在预览中拖动以调整尺寸。"),
    RecorderMoveSourceLeft => ("recorder-move-source-left", [], "Move left 10 source pixels", "向左移动 10 个来源像素"),
    RecorderMoveSourceRight => ("recorder-move-source-right", [], "Move right 10 source pixels", "向右移动 10 个来源像素"),
    RecorderMoveSourceUp => ("recorder-move-source-up", [], "Move up 10 source pixels", "向上移动 10 个来源像素"),
    RecorderMoveSourceDown => ("recorder-move-source-down", [], "Move down 10 source pixels", "向下移动 10 个来源像素"),
    RecorderSourceLocalStep => ("recorder-source-local-step", [], "10 px source-local", "来源内移动 10 px"),
    RecorderPreparationIdle => ("recorder-preparation-idle", [], "Wayland source preparation is idle.", "尚未开始准备 Wayland 录制来源。"),
    RecorderPreparationConnecting => ("recorder-preparation-connecting", [], "Connecting to the Wayland ScreenCast portal in the background…", "正在后台连接 Wayland ScreenCast Portal…"),
    RecorderPreparationChoosing => ("recorder-preparation-choosing", [], "Choose a screen or window in the trusted system dialog…", "请在可信的系统对话框中选择显示器或窗口…"),
    RecorderPreparationWaiting => ("recorder-preparation-waiting", [], "The portal selection is ready; waiting for the first mapped PipeWire frame…", "Portal 来源已选定；正在等待首个已映射的 PipeWire 帧…"),
    RecorderPreparationReady => ("recorder-preparation-ready", [], "The frozen preview is ready and its native session is paused.", "静态预览已就绪，原生录制会话已暂停。"),
    RecorderPreparationCancelling => ("recorder-preparation-cancelling", [], "Cancellation requested. If the trusted chooser is still open, close it to finish portal teardown.", "已请求取消。如果可信的系统选择器仍然打开，请关闭它以完成 Portal 会话清理。"),
    RecorderPreparationFinished => ("recorder-preparation-finished", [], "Wayland source preparation finished.", "Wayland 来源准备已结束。"),
    RecorderContinuousFps => ("recorder-continuous-fps", [], "Continuous FPS", "连续录制 FPS"),
    RecorderPeriodicSnapshots => ("recorder-periodic-snapshots", [], "Periodic snapshots", "周期快照"),
    RecorderManualSnapshots => ("recorder-manual-snapshots", [], "Manual snapshots", "手动快照"),
    RecorderInteractionSnapshots => ("recorder-interaction-snapshots", [], "Desktop interaction snapshots (X11)", "桌面交互快照（X11）"),
    RecorderIntervalSeconds => ("recorder-interval-seconds", [], "seconds", "秒"),
    RecorderIntervalMinutes => ("recorder-interval-minutes", [], "minutes", "分钟"),
    RecorderIntervalHours => ("recorder-interval-hours", [], "hours", "小时"),
    RecorderCaptureFrequency => ("recorder-capture-frequency", [], "Capture frequency", "采样频率"),
    RecorderFramesPerSecond => ("recorder-frames-per-second", [], "Frames per second", "每秒帧数"),
    RecorderPeriodicInterval => ("recorder-periodic-interval", [], "Periodic snapshot interval", "周期快照间隔"),
    RecorderInteractionScope => ("recorder-interaction-scope", [], "Interaction scope", "交互范围"),
    RecorderInteractionScopeHint => ("recorder-interaction-scope-hint", [], "Key presses, mouse-button presses and wheel events anywhere on this X11 desktop, including recorder controls. Motion and releases do not trigger snapshots.", "此 X11 桌面任何位置的按键按下、鼠标按钮按下和滚轮事件都会触发快照，包括录制器控件。移动和释放事件不会触发快照。"),
    RecorderInteractionPrivacyHint => ("recorder-interaction-privacy-hint", [], "Only while recording; paused input is discarded. Bursts coalesce while capture is busy. Key values and click coordinates are not saved unless input annotations are enabled separately.", "仅在录制时启用；暂停期间的输入会被丢弃。录制繁忙时会合并突发事件。除非另外启用输入标注，否则不保存键值和点击坐标。"),
    RecorderPlaybackTiming => ("recorder-playback-timing", [], "GIF playback timing", "GIF 播放计时"),
    RecorderFixedPlaybackRate => ("recorder-fixed-playback-rate", [], "Fixed playback rate", "固定播放帧率"),
    RecorderMeasuredTimingHint => ("recorder-measured-timing-hint", [], "Follow the active time between captured samples.", "按照采样帧之间的有效录制时间播放。"),
    RecorderFixedFrameDelay => ("recorder-fixed-frame-delay", [], "Fixed frame delay", "固定每帧延时"),
    RecorderPerFrameMsSuffix => ("recorder-per-frame-ms-suffix", [], " ms per frame", " ms 每帧"),
    RecorderFinalFrameMsSuffix => ("recorder-final-frame-ms-suffix", [], " ms final frame", " ms 末帧"),
    RecorderIndependentTimingHint => ("recorder-independent-timing-hint", [], "Sampling interval and GIF playback speed are independent.", "采样间隔与 GIF 播放速度互不影响。"),
    RecorderPeriodicMeasuredHint => ("recorder-periodic-measured-hint", [], "Follow source timing; the final frame uses the sampling interval.", "按照来源时序播放；末帧使用采样间隔。"),
    RecorderManualFixedHint => ("recorder-manual-fixed-hint", [], "Every snapshot gets this playback delay, regardless of time between clicks.", "每张快照都使用此播放延时，与两次点击的间隔无关。"),
    RecorderManualMeasuredHint => ("recorder-manual-measured-hint", [], "Earlier frames follow active time between clicks; this is only the final frame's delay.", "此前各帧按照点击之间的有效录制时间播放；此值仅用于末帧延时。"),
    RecorderInteractionPlaybackHint => ("recorder-interaction-playback-hint", [], "GIF playback delay is independent of time between input events. Captures have no added trigger delay.", "GIF 播放延时与输入事件之间的间隔无关。采样不添加触发延迟。"),
    RecorderAnnotations => ("recorder-annotations", [], "Cursor and input annotations", "光标与输入标注"),
    RecorderCursorEmbedded => ("recorder-cursor-embedded", [], "Cursor in recording pixels", "将光标录入画面"),
    RecorderCursorHidden => ("recorder-cursor-hidden", [], "Hide cursor", "隐藏光标"),
    RecorderCursorEditable => ("recorder-cursor-editable", [], "Editable cursor metadata", "可编辑光标元数据"),
    RecorderRecordInput => ("recorder-record-input", [], "Record key and mouse-button events (X11)", "记录按键和鼠标按钮事件（X11）"),
    RecorderInputPrivacyWarning => ("recorder-input-privacy-warning", [], "Keys can include passwords or private messages. Enabled only while recording; pause before typing sensitive information.", "按键可能包含密码或私人消息。此功能仅在录制时启用；输入敏感信息前请先暂停。"),
    RecorderWaylandAnnotationsHint => ("recorder-wayland-annotations-hint", [], "Wayland cannot record global key/click events; manual annotations remain available. Cursor mode is chosen before portal preparation.", "Wayland 无法记录全局按键和点击事件；仍可使用手工标注。光标模式需在准备 Portal 来源前选择。"),
    RecorderEditableCursorHint => ("recorder-editable-cursor-hint", [], "Editable mode leaves the cursor out of pixels; add its captured cursor annotation in the editor. Physical key transitions are captured, not IME text or server-generated auto-repeat.", "可编辑模式不会把光标录入画面；可在编辑器中添加已采集的光标标注。记录的是物理按键状态变化，不是输入法文本或服务器生成的自动重复。"),
    RecorderFrameRetention => ("recorder-frame-retention", [], "Frame retention", "帧保留策略"),
    RecorderChangedOnly => ("recorder-changed-only", [], "Store only changed pixels, cursor or input", "仅保留画面、光标或输入发生变化的帧"),
    RecorderManualRetainedHint => ("recorder-manual-retained-hint", [], "Every manual trigger is retained, including identical pixels.", "每次手动触发都会保留一帧，包括画面完全相同的帧。"),
    RecorderSkippedFixedHint => ("recorder-skipped-fixed-hint", [], "Skipped samples add no GIF playback time in fixed-delay mode.", "固定延时模式下，跳过的采样不会增加 GIF 播放时长。"),
    RecorderReadyToRecord => ("recorder-ready-to-record", [], "Ready to record", "准备录制"),
    RecorderWaitGeometry => ("recorder-wait-geometry", [], "Waiting for the recording guide and capture region to be ready.", "正在等待录制边框和录制区域就绪。"),
    RecorderPhysicalRegion => ("recorder-physical-region", [], "Recording region · physical pixels", "录制区域 · 物理像素"),
    RecorderGlobalCoordinates => ("recorder-global-coordinates", [], "Global desktop coordinates; negative monitor positions are supported.", "使用桌面全局坐标；支持位于负坐标的显示器。"),
    RecorderWidthPrefix => ("recorder-width-prefix", [], "Width: ", "宽度："),
    RecorderHeightPrefix => ("recorder-height-prefix", [], "Height: ", "高度："),
    RecorderCanvasPositionOnly => ("recorder-canvas-position-only", [], "Canvas size is locked; only the recording position can change.", "画布尺寸已锁定；只能移动录制位置。"),
    RecorderKeyboardMove => ("recorder-keyboard-move", [], "Move with arrow keys", "使用方向键移动"),
    RecorderKeyboardMoveHint => ("recorder-keyboard-move-hint", [], "Focus this control, then use the arrow keys. Shift moves 10 physical pixels; Escape releases keyboard movement. Text fields keep their own arrow keys.", "先让此控件获得焦点，再使用方向键。按住 Shift 可移动 10 个物理像素；Escape 退出键盘移动。文本框仍使用其自身的方向键操作。"),
    RecorderMoveStepHint => ("recorder-move-step-hint", [], "Move 1 px; hold Shift for 10 px. Position is clamped to the source.", "每次移动 1 px；按住 Shift 移动 10 px。位置不会超出来源范围。"),
    RecorderTiming => ("recorder-timing", [], "Timing", "计时"),
    RecorderMaximumDurationMs => ("recorder-maximum-duration-ms", [], "Maximum capture duration (ms)", "最长录制时长（ms）"),
    RecorderManualStopHint => ("recorder-manual-stop-hint", [], "0 ms means stop manually.", "0 ms 表示手动停止。"),
    RecorderRegionFinalizing => ("recorder-region-finalizing", [], "The recording region cannot change while finalizing", "完成录制期间无法更改录制区域"),
    RecorderCanvasLocked => ("recorder-canvas-locked", [], "Recording canvas size is locked", "录制画布尺寸已锁定"),
    RecorderDimensionsExceed => ("recorder-dimensions-exceed", [], "Recording dimensions exceed the capture source", "录制尺寸超出了来源范围"),
    RecorderInvalidDimensions => ("recorder-invalid-dimensions", [], "Recording dimensions must be nonzero and within the capture source.", "录制尺寸必须大于零且位于来源范围内。"),
    SnapTitle => ("snap-title", [], "Fit region to a window…", "将选区贴合窗口…"),
    SnapWindowFrame => ("snap-window-frame", [], "Window frame", "窗口边框"),
    SnapClientArea => ("snap-client-area", [], "Client area", "客户区"),
    SnapNativeBounds => ("snap-native-bounds", [], "Native bounds", "原生窗口边界"),
    SnapPickWindow => ("snap-pick-window", [], "Pick window on screen…", "在屏幕上选择窗口…"),
    SnapPickHelp => ("snap-pick-help", [], "Drag this button onto a window and release, or click it then pick a window. Right-click or Escape cancels. Controls hide only while selecting; the target must fit within the selected screen.", "按住此按钮拖到窗口上后松开，或先点击按钮再选择窗口。右键或 Escape 可取消。仅在选择期间隐藏控件；目标必须完整位于所选显示器内。"),
    SnapRetryDrag => ("snap-retry-drag", [], "Retry drag handle", "重试拖选按钮"),
    SnapNoWindows => ("snap-no-windows", [], "No windows discovered", "尚未发现窗口"),
    SnapRefresh => ("snap-refresh", [], "Refresh windows", "刷新窗口"),
    SnapApply => ("snap-apply", [], "Snap region", "贴合选区"),
    SnapCancelOperation => ("snap-cancel-operation", [], "Cancel window operation", "取消窗口操作"),
    SnapBoundsHelp => ("snap-bounds-help", [], "Window frame uses validated WM borders or client-side shadow hints. Native bounds may include invisible margins. Refresh discovers new or renamed windows without closing this controller.", "窗口边框使用经过验证的窗口管理器边界或客户端阴影提示。原生窗口边界可能包含不可见边距。刷新可发现新开或改名的窗口，无需关闭此控制器。"),
    SnapDragUnavailable => ("snap-drag-unavailable", [], "Drag unavailable; click-to-pick available.", "拖选不可用；仍可点击选窗。"),
    SnapDragReady => ("snap-drag-ready", [], "Drag ready; or click to pick.", "拖选已就绪；也可点击选窗。"),
    SnapDragPreparing => ("snap-drag-preparing", [], "Preparing drag; click-to-pick available.", "正在准备拖选；仍可点击选窗。"),
    SnapCancelledCleanup => ("snap-cancelled-cleanup", [], "Window selection cancelled; waiting for native cleanup.", "已取消选窗；正在等待原生资源清理。"),
    SnapLayoutChanged => ("snap-layout-changed", [], "Window selection cancelled because its initiating layout changed.", "发起选窗时的界面布局已改变，因此已取消选窗。"),
    SnapTargetChanged => ("snap-target-changed", [], "Window selection cancelled because its original layout or recording target changed.", "原界面布局或录制目标已改变，因此已取消选窗。"),
    SnapStaleResult => ("snap-stale-result", [], "Discarded a stale native window selection; current region kept.", "已丢弃过期的原生选窗结果；当前选区保持不变。"),
    SnapSelectionChanged => ("snap-selection-changed", [], "Window snap cancelled because the recording selection or stage changed.", "录制选区或阶段已改变，因此已取消窗口贴合。"),
    SnapCancelled => ("snap-cancelled", [], "Window selection cancelled; original region kept.", "已取消选窗；保留原选区。"),
    SnapCancelledUnchanged => ("snap-cancelled-unchanged", [], "Window snap cancelled; selection unchanged.", "已取消窗口贴合；选区保持不变。"),
    SnapSelected => ("snap-selected", [], "Selected window; recording region updated. This is one-time positioning, not window tracking.", "已选择窗口并更新录制区域。此操作仅定位一次，不会跟踪窗口。"),
    SnapPositioned => ("snap-positioned", [], "Snapped to the current window bounds. This is a one-time position, not window tracking.", "已贴合窗口当前边界。此操作仅定位一次，不会跟踪窗口。"),
    SnapPreparingClick => ("snap-preparing-click", [], "Preparing click-to-pick; waiting for the native drag handle to close.", "正在准备点击选窗；等待原生拖选按钮关闭。"),
    SnapReading => ("snap-reading", [], "Reading windows…", "正在读取窗口…"),
    SnapAlreadyFinishing => ("snap-already-finishing", [], "A window selection is already finishing.", "已有选窗操作正在结束。"),
    SnapStillFinishing => ("snap-still-finishing", [], "A window snap is still finishing.", "窗口贴合操作仍在结束中。"),
    SnapWorkerStopped => ("snap-worker-stopped", [], "Window snap worker stopped without a result.", "窗口贴合后台任务已停止，但未返回结果。"),
    SnapGenerationsExhausted => ("snap-generations-exhausted", [], "Drag-handle generations exhausted; reopen the recorder.", "拖选按钮的代次编号已耗尽；请重新打开录制器。"),
    SnapInvalidScale => ("snap-invalid-scale", [], "Invalid UI pixel scale.", "界面像素缩放比例无效。"),
    SnapHitLimit => ("snap-hit-limit", [], "Visible drag button exceeds the native input-child coordinate limit.", "可见拖选按钮超出了原生输入子窗口的坐标限制。"),
    RecorderBorderDragHint => ("recorder-border-drag-hint", [], "Drag a border to move the region; drag a corner to resize before recording. Controls are independent of the selected pixels.", "拖动边框可移动区域；录制前可拖动角点调整尺寸。控件与所选像素区域相互独立。"),
    RecorderX11WindowTitle => ("recorder-x11-window-title", [], "GifFromScreen recorder", "GifFromScreen 录制器"),
    RecorderPlacement => ("recorder-placement", [], "Recorder placement", "录制器位置"),
    RecorderPlacementUnsafe => ("recorder-placement-unsafe", [], "The window manager did not confirm a safe control position.", "窗口管理器未确认控件已位于安全位置。"),
    RecorderRetryPlacement => ("recorder-retry-placement", [], "Retry placement", "重试定位"),
    RecorderClose => ("recorder-close", [], "Close recorder", "关闭录制器"),
    RecorderRecoveringControls => ("recorder-recovering-controls", [], "Capture is pausing before showing the recovered controls. Resume hides them again when no safe screen space remains.", "正在暂停录制，然后再显示恢复的控件。若屏幕上没有安全空位，继续录制时会再次隐藏控件。"),
    RecorderUpdatingPosition => ("recorder-updating-position", [], "Updating the recording position while sampling is paused; waiting for native acknowledgements.", "正在暂停采样时更新录制位置；等待原生确认。"),
    RecorderNoControlSpace => ("recorder-no-control-space", [], "No space remains outside the selection. Start will hide/minimize these controls. Restore this window to pause and recover them; global shortcuts and timed stop remain available.", "选区外已没有空位。开始时将隐藏或最小化这些控件。还原此窗口可暂停录制并恢复控件；仍可使用全局快捷键和定时停止。"),
    RecorderStarting => ("recorder-starting", [], "Starting…", "正在开始…"),
    RecorderProgressSummary => ("recorder-progress-summary", ["frames", "seconds"], "{frames} frames · GIF {seconds}s", "{frames} 帧 · GIF {seconds}s"),
    RecorderSourceSpanHint => ("recorder-source-span-hint", ["seconds"], "Source sample span: {seconds}s. Playback timing does not change the captured input clock.", "来源采样跨度：{seconds}s。播放计时不会改变已采集输入的时钟。"),
    SnapFound => ("snap-found", ["count"], "Found {count} windows. Selection geometry is unchanged.", "发现 {count} 个窗口。选区几何保持不变。"),
    SnapFoundIncomplete => ("snap-found-incomplete", ["count"], "Found {count} windows. Selection geometry is unchanged. The discovery limit was reached; the list is incomplete.", "发现 {count} 个窗口。选区几何保持不变。已达到发现数量上限；列表并不完整。"),
    SnapSelectedDoesNotFit => ("snap-selected-does-not-fit", ["error"], "Selected window does not fit; original region kept: {error}", "所选窗口无法完整放入；保留原选区：{error}"),
    SnapFailed => ("snap-failed", ["error"], "Window snap failed; original selection kept: {error}", "窗口贴合失败；保留原选区：{error}"),
    SnapOperationFailed => ("snap-operation-failed", ["error"], "Window operation failed; selection and previous list kept: {error}", "窗口操作失败；保留选区和原有列表：{error}"),
    SnapDragError => ("snap-drag-error", ["error"], "Native drag handle unavailable: {error} Click-to-pick is still available; use Retry drag handle to retry.", "原生拖选按钮不可用：{error} 仍可点击选窗；可点击“重试拖选按钮”重试。"),
    SnapStartFailed => ("snap-start-failed", ["error"], "Could not start window snap: {error}", "无法开始窗口贴合：{error}"),
    RecorderWorkflowProgress => ("recorder-workflow-progress", ["phase", "count", "source_seconds", "playback_seconds"], "{phase}: {count} captured frames · source span {source_seconds}s · GIF {playback_seconds}s", "{phase}：已录制 {count} 帧 · 来源跨度 {source_seconds}s · GIF {playback_seconds}s"),
    RecorderSelectedSource => ("recorder-selected-source", ["width", "height", "x", "y", "kind"], "Selected source: {width}×{height} at {x},{y} ({kind})", "所选来源：{width}×{height}，位置 {x},{y}（{kind}）"),
    RecorderRegionSummary => ("recorder-region-summary", ["width", "height", "x", "y"], "{width}×{height} at {x},{y}", "{width}×{height}，位置 {x},{y}"),
    RecorderSelectedRegion => ("recorder-selected-region", ["width", "height", "x", "y"], "Selected {width}×{height} at {x},{y}", "已选择 {width}×{height}，位置 {x},{y}"),
    RecorderFrozenFrame => ("recorder-frozen-frame", ["width", "height"], "Frozen {width}×{height} PipeWire frame", "静态 {width}×{height} PipeWire 帧"),
    RecorderCropSummary => ("recorder-crop-summary", ["width", "height", "x", "y"], "Crop {width}×{height} at {x},{y}", "裁剪 {width}×{height}，位置 {x},{y}"),
    RecorderStartsIn => ("recorder-starts-in", ["seconds"], "Recording starts in {seconds}s", "{seconds}s 后开始录制"),
    RecorderFixedDelayHint => ("recorder-fixed-delay-hint", ["milliseconds"], "{milliseconds} ms per retained frame, independent of capture delays.", "每个保留帧播放 {milliseconds} ms，与采样延迟无关。"),
    RecorderProgressTimingHint => ("recorder-progress-timing-hint", ["source_seconds", "playback_seconds"], "Source sample span: {source_seconds}s. GIF playback duration: {playback_seconds}s. Fixed playback delay does not change the captured input clock.", "来源采样跨度：{source_seconds}s。GIF 播放时长：{playback_seconds}s。固定播放延时不会改变已采集输入的时钟。"),
    RecorderDiscardCleanupFailed => ("recorder-discard-cleanup-failed", ["error"], "Recording was discarded, but {error}", "录制已丢弃，但{error}"),
    RecorderProjectReady => ("recorder-project-ready", ["frames", "seconds", "path"], "Project ready: {frames} frames, {seconds}s at {path}", "工程已就绪：{frames} 帧，{seconds}s，位置 {path}"),
    RecorderPhaseStartingCapture => ("recorder-phase-starting-capture", [], "StartingCapture", "开始录制"),
    RecorderPhaseCapturing => ("recorder-phase-capturing", [], "Capturing", "录制中"),
    RecorderPhaseStoppingCapture => ("recorder-phase-stopping-capture", [], "StoppingCapture", "正在停止录制"),
    RecorderPhaseEncoding => ("recorder-phase-encoding", [], "Encoding", "编码中"),
    RecorderPhaseCommitting => ("recorder-phase-committing", [], "Committing", "正在写入"),
    RecorderPhaseComplete => ("recorder-phase-complete", [], "Complete", "已完成"),
    RecorderSourceMonitor => ("recorder-source-monitor", [], "Monitor", "显示器"),
    RecorderSourceWindow => ("recorder-source-window", [], "Window", "窗口"),
    RecorderUnknown => ("recorder-unknown", [], "Unknown", "未知"),
    RecorderCountdownNotice => ("recorder-countdown-notice", ["seconds"], "Recording starts in {seconds} seconds…", "录制将在 {seconds} 秒后开始…"),
    RecorderCountdownRange => ("recorder-countdown-range", ["maximum"], "Countdown must be between 0 and {maximum} seconds.", "倒计时必须在 0 到 {maximum} 秒之间。"),
    RecorderPreparedNotice => ("recorder-prepared-notice", ["width", "height"], "Wayland source prepared at {width}×{height} pixels. The native session is paused and retained by its worker.", "Wayland 来源已准备就绪，尺寸为 {width}×{height} 像素。原生会话已暂停，并由后台任务保留。"),
    RecorderPreparationCancelled => ("recorder-preparation-cancelled", [], "Wayland source preparation cancelled and its portal session closed.", "已取消 Wayland 来源准备，并关闭其 Portal 会话。"),
    RecorderPreparationFailed => ("recorder-preparation-failed", ["error"], "Could not prepare Wayland source: {error}", "无法准备 Wayland 来源：{error}"),
    RecorderPreparationNoResult => ("recorder-preparation-no-result", [], "Wayland preparation ended without a result.", "Wayland 来源准备已结束，但未返回结果。"),
    RecorderSourcesFound => ("recorder-sources-found", ["count", "display_server"], "Found {count} {display_server} capture source option(s).", "找到 {count} 个 {display_server} 录制来源选项。"),
    RecorderSourcesNoResult => ("recorder-sources-no-result", [], "Capture-source worker returned no result.", "录制来源后台任务未返回结果。"),
    RecorderSourcesFailed => ("recorder-sources-failed", ["error"], "Could not load Linux capture sources: {error}", "无法加载 Linux 录制来源：{error}"),
    RecorderStarted => ("recorder-started", [], "Recording started…", "录制已开始…"),
    RecorderWaylandOpeningChooser => ("recorder-wayland-opening-chooser", [], "Opening the Wayland system chooser in the background. Select a screen or window to prepare its frozen preview.", "正在后台打开 Wayland 系统选择器。请选择显示器或窗口以准备其静态预览。"),
    RecorderInteractionRequiresX11 => ("recorder-interaction-requires-x11", [], "Interaction snapshots require X11. Choose continuous, periodic or manual capture on Wayland.", "交互快照需要 X11。在 Wayland 上请选择连续录制、周期快照或手动快照。"),
    RecorderWaylandInputGuard => ("recorder-wayland-input-guard", [], "Choose a hidden/embedded cursor and disable X11 input events before opening the Wayland source chooser.", "打开 Wayland 来源选择器前，请选择隐藏光标或将光标录入画面，并关闭 X11 输入事件采集。"),
    RecorderNoWaylandSource => ("recorder-no-wayland-source", [], "No Wayland portal source is selected.", "尚未选择 Wayland Portal 来源。"),
    RecorderWaitInputHole => ("recorder-wait-input-hole", [], "Wait for the current input hole and keep the recorder inside its source before starting.", "请等待当前输入穿透区域就绪，并确保录制框位于来源范围内，然后再开始。"),
    RecorderNoX11Source => ("recorder-no-x11-source", [], "No X11 capture source is selected.", "尚未选择 X11 录制来源。"),
    RecorderSourcesLoading => ("recorder-sources-loading", [], "Loading Linux capture sources…", "正在加载 Linux 录制来源…"),
    RecorderWaylandStarted => ("recorder-wayland-started", [], "Wayland recording started from the prepared session…", "已使用准备好的会话开始 Wayland 录制…"),
    ExportTitle => ("export-title", [], "Export GIF", "导出 GIF"),
    ExportDestination => ("export-destination", [], "Output path", "输出路径"),
    ExportStart => ("export-start", [], "Export", "导出"),
    ExportWorking => ("export-working", [], "Exporting GIF…", "正在导出 GIF…"),
    ExportFinished => ("export-finished", ["frames", "path"], "Export complete. Frames: {frames}. File: {path}", "导出完成。帧数：{frames}。文件：{path}"),
    ExportFailed => ("export-failed", ["reason"], "GIF export failed: {reason}", "GIF 导出失败：{reason}"),
    ExportCancel => ("export-cancel", [], "Cancel export", "取消导出"),
    ExportAllFrames => ("export-all-frames", [], "All frames", "全部帧"),
    ExportSelectedFrames => ("export-selected-frames", [], "Selected frames", "选中的帧"),
}

/// Where the text of an individual resolved message came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogSource {
    /// The requested language provides this initial message set.
    RequestedCatalog,
    /// No requested-language catalog is present; the English message is used.
    EnglishFallback,
}

/// A static message/template with explicit provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalizedText {
    /// Message text, or its named-argument template.
    pub text: &'static str,
    /// Actual catalog language, distinct from the user's requested language.
    pub language_tag: &'static str,
    /// Whether the requested catalog or English fallback supplied this message.
    pub source: CatalogSource,
}

/// The denominator of a reported translation coverage count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogScope {
    /// Only the explicit initial UI messages in [`ALL_MESSAGES`].
    InitialUiSlice,
}

/// Coverage of this specific message set, never of the entire application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogCoverage {
    /// Number of messages supplied by the requested language, excluding fallback.
    pub translated: usize,
    /// Number of messages in this initial catalog contract.
    pub total: usize,
    /// Explicit coverage scope.
    pub scope: CatalogScope,
    /// False: many application messages and text-rendering gates remain outside this slice.
    pub entire_ui_covered: bool,
}

/// Report requested-language coverage without counting English fallback as translated.
pub fn catalog_coverage(language: &Language) -> CatalogCoverage {
    CatalogCoverage {
        translated: ALL_MESSAGES
            .iter()
            .filter(|message| message.entry().translation(language.tag).is_some())
            .count(),
        total: ALL_MESSAGES.len(),
        scope: CatalogScope::InitialUiSlice,
        entire_ui_covered: false,
    }
}

/// Immutable per-language message lookup; no process-wide locale or state changes.
#[derive(Clone, Copy, Debug)]
pub struct Localizer {
    requested: &'static Language,
}

impl Localizer {
    /// Select a requested language identity. Missing catalogs fall back explicitly.
    pub const fn new(language: &'static Language) -> Self {
        Self {
            requested: language,
        }
    }

    /// Original requested identity, preserved even without a catalog.
    pub const fn requested_language(self) -> &'static Language {
        self.requested
    }

    /// Resolve text and its provenance. Dynamic messages retain their placeholders.
    pub fn resolve(self, message: Message) -> LocalizedText {
        message.entry().resolve(self.requested.tag)
    }

    /// Return static text. For keys with [`Message::parameters`], use [`Self::format`].
    pub fn text(self, message: Message) -> &'static str {
        self.resolve(message).text
    }

    /// Format a message with an exact set of named string arguments.
    ///
    /// Values are appended literally, including braces, paths and Unicode. This
    /// layer does not normalize user data or choose numerical parsing rules.
    ///
    /// # Errors
    /// Rejects missing, duplicate or unknown names, invalid templates and output
    /// exceeding [`MAX_FORMATTED_MESSAGE_BYTES`], without partially returning text.
    pub fn format(
        self,
        message: Message,
        arguments: &[(&str, &str)],
    ) -> Result<String, FormatError> {
        validate_arguments(message.parameters(), arguments)?;
        format_template(self.text(message), arguments)
    }

    /// Requested-catalog coverage; fallback and unmigrated UI are not translated coverage.
    pub fn coverage(self) -> CatalogCoverage {
        catalog_coverage(self.requested)
    }
}

/// A caller/template contract failure; formatting never falls back to corrupt output.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum FormatError {
    /// An argument required by the typed key was omitted.
    #[error("Missing message argument: {0}")]
    MissingArgument(&'static str),
    /// A required name appeared more than once.
    #[error("Duplicate message argument: {0}")]
    DuplicateArgument(&'static str),
    /// A caller supplied a name outside this message's contract.
    #[error("Unknown message argument")]
    UnknownArgument,
    /// The embedded template has malformed/unknown placeholders.
    #[error("Invalid embedded message template")]
    InvalidTemplate,
    /// The complete message would exceed its bounded output budget.
    #[error("Formatted message exceeds its byte limit")]
    OutputTooLarge,
}

fn validate_arguments(
    expected: &'static [&'static str],
    arguments: &[(&str, &str)],
) -> Result<(), FormatError> {
    for (index, (name, _)) in arguments.iter().enumerate() {
        let Some(&canonical) = expected.iter().find(|&&expected| expected == *name) else {
            return Err(FormatError::UnknownArgument);
        };
        if arguments[..index]
            .iter()
            .any(|(previous, _)| previous == name)
        {
            return Err(FormatError::DuplicateArgument(canonical));
        }
    }
    for &name in expected {
        if !arguments.iter().any(|(provided, _)| *provided == name) {
            return Err(FormatError::MissingArgument(name));
        }
    }
    Ok(())
}

fn append(output: &mut String, value: &str) -> Result<(), FormatError> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|size| size > MAX_FORMATTED_MESSAGE_BYTES)
    {
        return Err(FormatError::OutputTooLarge);
    }
    output.push_str(value);
    Ok(())
}

fn format_template(mut template: &str, arguments: &[(&str, &str)]) -> Result<String, FormatError> {
    let mut output = String::new();
    while let Some(index) = template.find(['{', '}']) {
        append(&mut output, &template[..index])?;
        template = &template[index..];
        if template.starts_with("{{") || template.starts_with("}}") {
            append(&mut output, &template[..1])?;
            template = &template[2..];
            continue;
        }
        if !template.starts_with('{') {
            return Err(FormatError::InvalidTemplate);
        }
        let end = template.find('}').ok_or(FormatError::InvalidTemplate)?;
        let name = &template[1..end];
        let value = arguments
            .iter()
            .find_map(|(provided, value)| (*provided == name).then_some(*value))
            .ok_or(FormatError::InvalidTemplate)?;
        append(&mut output, value)?;
        template = &template[end + 1..];
    }
    append(&mut output, template)?;
    Ok(output)
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
