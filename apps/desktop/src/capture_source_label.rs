//! Translate application-owned chooser entries, never external source names.

use gif_from_screen_capture::{CaptureSource, CaptureSourceKind};
use gif_from_screen_capture_linux::LinuxDisplayServer;
use gif_from_screen_localization::{Localizer, Message};

pub(crate) fn display_name(
    source: &CaptureSource,
    display: Option<LinuxDisplayServer>,
    localizer: Localizer,
) -> &str {
    // These exact backend-owned IDs describe virtual chooser entries, not a
    // selected monitor/window. Their contract is shared by capture-linux's
    // wayland::WaylandPortalCapabilities::portal_sources and
    // wayland_pipewire::resolve_portal_target. The factory tests below detect a
    // future contract change without using source.name() as an English key.
    let message = match (
        display,
        source.geometry(),
        source.kind(),
        source.id().as_str(),
    ) {
        (
            Some(LinuxDisplayServer::Wayland),
            None,
            CaptureSourceKind::Monitor,
            "wayland:portal:monitor",
        ) => Message::RecorderChoosePortalScreen,
        (
            Some(LinuxDisplayServer::Wayland),
            None,
            CaptureSourceKind::Window,
            "wayland:portal:window",
        ) => Message::RecorderChoosePortalWindow,
        _ => return source.name(),
    };
    localizer.text(message)
}

#[cfg(test)]
mod tests {
    use gif_from_screen_capture::{CaptureSourceId, PhysicalRect};
    use gif_from_screen_capture_linux::WaylandPortalCapabilities;
    use gif_from_screen_localization::find_language;

    use super::*;

    fn localizer(tag: &str) -> Localizer {
        Localizer::new(find_language(tag).unwrap())
    }

    fn portal_sources() -> Vec<CaptureSource> {
        WaylandPortalCapabilities {
            version: 5,
            monitor: true,
            window: true,
            cursor_hidden: true,
            cursor_embedded: true,
            cursor_metadata: false,
        }
        .portal_sources()
        .unwrap()
    }

    fn source(
        id: &str,
        kind: CaptureSourceKind,
        name: &str,
        geometry: Option<PhysicalRect>,
    ) -> CaptureSource {
        CaptureSource::new(CaptureSourceId::new(id).unwrap(), name, kind, geometry, 1.0).unwrap()
    }

    fn assert_original(source: &CaptureSource, display: Option<LinuxDisplayServer>) {
        let before = source.clone();
        for tag in ["en", "zh", "fr"] {
            let shown = display_name(source, display, localizer(tag));
            assert_eq!(shown, source.name());
            assert!(
                std::ptr::eq(shown, source.name()),
                "the original name is borrowed"
            );
            assert_eq!(*source, before);
        }
    }

    #[test]
    fn actual_portal_factory_choices_translate_without_mutating_descriptors() {
        let sources = portal_sources();
        let before = sources.clone();
        assert_eq!(sources.len(), 2);
        for (source, expected_id, expected_kind, expected_label) in [
            (
                &sources[0],
                "wayland:portal:monitor",
                CaptureSourceKind::Monitor,
                "通过系统 Portal 选择显示器",
            ),
            (
                &sources[1],
                "wayland:portal:window",
                CaptureSourceKind::Window,
                "通过系统 Portal 选择窗口",
            ),
        ] {
            assert_eq!(source.id().as_str(), expected_id);
            assert_eq!(source.kind(), expected_kind);
            assert!(source.geometry().is_none());
            assert_eq!(
                display_name(source, Some(LinuxDisplayServer::Wayland), localizer("zh")),
                expected_label
            );
            for tag in ["en", "fr"] {
                assert_eq!(
                    display_name(source, Some(LinuxDisplayServer::Wayland), localizer(tag)),
                    source.name()
                );
            }
        }
        assert_eq!(sources, before);
    }

    #[test]
    fn missing_or_non_wayland_catalog_never_relabels_even_portal_ids() {
        for source in portal_sources() {
            assert_original(&source, None);
            assert_original(&source, Some(LinuxDisplayServer::X11));
        }
    }

    #[test]
    fn unknown_or_prefix_only_ids_and_mismatched_kinds_keep_the_original_name() {
        for (id, kind) in [
            ("wayland:portal:monitor:extra", CaptureSourceKind::Monitor),
            ("wayland:portal:window:extra", CaptureSourceKind::Window),
            ("wayland:stream:42", CaptureSourceKind::Window),
            ("wayland:portal:monitor", CaptureSourceKind::Window),
            ("wayland:portal:window", CaptureSourceKind::Monitor),
        ] {
            let source = source(id, kind, "窗口：用户/{count}/原始名称", None);
            assert_original(&source, Some(LinuxDisplayServer::Wayland));
        }
    }

    #[test]
    fn known_ids_with_geometry_are_not_virtual_chooser_entries() {
        let geometry = Some(PhysicalRect::new(-1920, -20, 720, 480).unwrap());
        for (id, kind) in [
            ("wayland:portal:monitor", CaptureSourceKind::Monitor),
            ("wayland:portal:window", CaptureSourceKind::Window),
        ] {
            let source = source(id, kind, "用户的真实来源", geometry);
            assert_original(&source, Some(LinuxDisplayServer::Wayland));
        }
    }

    #[test]
    fn real_source_renames_and_names_matching_english_catalog_text_stay_literal() {
        for id in ["x11:window:42", "wayland:stream:42"] {
            for name in [
                "Choose a screen with the system portal",
                "Choose a window with the system portal",
                "用户窗口：/home/用户/{width}/日誌 🌍",
                "通过系统 Portal 选择窗口",
                "Renamed window",
            ] {
                let source = source(id, CaptureSourceKind::Window, name, None);
                // Check both catalogs: the ID/type contract, not an English
                // string or a broad backend prefix, decides the display label.
                for display in [LinuxDisplayServer::X11, LinuxDisplayServer::Wayland] {
                    assert_original(&source, Some(display));
                }
            }
        }
    }

    #[test]
    fn virtual_identity_does_not_depend_on_the_backend_description_spelling() {
        for original in portal_sources() {
            let renamed = source(
                original.id().as_str(),
                original.kind(),
                "Backend wording changed {user}",
                None,
            );
            let before = renamed.clone();
            assert_eq!(
                display_name(&renamed, Some(LinuxDisplayServer::Wayland), localizer("zh")),
                display_name(
                    &original,
                    Some(LinuxDisplayServer::Wayland),
                    localizer("zh")
                )
            );
            assert_eq!(renamed, before);
            assert_eq!(renamed.name(), "Backend wording changed {user}");
        }
    }
}
