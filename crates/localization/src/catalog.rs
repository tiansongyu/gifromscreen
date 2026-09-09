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
    RecorderChoosePortalScreen => ("recorder-choose-portal-screen", [], "Choose a screen with the system portal", "通过系统 Portal 选择显示器"),
    RecorderChoosePortalWindow => ("recorder-choose-portal-window", [], "Choose a window with the system portal", "通过系统 Portal 选择窗口"),
    ShortcutsHelp => ("shortcuts-help", [], "Optional recorder controls, not keystroke recording. No shortcut is registered on the launcher or outside recorder scope.", "这是可选的录制控制，不是按键记录。启动页及录制范围之外不会注册快捷键。"),
    ShortcutsPrepareHint => ("shortcuts-prepare-hint", [], "Open/prepare the recorder first; press again to start once the frame/controller is ready.", "首次按键用于打开或准备录制器；录制框或控制器就绪后，再按一次开始录制。"),
    ShortcutsEnable => ("shortcuts-enable", [], "Enable shortcuts", "启用快捷键"),
    ShortcutsDisable => ("shortcuts-disable", [], "Disable shortcuts", "关闭快捷键"),
    ShortcutsCancelRegistration => ("shortcuts-cancel-registration", [], "Cancel registration", "取消注册"),
    ShortcutsRetryRegistration => ("shortcuts-retry-registration", [], "Retry registration", "重试注册"),
    ShortcutsApplyBindings => ("shortcuts-apply-bindings", [], "Apply bindings", "应用按键设置"),
    ShortcutsEditsPending => ("shortcuts-edits-pending", [], "Edits are not active yet. Apply waits for the previous registration to close before binding the new keys.", "更改尚未生效。应用时会等待原有注册关闭，然后再绑定新按键。"),
    ShortcutsRetryLoading => ("shortcuts-retry-loading", [], "Retry loading shortcut settings", "重试读取快捷键设置"),
    ShortcutsButtonsFallback => ("shortcuts-buttons-fallback", [], "If registration is unavailable, denied, conflicting or incomplete, use the recorder's visible buttons and timed stop. Discard has no global shortcut.", "若注册不可用、被拒绝、发生冲突或未完整注册，请使用录制器的可见按钮和定时停止。丢弃操作没有全局快捷键。"),
    ShortcutsDisabled => ("shortcuts-disabled", [], "Shortcuts are disabled.", "快捷键已关闭。"),
    ShortcutsScopePending => ("shortcuts-scope-pending", [], "Enabled preference; registration starts only in recorder scope.", "启用选项已打开；仅在录制范围内开始注册。"),
    ShortcutsNoDisplay => ("shortcuts-no-display", [], "No supported Linux display was detected; recorder buttons remain available.", "未检测到受支持的 Linux 显示环境；仍可使用录制器按钮。"),
    ShortcutsRegistering => ("shortcuts-registering", [], "Registering… Complete or cancel the system permission dialog.", "正在注册…请在系统权限对话框中完成授权或取消。"),
    ShortcutsStopping => ("shortcuts-stopping", [], "Stopping the previous registration… Its actions are already ignored. Use recorder buttons meanwhile.", "正在停止原有注册…其动作已被忽略。期间请使用录制器按钮。"),
    ShortcutsInactive => ("shortcuts-inactive", [], "No shortcut registration is active.", "当前没有生效的快捷键注册。"),
    ShortcutsRegistrationFailed => ("shortcuts-registration-failed", ["error"], "Registration failed: {error}", "注册失败：{error}"),
    ShortcutsActuallyRegistered => ("shortcuts-actually-registered", [], "Actually registered by the desktop:", "桌面实际注册的快捷键："),
    ShortcutsRegisteredBinding => ("shortcuts-registered-binding", ["action", "trigger"], "{action} — {trigger}", "{action} — {trigger}"),
    ShortcutsMissingBinding => ("shortcuts-missing-binding", ["action"], "{action} was not registered. Use its recorder button.", "{action}未注册。请使用录制器中的对应按钮。"),
    ShortcutsActionStartPause => ("shortcuts-action-start-pause", [], "Start / pause / resume", "开始 / 暂停 / 继续"),
    ShortcutsActionSnapshot => ("shortcuts-action-snapshot", [], "Manual snapshot", "手动快照"),
    ShortcutsSummaryRegistered => ("shortcuts-summary-registered", ["count"], "Shortcuts: {count}/3 registered", "快捷键：已注册 {count}/3"),
    ShortcutsSummaryPending => ("shortcuts-summary-pending", [], "Shortcuts: registration pending; use recorder buttons", "快捷键：等待注册；请使用录制器按钮"),
    ShortcutsSummaryStopping => ("shortcuts-summary-stopping", [], "Shortcuts: stopping old registration; use buttons", "快捷键：正在停止原有注册；请使用按钮"),
    ShortcutsSummaryInactive => ("shortcuts-summary-inactive", [], "Shortcuts: inactive; use recorder buttons", "快捷键：未生效；请使用录制器按钮"),
    ShortcutsSummaryFailed => ("shortcuts-summary-failed", ["error"], "Shortcuts unavailable: {error}", "快捷键不可用：{error}"),
    ShortcutsQueueOverflow => ("shortcuts-queue-overflow", ["count"], "{count} shortcut presses exceeded the bounded queue. Recorder buttons remain available.", "有 {count} 次快捷键按下超出了队列容量。仍可使用录制器按钮。"),
    ShortcutsInvalidBindings => ("shortcuts-invalid-bindings", ["error"], "Invalid shortcut bindings: {error}", "快捷键设置无效：{error}"),
    ShortcutsConfigureAll => ("shortcuts-configure-all", [], "Configure all three recorder actions.", "请配置全部三个录制动作。"),
    ShortcutsSettingsUnavailable => ("shortcuts-settings-unavailable", ["error"], "Shortcut settings are unavailable: {error}", "快捷键设置不可用：{error}"),
    ShortcutsSettingsIoFailed => ("shortcuts-settings-io-failed", ["error"], "Shortcut settings were not saved or loaded: {error}", "快捷键设置未能保存或读取：{error}"),
    ShortcutsSettingsLoadStartFailed => ("shortcuts-settings-load-start-failed", ["error"], "Could not start loading shortcut settings: {error}", "无法开始读取快捷键设置：{error}"),
    ShortcutsSettingsSaveStartFailed => ("shortcuts-settings-save-start-failed", ["error"], "Could not start saving shortcut settings: {error}", "无法开始保存快捷键设置：{error}"),
    ShortcutsSettingsNewerEditsKept => ("shortcuts-settings-newer-edits-kept", [], "Saved shortcuts were loaded, but your newer edits were kept.", "已读取保存的快捷键，但保留了您刚刚作出的更改。"),
    ShortcutsSettingsSaved => ("shortcuts-settings-saved", [], "Shortcut settings saved.", "快捷键设置已保存。"),
    ShortcutsSettingsSaving => ("shortcuts-settings-saving", [], "Saving shortcut settings…", "正在保存快捷键设置…"),
    ShortcutsInvalidTrigger => ("shortcuts-invalid-trigger", [], "Use F1–F24, or an uppercase letter/digit with Control, Alt or Super.", "请使用 F1–F24，或搭配 Control、Alt 或 Super 的大写字母或数字。"),
    ShortcutsUniqueBindings => ("shortcuts-unique-bindings", [], "Each recorder action and shortcut trigger must be unique.", "每个录制动作和快捷键组合都必须唯一。"),
    EditorTitle => ("editor-title", [], "Editor", "编辑器"),
    EditorFramesTab => ("editor-frames-tab", [], "Frames", "帧"),
    EditorTimingTab => ("editor-timing-tab", [], "Timing", "计时"),
    EditorTransformTab => ("editor-transform-tab", [], "Transform", "变换"),
    EditorEffectsTab => ("editor-effects-tab", [], "Effects", "效果"),
    EditorOverlaysTab => ("editor-overlays-tab", [], "Overlays", "叠加图层"),
    EditorProjectTab => ("editor-project-tab", [], "Project", "工程"),
    EditorFrameCount => ("editor-frame-count", ["count"], "{count} frames", "{count} 帧"),
    EditorSelectedCount => ("editor-selected-count", ["count"], "{count} selected", "已选择 {count} 帧"),
    EditorClipboardSummary => ("editor-clipboard-summary", ["frames", "snapshots"], "Clipboard: {frames} frame(s) in {snapshots} snapshot(s)", "剪贴板：{snapshots} 份快照，共 {frames} 帧"),
    EditorCheckpointPending => ("editor-checkpoint-pending", [], "Journaled · checkpoint pending", "已写入日志 · 等待检查点"),
    EditorAssetIssues => ("editor-asset-issues", ["count"], "{count} asset issue(s)", "{count} 项素材问题"),
    EditorNavigate => ("editor-navigate", [], "Navigate", "导航"),
    EditorFirst => ("editor-first", [], "First", "首帧"),
    EditorPrevious => ("editor-previous", [], "Previous", "上一帧"),
    EditorPlay => ("editor-play", [], "Play", "播放"),
    EditorPause => ("editor-pause", [], "Pause", "暂停"),
    EditorNext => ("editor-next", [], "Next", "下一帧"),
    EditorLast => ("editor-last", [], "Last", "末帧"),
    EditorLoopPreview => ("editor-loop-preview", [], "Loop preview", "循环预览"),
    EditorLoopPreviewHint => ("editor-loop-preview-hint", [], "Repeat editor playback. GIF export repetition is configured separately.", "循环播放编辑器预览。GIF 导出的循环次数需单独设置。"),
    EditorFrame => ("editor-frame", [], "Frame", "帧"),
    EditorGo => ("editor-go", [], "Go", "跳转"),
    EditorTimeMs => ("editor-time-ms", [], "Time ms", "时间 ms"),
    EditorGoToTime => ("editor-go-to-time", [], "Go to time", "跳转到时间"),
    EditorUndo => ("editor-undo", [], "Undo", "撤销"),
    EditorRedo => ("editor-redo", [], "Redo", "重做"),
    EditorSelect => ("editor-select", [], "Select", "选择"),
    EditorSelectAll => ("editor-select-all", [], "All", "全选"),
    EditorInvertSelection => ("editor-invert-selection", [], "Invert", "反选"),
    EditorClearSelection => ("editor-clear-selection", [], "Clear", "清除"),
    EditorExpression => ("editor-expression", [], "Expression", "表达式"),
    EditorApplySelection => ("editor-apply-selection", [], "Apply selection", "应用选择"),
    EditorCut => ("editor-cut", [], "Cut", "剪切"),
    EditorCopy => ("editor-copy", [], "Copy", "复制"),
    EditorPaste => ("editor-paste", [], "Paste", "粘贴"),
    EditorDelete => ("editor-delete", [], "Delete", "删除"),
    EditorDeleteBefore => ("editor-delete-before", [], "Delete before", "删除之前的帧"),
    EditorDeleteAfter => ("editor-delete-after", [], "Delete after", "删除之后的帧"),
    EditorMoveLeft => ("editor-move-left", [], "Move left", "左移"),
    EditorMoveRight => ("editor-move-right", [], "Move right", "右移"),
    EditorReverse => ("editor-reverse", [], "Reverse", "倒序"),
    EditorClipboardHistory => ("editor-clipboard-history", ["count"], "Clipboard history ({count})", "剪贴板历史（{count}）"),
    EditorClipboardEmpty => ("editor-clipboard-empty", [], "Copy or cut frames to create a session-local snapshot.", "复制或剪切帧以创建当前会话的快照。"),
    EditorDurationOverflow => ("editor-duration-overflow", [], "duration overflow", "时长溢出"),
    EditorClipboardEntry => ("editor-clipboard-entry", ["id", "frames", "duration"], "#{id} · {frames} frame(s) · {duration}", "#{id} · {frames} 帧 · {duration}"),
    EditorUseClipboardSnapshot => ("editor-use-clipboard-snapshot", [], "Use this snapshot for Paste", "使用此快照粘贴"),
    EditorRemoveClipboardEntry => ("editor-remove-clipboard-entry", [], "Remove", "移除"),
    EditorClearClipboardHistory => ("editor-clear-clipboard-history", [], "Clear clipboard history", "清空剪贴板历史"),
    EditorClipboardSelectChanged => ("editor-clipboard-select-changed", [], "Clipboard history changed before the entry could be selected.", "选择条目前剪贴板历史已发生变化。"),
    EditorClipboardRemoveChanged => ("editor-clipboard-remove-changed", [], "Clipboard history changed before the entry could be removed.", "移除条目前剪贴板历史已发生变化。"),
    EditorClipboardHistoryCleared => ("editor-clipboard-history-cleared", [], "Clipboard history cleared.", "剪贴板历史已清空。"),
    EditorFilmstripFrame => ("editor-filmstrip-frame", ["number", "duration"], "Frame {number} · {duration} µs", "第 {number} 帧 · {duration} µs"),
    EditorFrameNumber => ("editor-frame-number", ["number"], "Frame {number}", "第 {number} 帧"),
    EditorThumbnailUnavailable => ("editor-thumbnail-unavailable", [], "Unavailable", "不可用"),
    EditorThumbnailLoading => ("editor-thumbnail-loading", [], "Loading…", "正在加载…"),
    EditorTimeRange => ("editor-time-range", [], "Time range [start, end) ms", "时间范围 [开始, 结束) ms"),
    EditorRangeStart => ("editor-range-start", [], "Start", "开始"),
    EditorRangeEnd => ("editor-range-end", [], "End", "结束"),
    EditorSelectRange => ("editor-select-range", [], "Select range", "选择范围"),
    EditorKeepRange => ("editor-keep-range", [], "Keep range", "保留范围"),
    EditorDeleteRange => ("editor-delete-range", [], "Delete range", "删除范围"),
    EditorDelayUs => ("editor-delay-us", [], "Delay µs", "延时 µs"),
    EditorOverrideDelay => ("editor-override-delay", [], "Override", "覆盖"),
    EditorAdjustSignedDelay => ("editor-adjust-signed-delay", [], "Adjust signed", "增减延时"),
    EditorPercent => ("editor-percent", [], "Percent", "百分比"),
    EditorScaleDelay => ("editor-scale-delay", [], "Scale", "缩放"),
    EditorAdvancedTiming => ("editor-advanced-timing", [], "Advanced timing", "高级计时"),
    EditorReduceKeepEvery => ("editor-reduce-keep-every", [], "Reduce: keep every", "降帧：保留间隔"),
    EditorReduceFrames => ("editor-reduce-frames", [], "Reduce frames", "降帧"),
    EditorShortenTiming => ("editor-shorten-timing", [], "Shorten timing", "缩短时长"),
    EditorDelayToPrevious => ("editor-delay-to-previous", [], "Add delay to previous", "将延时加到前一帧"),
    EditorDistributeDelay => ("editor-distribute-delay", [], "Distribute delay evenly", "平均分配延时"),
    EditorYoyoSource => ("editor-yoyo-source", [], "Yoyo source", "往返动画来源"),
    EditorSelectionScope => ("editor-selection-scope", [], "Selection", "选中帧"),
    EditorEntireTimeline => ("editor-entire-timeline", [], "Entire timeline", "整个时间线"),
    EditorRepeatEndpoints => ("editor-repeat-endpoints", [], "Repeat endpoints", "重复首尾帧"),
    EditorCreateYoyo => ("editor-create-yoyo", [], "Create Yoyo", "创建往返动画"),
    EditorRenderedDuplicates => ("editor-rendered-duplicates", [], "Rendered duplicates ≥", "渲染后重复度 ≥"),
    EditorKeepFirstDuplicate => ("editor-keep-first-duplicate", [], "Keep first", "保留首帧"),
    EditorKeepLastDuplicate => ("editor-keep-last-duplicate", [], "Keep last", "保留末帧"),
    EditorKeepDuplicateDelay => ("editor-keep-duplicate-delay", [], "Keep delay", "保留延时"),
    EditorSumDuplicateDelay => ("editor-sum-duplicate-delay", [], "Sum delay", "延时求和"),
    EditorAverageDuplicateDelay => ("editor-average-duplicate-delay", [], "Average delay", "平均延时"),
    EditorRemoveDuplicates => ("editor-remove-duplicates", [], "Remove duplicates", "移除重复帧"),
    EditorDuplicateScanLimit => ("editor-duplicate-scan-limit", [], "Synchronous scan is limited to 256 selected frames and bounded render surfaces.", "同步扫描最多处理 256 个选中帧，并限制渲染画面的资源用量。"),
    EditorTransitions => ("editor-transitions", [], "Transitions", "转场"),
    EditorNoTransition => ("editor-no-transition", [], "The current frame has no outgoing transition.", "当前帧未设置向下一帧的转场。"),
    EditorCreateReplaceTransition => ("editor-create-replace-transition", [], "Create / replace", "创建 / 替换"),
    EditorDeletePairTransition => ("editor-delete-pair-transition", [], "Delete current pair transition", "删除当前帧对的转场"),
    EditorTransitionDurationHint => ("editor-transition-duration-hint", ["maximum"], "Duration is added to the timeline; steps must be 1..={maximum}.", "转场时长会加入时间线；步数必须在 1..={maximum} 之间。"),
    EditorTransitionType => ("editor-transition-type", [], "Type", "类型"),
    EditorTransitionTotalUs => ("editor-transition-total-us", [], "Total µs", "总时长 µs"),
    EditorTransitionSteps => ("editor-transition-steps", [], "Steps", "步数"),
    EditorFadeRgba => ("editor-fade-rgba", [], "Fade RGBA", "淡化 RGBA"),
    EditorFadeToNext => ("editor-fade-to-next", [], "Fade to next", "淡化到下一帧"),
    EditorFadeToRgba => ("editor-fade-to-rgba", [], "Fade to RGBA", "淡化到 RGBA"),
    EditorSlideLeft => ("editor-slide-left", [], "Slide left", "向左滑动"),
    EditorSlideRight => ("editor-slide-right", [], "Slide right", "向右滑动"),
    EditorSlideUp => ("editor-slide-up", [], "Slide up", "向上滑动"),
    EditorSlideDown => ("editor-slide-down", [], "Slide down", "向下滑动"),
    EditorTransitionSummary => ("editor-transition-summary", ["kind", "steps", "duration"], "Current outgoing: {kind} · {steps} step(s) · {duration} µs added", "当前转场：{kind} · {steps} 步 · 增加 {duration} µs"),
    EditorImageGeometry => ("editor-image-geometry", [], "Image geometry", "图像几何"),
    EditorWholeImageGeometryHint => ("editor-whole-image-geometry-hint", [], "Crop, resize and rotation affect all frames and their existing artwork.", "裁剪、缩放和旋转会作用于所有帧及其中已有的图层内容。"),
    EditorGeometryCoordinatesHint => ("editor-geometry-coordinates-hint", [], "Coordinates refer to the current image. Flip and effects use the selected frames.", "坐标以当前图像为准。翻转和效果仅作用于选中帧。"),
    EditorCrop => ("editor-crop", [], "Crop", "裁剪"),
    EditorApplyCrop => ("editor-apply-crop", [], "Apply crop", "应用裁剪"),
    EditorRemoveLastCrop => ("editor-remove-last-crop", [], "Remove last crop", "移除最近一次裁剪"),
    EditorRemoveLastCropHint => ("editor-remove-last-crop-hint", [], "Removes the most recent crop on all frames. Later layers keep their stage coordinates; a later crop or effect that no longer fits prevents the edit. Use Undo to restore the exact previous layout.", "移除所有帧上最近一次裁剪。后续图层保留各自阶段的坐标；若后续裁剪或效果不再适配，将拒绝此次编辑。使用撤销可精确恢复之前的布局。"),
    EditorResizeCurrentImage => ("editor-resize-current-image", [], "Resize current image", "缩放当前图像"),
    EditorResize => ("editor-resize", [], "Resize", "缩放"),
    EditorRemoveLastResize => ("editor-remove-last-resize", [], "Remove last resize", "移除最近一次缩放"),
    EditorRemoveLastResizeHint => ("editor-remove-last-resize-hint", [], "Removes the most recent resize on all frames. Later layers retain their own stage coordinates; re-author an input group to recalculate its source-coordinate mapping.", "移除所有帧上最近一次缩放。后续图层保留各自阶段的坐标；重新编辑输入标注组可重新计算其来源坐标映射。"),
    EditorRotateLeft => ("editor-rotate-left", [], "Rotate left", "向左旋转"),
    EditorRotateRight => ("editor-rotate-right", [], "Rotate right", "向右旋转"),
    EditorFlipHorizontal => ("editor-flip-horizontal", [], "Flip H", "水平翻转"),
    EditorFlipVertical => ("editor-flip-vertical", [], "Flip V", "垂直翻转"),
    RecorderRetargetRejected => ("recorder-retarget-rejected", ["error"], "Could not move the capture area; recording continues at its last accepted position: {error}", "无法移动录制区域；将继续在最后确认的位置录制：{error}"),
    RecorderRetargetWorkerExited => ("recorder-retarget-worker-exited", [], "Could not move the capture area because the recording worker has exited.", "录制后台任务已退出，无法移动录制区域。"),
    RecorderSnapshotCaptured => ("recorder-snapshot-captured", ["sequence", "seconds"], "Snapshot captured from native frame {sequence} at {seconds}s.", "已从时间为 {seconds}s 的原生帧 {sequence} 采集快照。"),
    RecorderSnapshotRejected => ("recorder-snapshot-rejected", ["reason"], "Snapshot was not captured: {reason}", "未能采集快照：{reason}"),
    RecorderSnapshotWorkerExited => ("recorder-snapshot-worker-exited", [], "Snapshot was not captured because the recording worker exited.", "录制后台任务已退出，未能采集快照。"),
    RecorderRecordedProjectOpenFailed => ("recorder-recorded-project-open-failed", ["error"], "Could not open recorded project: {error}", "无法打开录制工程：{error}"),
    RecorderDiscardedAutosaveRemoved => ("recorder-discarded-autosave-removed", [], "Recording discarded; its autosave project was removed.", "录制已丢弃，自动保存的工程已移除。"),
    RecorderDiscardCleanupIssue => ("recorder-discard-cleanup-issue", ["error"], "Recording discarded, but {error}", "录制已丢弃，但仍有问题：{error}"),
    RecorderFailedBeforeAutosave => ("recorder-failed-before-autosave", ["error"], "Recording failed before autosave project creation: {error}", "录制失败，尚未创建自动保存工程：{error}"),
    RecorderFailedRecoverable => ("recorder-failed-recoverable", ["error", "path"], "Recording failed: {error}. Recoverable autosave retained at {path}", "录制失败：{error}。可恢复的自动保存工程保留在 {path}"),
    RecorderStartExpired => ("recorder-start-expired", [], "Start expired while preparing the controls. Retry after the recorder is ready.", "准备控件期间开始请求已超时。请在录制器就绪后重试。"),
    RecorderStartRestored => ("recorder-start-restored", [], "Start cancelled because the controls were restored before capture began.", "录制开始前控件被还原，因此已取消开始请求。"),
    RecorderControlsSettleFailed => ("recorder-controls-settle-failed", [], "The recorder could not finish hiding or updating its controls; stopping safely.", "录制器未能完成控件隐藏或更新；正在安全停止。"),
    RecorderSnapshotsRejectedForMove => ("recorder-snapshots-rejected-for-move", ["count"], "{count} snapshot requests were not captured while the recording region changed.", "录制区域改变期间，有 {count} 次快照请求未能采集。"),
    RecorderSnapshotDuringMove => ("recorder-snapshot-during-move", [], "Snapshot not captured while the recording position is changing.", "录制位置正在改变，未能采集快照。"),
    EditorUndoCompleted => ("editor-undo-completed", [], "Undid the previous edit.", "已撤销上一次编辑。"),
    EditorRedoCompleted => ("editor-redo-completed", [], "Reapplied the edit.", "已重新应用该编辑。"),
    EditorOperationFailed => ("editor-operation-failed", ["operation", "error"], "Editor {operation} failed: {error}", "编辑器操作 {operation} 失败：{error}"),
    PreviewZoomFit => ("preview-zoom-fit", [], "Fit", "适应窗口"),
    PreviewZoomNative => ("preview-zoom-native", [], "100%", "100%"),
    PreviewZoomDouble => ("preview-zoom-double", [], "200%", "200%"),
    PreviewFitHint => ("preview-fit-hint", [], "Fit may downsample large images; small images can be enlarged for visibility.", "适应窗口时可能降低大图的预览分辨率；小图可放大以便查看。"),
    PreviewExactPixelHint => ("preview-exact-pixel-hint", [], "100% = one image pixel per physical screen pixel. Use scrollbars, wheel or middle-drag to pan.", "100% 表示一个图像像素对应一个物理屏幕像素。可使用滚动条、滚轮或按住中键拖动来平移。"),
    PreviewInvalidGeometry => ("preview-invalid-geometry", [], "Preview dimensions, viewport and pixel scale must be finite and positive", "预览尺寸、视口尺寸和像素缩放比例必须为有限正数"),
    PreviewUnrepresentableExtent => ("preview-unrepresentable-extent", [], "Preview display extent cannot be represented by the UI", "界面无法表示此预览显示尺寸"),
    CropStart => ("crop-start", [], "Crop on preview…", "在预览中裁剪…"),
    CropTitle => ("crop-title", [], "Crop draft · applies to all frames", "裁剪草稿 · 应用于所有帧"),
    CropHelp => ("crop-help", [], "Drag over the image or edit pixel bounds. Nothing changes until Apply.", "在图像上拖动或编辑像素边界。点击应用前不会更改图像。"),
    CropApplyAll => ("crop-apply-all", [], "Apply crop to all frames", "将裁剪应用于所有帧"),
    CropCancel => ("crop-cancel", [], "Cancel crop", "取消裁剪"),
    CropFieldX => ("crop-field-x", [], "X", "X"),
    CropFieldY => ("crop-field-y", [], "Y", "Y"),
    CropFieldWidth => ("crop-field-width", [], "W", "宽"),
    CropFieldHeight => ("crop-field-height", [], "H", "高"),
    CropSelectOriginal => ("crop-select-original", [], "Select an original frame before starting a crop draft", "开始裁剪草稿前，请先选择一个原始帧"),
    CropDraftDiscarded => ("crop-draft-discarded", [], "Crop draft discarded because the project, revision or selection changed.", "工程、修订版本或选择已改变，因此已丢弃裁剪草稿。"),
    CropNoDraft => ("crop-no-draft", [], "No crop draft is active", "当前没有裁剪草稿"),
    CropStaleDraft => ("crop-stale-draft", [], "Crop draft is stale; start a new draft for the current selection", "裁剪草稿已过期；请为当前选择创建新草稿"),
    CropFinishGesture => ("crop-finish-gesture", [], "Finish the crop gesture before applying", "请先完成裁剪拖动，再应用更改"),
    CropApplied => ("crop-applied", [], "Crop applied to all frames. Undo restores the previous image layout.", "裁剪已应用于所有帧。撤销可恢复之前的图像布局。"),
    CropUnsignedPixels => ("crop-unsigned-pixels", [], "Crop coordinates must be unsigned whole pixels", "裁剪坐标必须是非负整数像素"),
    CropFitsImage => ("crop-fits-image", [], "Crop must fit inside the current rendered image", "裁剪区域必须位于当前渲染图像内"),
    CropEmptySize => ("crop-empty-size", [], "physical width and height must be greater than zero", "物理像素宽度和高度必须大于零"),
    CropCoordinateOverflow => ("crop-coordinate-overflow", [], "physical rectangle coordinate overflows u32", "物理像素矩形坐标超出 u32 范围"),
    CropInvalidBounds => ("crop-invalid-bounds", ["error"], "Invalid crop bounds: {error}", "裁剪边界无效：{error}"),
    CropOperationFailed => ("crop-operation-failed", ["error"], "Crop operation failed: {error}", "裁剪操作失败：{error}"),
    CropStartFailed => ("crop-start-failed", ["error"], "Could not start crop draft: {error}", "无法开始裁剪草稿：{error}"),
    PreviewCinemagraphReference => ("preview-cinemagraph-reference", [], "Cinemagraph reference · frame 1", "Cinemagraph 参考图 · 第 1 帧"),
    PreviewTransitionTitle => ("preview-transition-title", [], "Transition preview", "转场预览"),
    PreviewCurrentFrameTitle => ("preview-current-frame-title", [], "Current frame preview", "当前帧预览"),
    PreviewSelectFrame => ("preview-select-frame", [], "Select a frame to preview it.", "请选择一帧进行预览。"),
    PreviewRenderFailed => ("preview-render-failed", ["error"], "Could not render preview: {error}", "无法渲染预览：{error}"),
    PreviewExactLimitHint => ("preview-exact-limit-hint", [], "Exact-pixel views keep the existing texture/cache limits. Choose Fit for a bounded downsampled preview.", "原像素视图仍受现有纹理和缓存限制。请选择适应窗口，以使用资源有界的降采样预览。"),
    PreviewTransitionStep => ("preview-transition-step", ["step"], "Transition step {step} · select an original frame to edit", "转场第 {step} 步 · 请选择原始帧进行编辑"),
    PreviewRenderedSizes => ("preview-rendered-sizes", ["rendered_width", "rendered_height", "preview_width", "preview_height"], "Rendered {rendered_width}×{rendered_height} · preview {preview_width}×{preview_height}", "渲染尺寸 {rendered_width}×{rendered_height} · 预览尺寸 {preview_width}×{preview_height}"),
    PreviewBlockedAssets => ("preview-blocked-assets", ["count"], "Preview and export are blocked by {count} unresolved asset issue(s).", "有 {count} 项素材问题尚未解决，无法预览或导出。"),
    ExportFinishAssetJob => ("export-finish-asset-job", [], "Finish the active editor asset job before exporting.", "请先完成当前编辑器素材任务，再进行导出。"),
    ExportResolveAssets => ("export-resolve-assets", ["count"], "Resolve {count} asset issue(s) before exporting.", "导出前请先解决 {count} 项素材问题。"),
    ExportProgress => ("export-progress", ["phase", "frames_rendered", "frames_encoded", "total_frames"], "{phase}: rendered {frames_rendered}/{total_frames}, encoded {frames_encoded}/{total_frames}", "{phase}：已渲染 {frames_rendered}/{total_frames}，已编码 {frames_encoded}/{total_frames}"),
    ExportStartingWorker => ("export-starting-worker", [], "Starting export worker…", "正在启动导出后台任务…"),
    ExportCancelling => ("export-cancelling", [], "Cancelling…", "正在取消…"),
    ExportFinishingResult => ("export-finishing-result", [], "Finishing export result…", "正在处理导出结果…"),
    ExportOutputLabel => ("export-output-label", [], "Output", "输出"),
    ExportScopeAll => ("export-scope-all", [], "All", "全部"),
    ExportScopeSelectedCount => ("export-scope-selected-count", ["count"], "Selected ({count})", "选中帧（{count}）"),
    ExportMaximumColors => ("export-maximum-colors", [], "Maximum colors", "最大颜色数"),
    ExportPalette => ("export-palette", [], "Palette", "调色板"),
    ExportPaletteLocal => ("export-palette-local", [], "Local per frame", "逐帧局部调色板"),
    ExportPaletteGlobal => ("export-palette-global", [], "Global", "全局"),
    ExportQuantizer => ("export-quantizer", [], "Quantizer", "颜色量化器"),
    ExportFixedPaletteMinimum => ("export-fixed-palette-minimum", ["required"], "Fixed palette · at least {required} colors including transparency", "固定调色板 · 至少需要 {required} 种颜色，含透明色"),
    ExportCustomColors => ("export-custom-colors", [], "Custom colors", "自定义颜色"),
    ExportCustomPaletteHint => ("export-custom-palette-hint", [], "2..=256 strict #RRGGBB entries separated by commas or whitespace; count must not exceed Maximum colors.", "请输入 2..=256 个严格符合 #RRGGBB 格式的颜色，以逗号或空白分隔；数量不得超过最大颜色数。"),
    ExportCustomTransparency => ("export-custom-transparency", [], "Custom transparency", "自定义透明色"),
    ExportTransparentIndex => ("export-transparent-index", [], "Use transparent palette index", "使用透明色索引"),
    ExportZeroBased => ("export-zero-based", [], "zero-based", "从 0 开始"),
    ExportTransparencyRequired => ("export-transparency-required", [], "Required when rendered pixels cross the alpha threshold.", "渲染像素的透明度越过 Alpha 阈值时需要设置此项。"),
    ExportDither => ("export-dither", [], "Dither", "抖动"),
    ExportAlphaThreshold => ("export-alpha-threshold", [], "Alpha threshold", "Alpha 阈值"),
    ExportLoop => ("export-loop", [], "Loop", "循环"),
    ExportLoopInfinite => ("export-loop-infinite", [], "Infinite", "无限"),
    ExportLoopFinite => ("export-loop-finite", [], "Finite", "有限次数"),
    ExportOptimization => ("export-optimization", [], "Optimization", "优化"),
    ExportChangedRectangles => ("export-changed-rectangles", [], "Changed rectangles", "仅编码变化矩形"),
    ExportOverwriteOutput => ("export-overwrite-output", [], "Overwrite output", "覆盖输出文件"),
    ExportQuantizerMedianCut => ("export-quantizer-median-cut", [], "Median cut", "Median cut（中位切分）"),
    ExportQuantizerOctree => ("export-quantizer-octree", [], "Octree", "Octree（八叉树）"),
    ExportQuantizerWu => ("export-quantizer-wu", [], "Wu variance", "Wu 方差"),
    ExportQuantizerGrayscale => ("export-quantizer-grayscale", [], "Grayscale", "灰度"),
    ExportQuantizerMostUsed => ("export-quantizer-most-used", [], "Most used", "最常用颜色"),
    ExportQuantizerNeuQuant => ("export-quantizer-neu-quant", [], "NeuQuant", "NeuQuant"),
    ExportQuantizerWebSafe => ("export-quantizer-web-safe", [], "Web safe 216 (fixed)", "Web 安全色 216（固定）"),
    ExportQuantizerMonochrome => ("export-quantizer-monochrome", [], "Monochrome (fixed)", "单色（固定）"),
    ExportQuantizerWindows => ("export-quantizer-windows", [], "Windows 16 (fixed)", "Windows 16（固定）"),
    ExportQuantizerCustom => ("export-quantizer-custom", [], "Custom palette", "自定义调色板"),
    ExportDitherNone => ("export-dither-none", [], "None", "无"),
    ExportDitherBayer => ("export-dither-bayer", [], "Bayer 4×4", "Bayer 4×4"),
    ExportDitherDotted => ("export-dither-dotted", [], "Dotted halftone", "点状半色调"),
    ExportDitherBlueNoise => ("export-dither-blue-noise", [], "Blue noise", "蓝噪声"),
    ExportDitherInterleavedNoise => ("export-dither-interleaved-noise", [], "Interleaved gradient noise", "交错梯度噪声"),
    ExportDitherFloydSteinberg => ("export-dither-floyd-steinberg", [], "Floyd–Steinberg", "Floyd–Steinberg"),
    ExportDitherAtkinson => ("export-dither-atkinson", [], "Atkinson", "Atkinson"),
    ExportDitherBurkes => ("export-dither-burkes", [], "Burkes", "Burkes"),
    ExportDitherSierra => ("export-dither-sierra", [], "Sierra", "Sierra"),
    ExportDitherSierraLite => ("export-dither-sierra-lite", [], "Sierra Lite", "Sierra Lite"),
    ExportDitherTwoRowSierra => ("export-dither-two-row-sierra", [], "Two-row Sierra", "双行 Sierra"),
    ExportDitherJarvisJudiceNinke => ("export-dither-jarvis-judice-ninke", [], "Jarvis–Judice–Ninke", "Jarvis–Judice–Ninke"),
    ExportDitherStucki => ("export-dither-stucki", [], "Stucki", "Stucki"),
    ExportDitherStevensonArce => ("export-dither-stevenson-arce", [], "Stevenson–Arce", "Stevenson–Arce"),
    ExportNoProject => ("export-no-project", [], "No active editor project is available for export.", "当前没有可供导出的编辑工程。"),
    ExportBlockedAssets => ("export-blocked-assets", ["count"], "Cannot export while the project has {count} unresolved asset issue(s).", "工程中有 {count} 项素材问题尚未解决，无法导出。"),
    ExportStarted => ("export-started", [], "GIF export started…", "GIF 导出已开始…"),
    ExportWorkerNoResult => ("export-worker-no-result", [], "GIF export worker finished without a result.", "GIF 导出后台任务已结束，但未返回结果。"),
    ExportCompletedReport => ("export-completed-report", ["selected_frames", "encoded_frames", "bytes", "path"], "Exported {selected_frames} selected frames as {encoded_frames} GIF images ({bytes} bytes) to {path}", "已将 {selected_frames} 个选中帧导出为 {encoded_frames} 幅 GIF 图像（{bytes} 字节），保存到 {path}"),
    ExportPhasePreparing => ("export-phase-preparing", [], "Preparing", "准备中"),
    ExportPhaseAnalyzingPalette => ("export-phase-analyzing-palette", [], "AnalyzingPalette", "正在分析调色板"),
    ExportPhaseSamplingPalette => ("export-phase-sampling-palette", [], "SamplingPalette", "正在采样调色板"),
    ExportPhaseRendering => ("export-phase-rendering", [], "Rendering", "渲染中"),
    ExportPhaseSyncing => ("export-phase-syncing", [], "Syncing", "正在同步"),
    ExportPresetPaletteLimit => ("export-preset-palette-limit", [], "Preset custom palette text must not exceed 4096 bytes.", "预设的自定义调色板文本不得超过 4096 字节。"),
    ExportPresetAdaptiveUnsupported => ("export-preset-adaptive-unsupported", [], "This legacy preset uses Adaptive palette selection, which is not available in the editor. Choose an explicit palette and save a new preset.", "此旧预设使用编辑器不支持的 Adaptive 调色板选择策略。请明确选择一种调色板并保存为新预设。"),
    ExportProjectPreset => ("export-project-preset", [], "Project preset", "工程预设"),
    ExportChoosePreset => ("export-choose-preset", [], "Choose a preset", "选择预设"),
    ExportPresetLoad => ("export-preset-load", [], "Load", "加载"),
    ExportPresetUpdate => ("export-preset-update", [], "Update selected", "更新选中预设"),
    ExportPresetName => ("export-preset-name", [], "Preset name", "预设名称"),
    ExportPresetSaveNew => ("export-preset-save-new", [], "Save new", "另存新预设"),
    ExportPresetRename => ("export-preset-rename", [], "Rename selected", "重命名选中预设"),
    ExportPresetStorageHint => ("export-preset-storage-hint", [], "Saved inside this project · changes support Undo · loading clears overwrite permission and keeps the output path.", "保存在此工程中 · 更改支持撤销 · 加载会清除覆盖权限，并保留输出路径。"),
    ExportPresetFailed => ("export-preset-failed", ["error"], "Preset: {error}", "预设：{error}"),
    ExportPresetSelectFirst => ("export-preset-select-first", [], "Select an existing preset first.", "请先选择一个已有预设。"),
    ExportPresetLoaded => ("export-preset-loaded", ["name"], "Loaded {name}. Review the current frame selection before exporting.", "已加载 {name}。导出前请检查当前帧选择。"),
    ExportPresetSaved => ("export-preset-saved", ["name"], "Saved {name} in this project.", "已在此工程中保存 {name}。"),
    ExportPresetUpdated => ("export-preset-updated", ["name"], "Updated {name}. Undo restores its previous settings.", "已更新 {name}。撤销可恢复其之前的设置。"),
    ExportPresetRenamed => ("export-preset-renamed", ["name"], "Renamed preset to {name}.", "已将预设重命名为 {name}。"),
    ExportPresetDeleted => ("export-preset-deleted", [], "Deleted preset. Undo restores it.", "预设已删除。撤销可恢复该预设。"),
    ExportNoFrames => ("export-no-frames", [], "The project has no frames to export.", "工程中没有可供导出的帧。"),
    ExportNoSelectedFrames => ("export-no-selected-frames", [], "Select at least one frame before exporting Selected frames.", "导出选中帧前，请至少选择一帧。"),
    ExportColorsRange => ("export-colors-range", [], "Maximum colors must be between 2 and 256.", "最大颜色数必须在 2 到 256 之间。"),
    ExportInvalidCustomPalette => ("export-invalid-custom-palette", ["error"], "Invalid custom palette: {error}", "自定义调色板无效：{error}"),
    ExportCustomPaletteTooLarge => ("export-custom-palette-too-large", ["count", "maximum"], "Custom palette contains {count} colors, above Maximum colors {maximum}.", "自定义调色板包含 {count} 种颜色，超过最大颜色数 {maximum}。"),
    ExportFixedPaletteRequired => ("export-fixed-palette-required", ["required"], "The selected fixed palette requires at least {required} colors including transparency.", "所选固定调色板至少需要 {required} 种颜色，含透明色。"),
    ExportFiniteLoopMinimum => ("export-finite-loop-minimum", [], "Finite loop count must be at least one.", "有限循环次数必须至少为 1。"),
    ExportOutputFileRequired => ("export-output-file-required", [], "Export output must identify a GIF file.", "导出路径必须指定一个 GIF 文件。"),
    ExportOutputGifExtension => ("export-output-gif-extension", [], "Export output filename must end in .gif.", "导出文件名必须以 .gif 结尾。"),
    EditorInputRequired => ("editor-input-required", ["field"], "{field} is required", "请输入{field}。"),
    EditorInputInvalid => ("editor-input-invalid", ["field", "error"], "invalid {field}: {error}", "{field}无效：{error}"),
    ImageBorderScopeHint => ("image-border-scope-hint", [], "Positive edges draw inside; negative edges expand the canvas on all frames.", "边线为正值时绘制在画布内；负值会扩展所有帧的画布。"),
    ImageBorderTop => ("image-border-top", [], "Top", "上"),
    ImageBorderRight => ("image-border-right", [], "Right", "右"),
    ImageBorderBottom => ("image-border-bottom", [], "Bottom", "下"),
    ImageBorderLeft => ("image-border-left", [], "Left", "左"),
    ImageBorderColor => ("image-border-color", [], "Border", "边框颜色"),
    ImageEffectBackground => ("image-effect-background", [], "Background", "背景颜色"),
    ImageBorderBackgroundHint => ("image-border-background-hint", [], "The reference border uses a white background. A transparent background preserves source transparency.", "参考边框使用白色背景。透明背景可保留来源的透明度。"),
    ImageShadowScopeHint => ("image-shadow-scope-hint", [], "Applies to all frames. The canvas expands to keep space for the shadow.", "应用于所有帧。画布会扩展以容纳阴影。"),
    ImageShadowBlur => ("image-shadow-blur", [], "Blur", "模糊"),
    ImageShadowDistance => ("image-shadow-distance", [], "Distance", "距离"),
    ImageShadowDirection => ("image-shadow-direction", [], "Direction", "方向"),
    ImageShadowOpacity => ("image-shadow-opacity", [], "Opacity", "不透明度"),
    ImageShadowAnglesHint => ("image-shadow-angles-hint", [], "0° points right; 90° points up. Blur, distance, angle and opacity keep two decimal places.", "0° 向右，90° 向上。模糊、距离、角度和不透明度保留两位小数。"),
    ImageShadowColor => ("image-shadow-color", [], "Shadow color", "阴影颜色"),
    ImageShadowGifHint => ("image-shadow-gif-hint", [], "Use an opaque background to keep soft shadow edges in a GIF; GIF transparency is binary.", "使用不透明背景可在 GIF 中保留柔和的阴影边缘；GIF 只有透明和不透明两种状态。"),
    EffectsTitle => ("effects-title", [], "Frame effects", "帧效果"),
    EffectsReplaceNumber => ("effects-replace-number", [], "Replace #", "替换 #"),
    EffectsAdd => ("effects-add", [], "Add effect", "添加效果"),
    EffectsReplace => ("effects-replace", [], "Replace effect", "替换效果"),
    EffectsClear => ("effects-clear", [], "Clear effects", "清除效果"),
    EffectsClearHint => ("effects-clear-hint", [], "Selected frames, unless a canvas-changing border or shadow is involved: then all frames are cleared to keep a consistent animation size. Undo restores the whole edit.", "仅清除选中帧的效果；若涉及改变画布的边框或阴影，则清除所有帧的效果，以保持动画尺寸一致。撤销可恢复整次编辑。"),
    EffectsCanvasScopeHint => ("effects-canvas-scope-hint", [], "Adding shadow or an outer border affects all frames. Replacing or clearing canvas effects also uses all frames; later artwork keeps its authored stage coordinates.", "添加阴影或外边框会作用于所有帧。替换或清除画布效果也会作用于所有帧；后续图层保留其创建阶段的坐标。"),
    EffectsRegion => ("effects-region", [], "Canvas region X/Y/W/H", "画布区域 X/Y/W/H"),
    EffectsRadius => ("effects-radius", [], "Radius", "半径"),
    EffectsBlock => ("effects-block", [], "Block", "块大小"),
    EffectsPercent => ("effects-percent", [], "Percent", "百分比"),
    EffectsEdges => ("effects-edges", [], "Edges T/R/B/L", "边线 T/R/B/L"),
    EffectsOffset => ("effects-offset", [], "Offset X/Y", "偏移 X/Y"),
    EffectsBlur => ("effects-blur", [], "Blur", "模糊"),
    EffectsRgba => ("effects-rgba", [], "RGBA", "RGBA"),
    EffectChoiceBlur => ("effect-choice-blur", [], "Blur", "模糊"),
    EffectChoicePixelate => ("effect-choice-pixelate", [], "Pixelate", "像素化"),
    EffectChoiceDarken => ("effect-choice-darken", [], "Darken", "变暗"),
    EffectChoiceLighten => ("effect-choice-lighten", [], "Lighten", "变亮"),
    EffectChoiceLegacyBorder => ("effect-choice-legacy-border", [], "Legacy inset border", "旧版内边框"),
    EffectChoiceLegacyShadow => ("effect-choice-legacy-shadow", [], "Legacy clipped shadow", "旧版裁切阴影"),
    EffectChoiceImageBorder => ("effect-choice-image-border", [], "Border · inner / outer", "边框 · 内 / 外"),
    EffectChoiceImageShadow => ("effect-choice-image-shadow", [], "Shadow · expanded canvas", "阴影 · 扩展画布"),
    EffectFieldBlurRadius => ("effect-field-blur-radius", [], "blur radius", "模糊半径"),
    EffectFieldBlockSize => ("effect-field-block-size", [], "pixel block size", "像素块大小"),
    EffectFieldTonePercent => ("effect-field-tone-percent", [], "tone percentage", "明暗百分比"),
    EffectFieldBorderTop => ("effect-field-border-top", [], "border top", "上边框"),
    EffectFieldBorderRight => ("effect-field-border-right", [], "border right", "右边框"),
    EffectFieldBorderBottom => ("effect-field-border-bottom", [], "border bottom", "下边框"),
    EffectFieldBorderLeft => ("effect-field-border-left", [], "border left", "左边框"),
    EffectFieldShadowBlur => ("effect-field-shadow-blur", [], "shadow blur radius", "阴影模糊半径"),
    EffectFieldShadowX => ("effect-field-shadow-x", [], "shadow X", "阴影 X 偏移"),
    EffectFieldShadowY => ("effect-field-shadow-y", [], "shadow Y", "阴影 Y 偏移"),
    EffectFieldRegionX => ("effect-field-region-x", [], "effect region X", "效果区域 X 坐标"),
    EffectFieldRegionY => ("effect-field-region-y", [], "effect region Y", "效果区域 Y 坐标"),
    EffectFieldRegionWidth => ("effect-field-region-width", [], "effect region width", "效果区域宽度"),
    EffectFieldRegionHeight => ("effect-field-region-height", [], "effect region height", "效果区域高度"),
    EffectFieldRed => ("effect-field-red", [], "effect red", "效果红色通道"),
    EffectFieldGreen => ("effect-field-green", [], "effect green", "效果绿色通道"),
    EffectFieldBlue => ("effect-field-blue", [], "effect blue", "效果蓝色通道"),
    EffectFieldAlpha => ("effect-field-alpha", [], "effect alpha", "效果 Alpha"),
    EffectFieldIndex => ("effect-field-index", [], "effect number", "效果序号"),
    EffectUseImageBuilder => ("effect-use-image-builder", [], "Use the current-image effect builder for canvas effects.", "画布效果请使用当前图像效果构建器。"),
    EffectBlurRange => ("effect-blur-range", ["maximum"], "blur radius must be between 1 and {maximum}", "模糊半径必须在 1 到 {maximum} 之间。"),
    EffectBlockPositive => ("effect-block-positive", [], "pixel block size must be positive", "像素块大小必须大于零。"),
    EffectToneRange => ("effect-tone-range", [], "tone percentage must be between 0 and 100", "明暗百分比必须在 0 到 100 之间。"),
    EffectBorderPositive => ("effect-border-positive", [], "at least one border edge must be positive", "至少一条边框的宽度必须大于零。"),
    EffectShadowBlurMaximum => ("effect-shadow-blur-maximum", ["maximum"], "shadow blur radius must be at most {maximum}", "阴影模糊半径不得超过 {maximum}。"),
    EffectInvalidRegion => ("effect-invalid-region", ["error"], "invalid effect region: {error}", "效果区域无效：{error}"),
    EffectAlphaPositive => ("effect-alpha-positive", [], "effect color alpha must be positive", "效果颜色的 Alpha 必须大于零。"),
    EffectIndexPositive => ("effect-index-positive", [], "effect number is 1-based and must be positive", "效果序号从 1 开始，必须为正数。"),
    EditorSaveCheckpoint => ("editor-save-checkpoint", [], "Save checkpoint", "保存检查点"),
    EditorCheckpointSaved => ("editor-checkpoint-saved", [], "Project manifest checkpoint saved.", "工程清单检查点已保存。"),
    EditorSaveCompact => ("editor-save-compact", [], "Save & compact", "保存并压缩日志"),
    EditorCompacted => ("editor-compacted", [], "Project checkpoint saved and journal compacted.", "工程检查点已保存，日志已压缩。"),
    EditorRepairJournal => ("editor-repair-journal", [], "Repair journal", "修复日志"),
    EditorStatistics => ("editor-statistics", [], "Statistics", "统计信息"),
    EditorStatsFrames => ("editor-stats-frames", [], "Frames", "帧数"),
    EditorStatsSelectedFrames => ("editor-stats-selected-frames", [], "Selected frames", "选中帧数"),
    EditorStatsCanvas => ("editor-stats-canvas", [], "Canvas", "画布"),
    EditorStatsTotalDuration => ("editor-stats-total-duration", [], "Total duration", "总时长"),
    EditorStatsSelectedDuration => ("editor-stats-selected-duration", [], "Selected duration", "选中帧时长"),
    EditorStatsMinimumDelay => ("editor-stats-minimum-delay", [], "Minimum delay", "最短延时"),
    EditorStatsMaximumDelay => ("editor-stats-maximum-delay", [], "Maximum delay", "最长延时"),
    EditorStatsAverageDelay => ("editor-stats-average-delay", [], "Average delay", "平均延时"),
    EditorStatsUniqueAssets => ("editor-stats-unique-assets", [], "Unique assets", "唯一素材数"),
    EditorStatsAssetBytes => ("editor-stats-asset-bytes", [], "Asset descriptor bytes", "素材声明字节数"),
    EditorStatsCurrentFrame => ("editor-stats-current-frame", [], "Current frame", "当前帧"),
    EditorOptionalNone => ("editor-optional-none", [], "None", "无"),
    EditorStatsCurrentValue => ("editor-stats-current-value", ["number", "start", "delay"], "#{number} · start {start} · delay {delay}", "#{number} · 开始 {start} · 延时 {delay}"),
    EditorJournalClean => ("editor-journal-clean", [], "Journal is already clean; no repair was needed.", "日志已处于正常状态，无需修复。"),
    EditorJournalPreserved => ("editor-journal-preserved", ["path"], "Rejected journal preserved at {path}.", "被拒绝的日志已保留在 {path}。"),
    EditorShapeOverlays => ("editor-shape-overlays", [], "Shape overlays", "形状图层"),
    EditorSelectedOverlayScope => ("editor-selected-overlay-scope", [], "Applies to selected frames only; gaps in the selection stay unchanged.", "仅应用于选中帧；选择范围中的间隙保持不变。"),
    EditorAddShapeOverlay => ("editor-add-shape-overlay", [], "Add shape overlay", "添加形状图层"),
    EditorOverlayName => ("editor-overlay-name", [], "Name", "名称"),
    EditorOverlayKind => ("editor-overlay-kind", [], "Kind", "类型"),
    EditorTrackOpacity => ("editor-track-opacity", [], "Track opacity", "轨道不透明度"),
    EditorOverlayBlend => ("editor-overlay-blend", [], "Blend", "混合模式"),
    EditorOverlayBounds => ("editor-overlay-bounds", [], "Bounds X/Y/W/H", "边界 X/Y/W/H"),
    EditorStrokeWidth => ("editor-stroke-width", [], "Stroke width", "描边宽度"),
    EditorStrokeRgba => ("editor-stroke-rgba", [], "Stroke RGBA", "描边 RGBA"),
    EditorFill => ("editor-fill", [], "Fill", "填充"),
    EditorFillRgba => ("editor-fill-rgba", [], "Fill RGBA", "填充 RGBA"),
    EditorFreeDrawing => ("editor-free-drawing", [], "Free drawing", "自由绘图"),
    EditorDrawStroke => ("editor-draw-stroke", [], "Draw one stroke on preview", "在预览中绘制一笔"),
    EditorDragStroke => ("editor-drag-stroke", [], "Drag once across the current preview.", "请在当前预览上拖动一次。"),
    EditorCancelStroke => ("editor-cancel-stroke", [], "Cancel stroke", "取消笔迹"),
    EditorCommitDrawing => ("editor-commit-drawing", [], "Commit drawing overlay", "提交绘图图层"),
    EditorDrawingPoints => ("editor-drawing-points", ["count"], "{count} point(s)", "{count} 个点"),
    EditorDrawingLimit => ("editor-drawing-limit", ["limit"], "Stroke stopped at the {limit}-point safety limit.", "笔迹已达到 {limit} 个点的安全上限，已停止采集。"),
    EditorShapeLine => ("editor-shape-line", [], "Line", "直线"),
    EditorShapeArrow => ("editor-shape-arrow", [], "Arrow", "箭头"),
    EditorShapeRectangle => ("editor-shape-rectangle", [], "Rectangle", "矩形"),
    EditorShapeEllipse => ("editor-shape-ellipse", [], "Ellipse", "椭圆"),
    EditorBlendNormal => ("editor-blend-normal", [], "Normal", "正常"),
    EditorBlendMultiply => ("editor-blend-multiply", [], "Multiply", "正片叠底"),
    EditorBlendScreen => ("editor-blend-screen", [], "Screen", "滤色"),
    EditorContentRaster => ("editor-content-raster", [], "Raster", "栅格图像"),
    EditorContentText => ("editor-content-text", [], "Text", "文字"),
    EditorContentShape => ("editor-content-shape", [], "Shape", "形状"),
    EditorContentDrawing => ("editor-content-drawing", [], "Drawing", "绘图"),
    EditorContentKeyStroke => ("editor-content-key-stroke", [], "Key stroke", "按键"),
    EditorContentCursor => ("editor-content-cursor", [], "Cursor", "光标"),
    EditorContentMouseClick => ("editor-content-mouse-click", [], "Mouse click", "鼠标点击"),
    EditorContentProgress => ("editor-content-progress", [], "Progress", "进度"),
    EditorContentEmpty => ("editor-content-empty", [], "Empty", "空"),
    EditorFrameOwned => ("editor-frame-owned", [], "Frame-owned", "帧归属"),
    EditorTimeAnchored => ("editor-time-anchored", [], "Time-anchored", "时间锚定"),
    EditorLayerRow => ("editor-layer-row", ["number", "name", "kind", "count", "ownership"], "Layer {number} · {name} · {kind} · {count} item(s) · {ownership}", "图层 {number} · {name} · {kind} · {count} 项 · {ownership}"),
    EditorHideLayer => ("editor-hide-layer", [], "Hide", "隐藏"),
    EditorShowLayer => ("editor-show-layer", [], "Show", "显示"),
    EditorAttachFrames => ("editor-attach-frames", [], "Attach to frames", "附着到帧"),
    EditorRemoveTrack => ("editor-remove-track", [], "Remove track", "移除轨道"),
    EditorPreviousLayerPage => ("editor-previous-layer-page", [], "Previous page", "上一页"),
    EditorNextLayerPage => ("editor-next-layer-page", [], "Next page", "下一页"),
    EditorLayerPage => ("editor-layer-page", ["start", "end", "total", "page", "pages"], "Layers {start}–{end} of {total} · Page {page} / {pages}", "图层 {start}–{end}，共 {total} 个 · 第 {page} / {pages} 页"),
    EditorStageOrder => ("editor-stage-order", [], "Layer order applies within each editing stage.", "图层顺序仅在各编辑阶段内部生效。"),
    EditorLayerVisibilityHelp => ("editor-layer-visibility-help", [], "Toggle this layer in previews and GIF export, keeping its artwork, assets and paint stage. Previously frozen reference pixels are unchanged; earlier artwork changes only the live region. Undo restores visibility.", "切换此图层在预览和 GIF 导出中的可见性，保留其内容、素材和绘制阶段。此前冻结的参考像素保持不变；修改早期图层内容只影响动态区域。撤销可恢复可见性。"),
    EditorAttachFramesHelp => ("editor-attach-frames-help", [], "Preserve this entire layer's current frame appearances, including hidden content. Future frame moves and copies carry its marks. Original input history is not inferred; some older input groups cannot be regenerated. Undo restores the timed layer, but the project format stays upgraded.", "保留整个图层当前的逐帧外观，包括隐藏内容。之后移动或复制帧时会携带其标记。不会推测原始输入历史；部分旧输入标注组无法重新生成。撤销会恢复时间锚定图层，但不会回退工程格式版本。"),
    EditorAttachUnknownHelp => ("editor-attach-unknown-help", [], "This older annotation group did not save its original authoring coverage. Keep its timed behavior or recreate it from an explicit frame selection; visible marks alone cannot prove that coverage.", "此旧标注组未保存原始创作范围。请保留其按时间定位的行为，或从明确选择的帧重新创建；不能仅凭可见标记确定原始范围。"),
    EditorStageOrderHelp => ("editor-stage-order-help", [], "Earlier artwork follows later image operations. Newly added artwork is drawn after existing operations.", "先前的图层内容会经过后续图像操作。新添加的图层内容绘制在已有操作之后。"),
    EditorShapeNameRequired => ("editor-shape-name-required", [], "Shape overlay name is required.", "形状图层名称不能为空。"),
    EditorShapeOpacityRequired => ("editor-shape-opacity-required", [], "Shape track opacity must be greater than zero.", "形状轨道不透明度必须大于零。"),
    EditorShapeBoundsInvalid => ("editor-shape-bounds-invalid", ["error"], "Invalid shape bounds: {error}", "形状边界无效：{error}"),
    EditorShapeBoundsOutside => ("editor-shape-bounds-outside", [], "Shape bounds must stay inside the rendered canvas.", "形状边界必须位于渲染画布内。"),
    EditorLineStrokeRequired => ("editor-line-stroke-required", [], "Line and arrow overlays require a visible positive-width stroke.", "直线和箭头图层需要可见且宽度大于零的描边。"),
    EditorShapeVisibleRequired => ("editor-shape-visible-required", [], "Rectangle and ellipse overlays require a visible stroke or fill.", "矩形和椭圆图层需要可见的描边或填充。"),
    EditorDrawingNameRequired => ("editor-drawing-name-required", [], "Drawing overlay name is required.", "绘图图层名称不能为空。"),
    EditorDrawingVisibleRequired => ("editor-drawing-visible-required", [], "Drawing width, color alpha, and track opacity must be visible.", "笔迹宽度、颜色 Alpha 和轨道不透明度必须保证笔迹可见。"),
    EditorDrawingPointRequired => ("editor-drawing-point-required", [], "Draw at least one point on the preview before committing.", "提交前，请在预览上至少绘制一个点。"),
    EditorDrawingTooManyPoints => ("editor-drawing-too-many-points", ["limit"], "Drawing contains more than {limit} points.", "绘图包含的点数超过 {limit}。"),
    EditorDrawingPressureRange => ("editor-drawing-pressure-range", [], "Drawing pressure must stay in 0..=1000.", "绘图压力值必须在 0..=1000 之间。"),
    EditorOverlayTextTab => ("editor-overlay-text-tab", [], "Text & titles", "文字与标题"),
    EditorOverlayImageTab => ("editor-overlay-image-tab", [], "Image", "图片"),
    EditorOverlayShapeTab => ("editor-overlay-shape-tab", [], "Shape", "形状"),
    EditorOverlayDrawTab => ("editor-overlay-draw-tab", [], "Draw", "绘图"),
    EditorOverlayLayersTab => ("editor-overlay-layers-tab", [], "Layers", "图层"),
    VectorOpen => ("vector-open", [], "Open shape canvas…", "打开形状画布…"),
    VectorShapesTitle => ("vector-shapes-title", [], "Shape canvas", "形状画布"),
    VectorInsertMode => ("vector-insert-mode", [], "Insert", "插入"),
    VectorSelectMode => ("vector-select-mode", [], "Select", "选择"),
    VectorTriangle => ("vector-triangle", [], "Triangle", "三角形"),
    VectorBlockArrow => ("vector-block-arrow", [], "Block arrow", "块状箭头"),
    VectorCornerRadius => ("vector-corner-radius", [], "Corner radius", "圆角半径"),
    VectorRotation => ("vector-rotation", [], "Rotation", "旋转角度"),
    VectorResetRotation => ("vector-reset-rotation", [], "Reset rotation", "重置旋转"),
    VectorSelectAll => ("vector-select-all", [], "Select all shapes", "选择全部形状"),
    VectorDeleteSelected => ("vector-delete-selected", [], "Delete selected shapes", "删除选中形状"),
    VectorClear => ("vector-clear", [], "Clear shapes", "清空形状"),
    VectorApply => ("vector-apply", [], "Apply shapes", "应用形状"),
    VectorClose => ("vector-close", [], "Close shape canvas", "关闭形状画布"),
    VectorHelp => ("vector-help", [], "Draw shapes, or select them to move, resize and rotate. Apply changes the selected frames once.", "绘制形状，或选择形状后移动、缩放与旋转。应用时一次性修改选中帧。"),
    VectorCount => ("vector-count", ["count", "selected"], "{count} shapes · {selected} selected", "{count} 个形状 · 已选择 {selected} 个"),
    VectorStale => ("vector-stale", [], "The project, revision or target selection changed. This draft is kept but cannot be applied; start a new shape canvas.", "工程、修订或目标帧选择已改变。草稿已保留，但无法应用；请重新开始形状画布。"),
    VectorNeedFrames => ("vector-need-frames", [], "Select an original frame and at least one target frame before opening the shape canvas.", "打开形状画布前，请选择一个原始帧和至少一个目标帧。"),
    VectorNoDraft => ("vector-no-draft", [], "Open the shape canvas first.", "请先打开形状画布。"),
    VectorNoObjects => ("vector-no-objects", [], "Draw at least one shape before applying.", "应用前请至少绘制一个形状。"),
    VectorFinishGesture => ("vector-finish-gesture", [], "Finish or cancel the active shape gesture first.", "请先完成或取消当前形状操作。"),
    VectorObjectLimit => ("vector-object-limit", ["limit"], "The shape draft is limited to {limit} objects; nothing was truncated.", "形状草稿最多包含 {limit} 个对象；没有截断任何内容。"),
    VectorIdsExhausted => ("vector-ids-exhausted", [], "Shape identities are exhausted; reopen the shape canvas.", "形状标识已用尽，请重新打开形状画布。"),
    VectorOperationFailed => ("vector-operation-failed", ["error"], "Shape canvas: {error}", "形状画布：{error}"),
    VectorInputLimit => ("vector-input-limit", [], "The shape input batch exceeded its safety limit; the unfinished gesture was cancelled.", "本批形状输入超出安全上限；未完成的操作已取消。"),
    VectorPreviewPending => ("vector-preview-pending", [], "Rendering the current shape draft…", "正在渲染当前形状草稿…"),
    VectorRestartRequired => ("vector-restart-required", [], "Use Restart on current selection to replace this draft.", "请使用“按当前选择重新开始”来替换此草稿。"),
    VectorRestart => ("vector-restart", [], "Restart on current selection", "按当前选择重新开始"),
    VectorApplied => ("vector-applied", [], "Applied shape group to the selected frames.", "已将形状组应用到选中帧。"),
    ExportTitle => ("export-title", [], "Export GIF", "导出 GIF"),
    ExportDestination => ("export-destination", [], "Output path", "输出路径"),
    ExportStart => ("export-start", [], "Export", "导出"),
    ExportWorking => ("export-working", [], "Exporting GIF…", "正在导出 GIF…"),
    ExportFinished => ("export-finished", ["frames", "path"], "Export complete. Frames: {frames}. File: {path}", "导出完成。帧数：{frames}。文件：{path}"),
    ExportFailed => ("export-failed", ["reason"], "GIF export failed: {reason}", "GIF 导出失败：{reason}"),
    ExportCancel => ("export-cancel", [], "Cancel export", "取消导出"),
    ExportCancelled => ("export-cancelled", [], "GIF export cancelled.", "已取消 GIF 导出。"),
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
