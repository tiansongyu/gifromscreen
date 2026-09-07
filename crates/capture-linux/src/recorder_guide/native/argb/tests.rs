use x11rb::protocol::{
    render::{Directformat, Pictdepth, Pictscreen, Pictvisual},
    xproto::Depth,
};

use super::*;

fn fixture() -> (Screen, QueryPictFormatsReply) {
    let visual = Visualtype {
        visual_id: 42,
        class: VisualClass::TRUE_COLOR,
        red_mask: 0x00ff_0000,
        green_mask: 0x0000_ff00,
        blue_mask: 0x0000_00ff,
        ..Visualtype::default()
    };
    let screen = Screen {
        root_visual: 1,
        allowed_depths: vec![Depth {
            depth: 32,
            visuals: vec![visual],
        }],
        ..Screen::default()
    };
    let formats = QueryPictFormatsReply {
        formats: vec![Pictforminfo {
            id: 99,
            depth: 32,
            type_: PictType::DIRECT,
            direct: Directformat {
                red_shift: 16,
                red_mask: 255,
                green_shift: 8,
                green_mask: 255,
                blue_shift: 0,
                blue_mask: 255,
                alpha_shift: 24,
                alpha_mask: 255,
            },
            ..Pictforminfo::default()
        }],
        screens: vec![Pictscreen {
            depths: vec![Pictdepth {
                depth: 32,
                visuals: vec![Pictvisual {
                    visual: 42,
                    format: 99,
                }],
            }],
            ..Pictscreen::default()
        }],
        ..QueryPictFormatsReply::default()
    };
    (screen, formats)
}

#[test]
fn selection_uses_render_alpha_not_root_visual_or_a_guessed_high_byte() {
    let (mut screen, mut formats) = fixture();
    let selected = select(&screen, 0, &formats).unwrap();
    assert_eq!(selected.visual, 42);
    assert_eq!(selected.pixel, 0xfff2_994a);
    let direct = &mut formats.formats[0].direct;
    direct.red_shift = 0;
    direct.green_shift = 24;
    direct.blue_shift = 8;
    direct.alpha_shift = 16;
    let visual = &mut screen.allowed_depths[0].visuals[0];
    visual.red_mask = 0x0000_00ff;
    visual.green_mask = 0xff00_0000;
    visual.blue_mask = 0x0000_ff00;
    assert_eq!(select(&screen, 0, &formats).unwrap().pixel, 0x99ff_4af2);
}

#[test]
fn selection_rejects_no_alpha_indexed_wrong_depth_or_other_screen_without_rgb_fallback() {
    let (screen, formats) = fixture();
    for mutate in [
        |reply: &mut QueryPictFormatsReply| reply.formats[0].direct.alpha_mask = 0,
        |reply: &mut QueryPictFormatsReply| reply.formats[0].type_ = PictType::INDEXED,
        |reply: &mut QueryPictFormatsReply| reply.formats[0].depth = 24,
        |reply: &mut QueryPictFormatsReply| reply.screens[0].depths[0].depth = 24,
        |reply: &mut QueryPictFormatsReply| reply.screens[0].depths[0].visuals[0].visual = 77,
        |reply: &mut QueryPictFormatsReply| reply.screens[0].depths.clear(),
    ] {
        let mut bad = formats.clone();
        mutate(&mut bad);
        assert!(
            select(&screen, 0, &bad)
                .unwrap_err()
                .contains("alpha visual")
        );
    }
    assert!(select(&screen, 1, &formats).is_err());
    let mut wrong = screen;
    wrong.allowed_depths[0].visuals[0].class = VisualClass::DIRECT_COLOR;
    assert!(select(&wrong, 0, &formats).is_err());
}

#[test]
fn malformed_masks_cannot_alias_alpha_and_color_or_overflow_the_pixel() {
    let (screen, formats) = fixture();
    for mutate in [
        |format: &mut Pictforminfo| format.direct.alpha_shift = 16,
        |format: &mut Pictforminfo| format.direct.alpha_shift = 32,
        |format: &mut Pictforminfo| format.direct.alpha_mask = 256,
        |format: &mut Pictforminfo| format.direct.alpha_mask = 511,
        |format: &mut Pictforminfo| format.direct.red_shift = 0,
        |format: &mut Pictforminfo| format.direct.blue_mask = 0,
    ] {
        let mut bad = formats.clone();
        mutate(&mut bad.formats[0]);
        assert!(select(&screen, 0, &bad).is_err());
    }
    let mut valid = formats;
    valid.formats.insert(0, Pictforminfo::default());
    assert_eq!(select(&screen, 0, &valid).unwrap().pixel, 0xfff2_994a);
}

#[test]
fn unequal_channel_widths_are_scaled_and_the_entire_alpha_field_is_set() {
    let (mut screen, mut formats) = fixture();
    formats.formats[0].direct = Directformat {
        red_shift: 20,
        red_mask: 1023,
        green_shift: 10,
        green_mask: 1023,
        blue_shift: 0,
        blue_mask: 1023,
        alpha_shift: 30,
        alpha_mask: 3,
    };
    let visual = &mut screen.allowed_depths[0].visuals[0];
    visual.red_mask = 1023 << 20;
    visual.green_mask = 1023 << 10;
    visual.blue_mask = 1023;
    let pixel = select(&screen, 0, &formats).unwrap().pixel;
    assert_eq!(pixel >> 30, 3);
    assert_eq!((pixel >> 20) & 1023, (242 * 1023 + 127) / 255);
    assert_eq!((pixel >> 10) & 1023, (153 * 1023 + 127) / 255);
    assert_eq!(pixel & 1023, (74 * 1023 + 127) / 255);
}
