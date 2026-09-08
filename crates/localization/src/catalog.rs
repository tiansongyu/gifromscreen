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
