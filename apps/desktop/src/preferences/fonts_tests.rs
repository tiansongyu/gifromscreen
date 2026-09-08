use std::borrow::Cow;

use eframe::egui::{self, Color32, FontDefinitions, FontFamily, FontId};
use sha2::{Digest, Sha256};

use super::{FONT_BYTES, FONT_NAME, definitions, install};

const CJK_SAMPLES: [&str; 4] = [
    "简体中文设置录制暂停停止保存图像选择窗口截取鼠标键盘时间区域",
    "繁體中文設定錄製暫停停止儲存圖像選擇視窗擷取滑鼠鍵盤時間區域",
    "日本語設定録画一時停止保存画像選択ウィンドウマウスキーボード時間領域",
    "한국어설정녹화일시정지저장이미지선택창마우스키보드시간영역",
];

#[test]
fn embedded_font_and_original_license_match_recorded_sha256() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../assets/fonts/sources.json")).unwrap();
    let font = &manifest["files"][0];
    assert_eq!(FONT_BYTES.len(), 16_437_364);
    assert!(FONT_BYTES.len() < 20_000_000);
    assert_eq!(font["bytes"].as_u64().unwrap(), FONT_BYTES.len() as u64);
    assert_eq!(font["face_index"], 0);
    assert_eq!(font["modified"], false);
    assert_eq!(
        format!("{:x}", Sha256::digest(FONT_BYTES)),
        "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b"
    );
    assert_eq!(font["sha256"], format!("{:x}", Sha256::digest(FONT_BYTES)));
    let license = include_bytes!("../../assets/fonts/OFL.txt");
    assert_eq!(license.len(), 4301);
    assert_eq!(
        format!("{:x}", Sha256::digest(license)),
        "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2"
    );
    assert_eq!(
        manifest["files"][1]["sha256"],
        format!("{:x}", Sha256::digest(license))
    );
}

#[test]
fn selected_face_cmap_contains_cjk_samples_kana_and_modern_hangul() {
    let face = ttf_parser::Face::parse(FONT_BYTES, 0).unwrap();
    assert_eq!(face.number_of_glyphs(), 65_535);
    assert_eq!(face.units_per_em(), 1000);
    for character in CJK_SAMPLES.into_iter().flat_map(str::chars) {
        assert!(face.glyph_index(character).is_some(), "missing {character}");
    }
    // Hangul syllables and the basic kana ranges are bounded, exact cmap checks,
    // not a claim about shaping, locale-specific Han forms or all Unicode.
    for point in (0xAC00..=0xD7A3)
        .chain(0x3041..=0x3096)
        .chain(0x30A1..=0x30FA)
    {
        let character = char::from_u32(point).unwrap();
        assert!(
            face.glyph_index(character).is_some(),
            "missing U+{point:04X}"
        );
    }
    let copyrights: Vec<_> = face
        .names()
        .into_iter()
        .filter(|name| name.name_id == ttf_parser::name_id::COPYRIGHT_NOTICE)
        .filter_map(|name| name.to_string())
        .collect();
    assert!(
        copyrights
            .iter()
            .any(|notice| { notice == "© 2014-2021 Adobe (http://www.adobe.com/)." })
    );
}

#[test]
fn fallback_preserves_all_default_font_data_and_family_priorities() {
    let original = FontDefinitions::default();
    let extended = definitions();
    assert_eq!(extended.font_data.len(), original.font_data.len() + 1);
    for (name, data) in original.font_data {
        assert_eq!(extended.font_data[&name], data);
    }
    for (family, names) in original.families {
        let expected = names
            .into_iter()
            .chain(std::iter::once(FONT_NAME.to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(extended.families[&family], expected);
    }
    let fallback = &extended.font_data[FONT_NAME];
    assert_eq!(fallback.index, 0);
    assert!(matches!(fallback.font, Cow::Borrowed(_)));
    assert!(std::ptr::eq(fallback.font.as_ptr(), FONT_BYTES.as_ptr()));
}

#[test]
fn real_egui_fallback_loads_cjk_in_both_ui_font_families() {
    let context = egui::Context::default();
    install(&context);
    let output = context.run(egui::RawInput::default(), |context| {
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let font = FontId::new(18.0, family);
            context.fonts(|fonts| {
                for sample in CJK_SAMPLES {
                    assert!(
                        fonts.has_glyphs(&font, sample),
                        "missing UI glyph: {sample}"
                    );
                    let galley =
                        fonts.layout_no_wrap(sample.to_owned(), font.clone(), Color32::WHITE);
                    assert!(galley.size().x > 0.0 && galley.size().y > 0.0);
                    assert!(galley.num_vertices > 0);
                }
            });
        }
    });
    assert!(!output.textures_delta.set.is_empty());
}

#[test]
fn fallback_does_not_change_default_latin_emoji_layout_or_atlas_pixels() {
    let defaults = egui::Context::default();
    let extended = egui::Context::default();
    install(&extended);
    let samples = ["Save GIF · 0123456789 ABC xyz", "☀ ♫ ♥ 😀"];
    let mut reference = Vec::new();
    let mut reference_atlas = None;
    let _ = defaults.run(egui::RawInput::default(), |context| {
        context.fonts(|fonts| {
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let font = FontId::new(18.0, family);
                for sample in samples {
                    assert!(fonts.has_glyphs(&font, sample));
                    reference.push(fonts.layout_no_wrap(
                        sample.to_owned(),
                        font.clone(),
                        Color32::WHITE,
                    ));
                }
            }
            reference_atlas = Some(fonts.image());
        });
    });
    let _ = extended.run(egui::RawInput::default(), |context| {
        context.fonts(|fonts| {
            let mut index = 0;
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let font = FontId::new(18.0, family);
                for sample in samples {
                    let galley =
                        fonts.layout_no_wrap(sample.to_owned(), font.clone(), Color32::WHITE);
                    assert_eq!(galley, reference[index]);
                    index += 1;
                }
            }
            assert_eq!(fonts.image(), reference_atlas.clone().unwrap());
        });
    });
}

#[test]
fn installation_keeps_native_scale_and_user_zoom_unchanged() {
    let context = egui::Context::default();
    context.set_zoom_factor(1.25);
    let mut input = egui::RawInput::default();
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(2.0);
    let _ = context.run(input.clone(), |_| {});
    let before_ppp = context.pixels_per_point();
    let before_zoom = context.zoom_factor();
    install(&context);
    let _ = context.run(input, |context| {
        assert_eq!(context.pixels_per_point().to_bits(), before_ppp.to_bits());
        assert_eq!(context.zoom_factor().to_bits(), before_zoom.to_bits());
        context.fonts(|fonts| {
            assert_eq!(fonts.pixels_per_point().to_bits(), before_ppp.to_bits());
            assert!(fonts.has_glyphs(&FontId::proportional(18.0), CJK_SAMPLES[0]));
        });
    });
}
