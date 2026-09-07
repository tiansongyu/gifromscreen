use std::{thread, time::Duration};

use gif_from_screen_capture::{PhysicalPosition, PhysicalRect, PhysicalSize};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        shape::{ConnectionExt as _, SK},
        xfixes::ConnectionExt as _,
        xproto::{
            ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
            EventMask, Rectangle, StackMode, Window, WindowClass,
        },
    },
    wrapper::ConnectionExt as _,
};

use super::{
    Context, GuideAck, GuideEdge, GuidePointerEvent, GuideRequest,
    geometry::{self, Strip},
};
use crate::shortcuts::x11::{Client, stream};

pub(super) fn run(display: Option<&str>, context: &Context) -> Result<(), String> {
    let parsed = x11rb_protocol::parse_display::parse_display(display).map_err(native_error)?;
    let connection = stream::connect(display, &context.cancel)?;
    let version = connection
        .xfixes_query_version(5, 0)
        .map_err(native_error)?
        .reply()
        .map_err(native_error)?;
    let shape = connection
        .shape_query_version()
        .map_err(native_error)?
        .reply()
        .map_err(native_error)?;
    if version.major_version < 3 || (shape.major_version, shape.minor_version) < (1, 1) {
        return Err("Recorder guides require XFixes 3 and SHAPE 1.1.".into());
    }
    let screen = connection
        .setup()
        .roots
        .get(usize::from(parsed.screen))
        .ok_or("The selected X11 screen is unavailable.")?;
    let root_size = PhysicalSize::new(
        u32::from(screen.width_in_pixels),
        u32::from(screen.height_in_pixels),
    )
    .map_err(native_error)?;
    let color = root_color(screen)?;
    // Select only size/lifetime notifications for THIS client. No root pointer
    // or keyboard feed is installed, and other clients' masks are unaffected.
    connection
        .change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .map_err(native_error)?
        .check()
        .map_err(native_error)?;
    let mut owned = Windows {
        connection: &connection,
        root: screen.root,
        root_size,
        ids: Vec::with_capacity(4),
        current: None,
        visible: [false; 4],
        event_floor: 0,
        pressed: None,
    };
    for _ in 0..4 {
        owned.create(color)?;
    }
    context.connected();
    connection.stream().registration_complete();
    let result = (|| {
        while !context.cancelled() {
            if let Some(request) = context.request() {
                connection.stream().begin_operation();
                let visible = owned.apply(request, context)?;
                connection.stream().registration_complete();
                context.acknowledge(GuideAck {
                    generation: request.generation,
                    visible,
                });
            }
            owned.events(context)?;
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    drop(owned); // Bounded cleanup before the terminal result is observable.
    drop(connection);
    if context.cancelled() { Ok(()) } else { result }
}

struct Windows<'a, 'c> {
    connection: &'a Client<'c>,
    root: Window,
    root_size: PhysicalSize,
    ids: Vec<Window>,
    current: Option<GuideRequest>,
    visible: [bool; 4],
    event_floor: u64,
    pressed: Option<u64>,
}

impl Windows<'_, '_> {
    fn create(&mut self, color: u32) -> Result<(), String> {
        let id = self.connection.generate_id().map_err(native_error)?;
        self.connection
            .create_window(
                0,
                id,
                self.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(color)
                    .event_mask(
                        EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::POINTER_MOTION
                            | EventMask::STRUCTURE_NOTIFY,
                    ),
            )
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        self.ids.push(id);
        Ok(())
    }

    fn apply(&mut self, request: GuideRequest, context: &Context) -> Result<bool, String> {
        self.check_root()?;
        self.cancel_gesture(context);
        for id in &self.ids {
            self.connection
                .unmap_window(*id)
                .map_err(native_error)?
                .check()
                .map_err(native_error)?;
        }
        self.visible = [false; 4];
        let region = request
            .region
            .filter(|region| !geometry::covers_root(*region, self.root_size));
        if let Some(region) = region {
            let root = PhysicalRect::new(0, 0, self.root_size.width(), self.root_size.height())
                .map_err(native_error)?;
            for (index, strip) in geometry::strips(region, request.border_width)?
                .iter()
                .enumerate()
            {
                if context.cancelled() {
                    return Err("Recorder guide update cancelled.".into());
                }
                let visible = geometry::intersection(strip.rect, root);
                let excluded = visible.and_then(|visible| {
                    request
                        .protected_region
                        .and_then(|protected| geometry::intersection(visible, protected))
                });
                self.configure(index, *strip, visible, excluded)?;
                self.visible[index] = visible.is_some() && visible != excluded;
            }
        } else {
            for id in &self.ids {
                self.shapes(*id, None, None)?;
            }
        }
        if context.cancelled() {
            return Err("Recorder guide update cancelled.".into());
        }
        for (index, id) in self.ids.iter().enumerate() {
            if self.visible[index] {
                self.connection
                    .map_window(*id)
                    .map_err(native_error)?
                    .check()
                    .map_err(native_error)?;
                self.connection
                    .configure_window(*id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))
                    .map_err(native_error)?
                    .check()
                    .map_err(native_error)?;
            }
        }
        let barrier = self.connection.get_input_focus().map_err(native_error)?;
        self.event_floor = barrier.sequence_number();
        barrier.reply().map_err(native_error)?;
        self.check_root()?;
        self.current = Some(request);
        Ok(self.visible.iter().any(|visible| *visible))
    }

    fn configure(
        &self,
        index: usize,
        strip: Strip,
        visible: Option<PhysicalRect>,
        excluded: Option<PhysicalRect>,
    ) -> Result<(), String> {
        let id = self.ids[index];
        self.connection
            .configure_window(
                id,
                &ConfigureWindowAux::new()
                    .x(strip.rect.origin().x)
                    .y(strip.rect.origin().y)
                    .width(strip.rect.size().width())
                    .height(strip.rect.size().height()),
            )
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        self.shapes(
            id,
            visible.map(|rect| local(rect, strip.rect)).transpose()?,
            excluded.map(|rect| local(rect, strip.rect)).transpose()?,
        )?;
        let actual = self
            .connection
            .get_geometry(id)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        if i32::from(actual.x) != strip.rect.origin().x
            || i32::from(actual.y) != strip.rect.origin().y
            || u32::from(actual.width) != strip.rect.size().width()
            || u32::from(actual.height) != strip.rect.size().height()
        {
            return Err("The X11 server did not accept the requested guide geometry.".into());
        }
        Ok(())
    }

    fn shapes(
        &self,
        id: Window,
        visible: Option<Rectangle>,
        excluded: Option<Rectangle>,
    ) -> Result<(), String> {
        let base = Region::new(self.connection, visible)?;
        if let Some(excluded) = excluded {
            let hole = Region::new(self.connection, Some(excluded))?;
            self.connection
                .xfixes_subtract_region(base.id, hole.id, base.id)
                .map_err(native_error)?
                .check()
                .map_err(native_error)?;
        }
        // Both changes happen while this owned window is unmapped. Its center
        // never relies on alpha, a compositor effect, or an input-only hole.
        for kind in [SK::BOUNDING, SK::INPUT] {
            self.connection
                .xfixes_set_window_shape_region(id, kind, 0, 0, base.id)
                .map_err(native_error)?
                .check()
                .map_err(native_error)?;
        }
        Ok(())
    }

    fn cancel_gesture(&mut self, context: &Context) {
        if let Some(generation) = self.pressed.take() {
            context.event(GuidePointerEvent::Cancelled { generation });
        }
    }

    fn check_root(&self) -> Result<(), String> {
        let root = self
            .connection
            .get_geometry(self.root)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        check_root_dimensions(root.width, root.height, self.root_size)
    }

    fn events(&mut self, context: &Context) -> Result<(), String> {
        for _ in 0..128 {
            if context.cancelled() {
                break;
            }
            let Some((event, sequence)) = self
                .connection
                .poll_for_event_with_sequence()
                .map_err(native_error)?
            else {
                break;
            };
            // The sequence floor filters stale POINTER coordinates only. Never
            // discard a root/lifetime failure that happened during an update.
            match &event {
                Event::ConfigureNotify(event)
                    if event.response_type & 0x80 == 0 && event.window == self.root =>
                {
                    check_root_dimensions(event.width, event.height, self.root_size)?;
                }
                Event::DestroyNotify(event) if self.ids.contains(&event.window) => {
                    return Err("An owned recorder guide window disappeared.".into());
                }
                Event::Error(_) => {
                    return Err("The X11 server rejected a recorder guide request.".into());
                }
                _ => {}
            }
            if sequence < self.event_floor {
                continue;
            }
            let Some(request) = self.current else {
                continue;
            };
            match event {
                Event::ButtonPress(event)
                    if event.response_type & 0x80 == 0 && event.detail == 1 =>
                {
                    let Some(index) = self.ids.iter().position(|id| *id == event.event) else {
                        continue;
                    };
                    if !self.visible[index] || event.root != self.root {
                        continue;
                    }
                    let Some(region) = request.region else {
                        continue;
                    };
                    let position = PhysicalPosition {
                        x: i32::from(event.root_x),
                        y: i32::from(event.root_y),
                    };
                    let edge = hit_edge(index, position, region);
                    self.pressed = Some(request.generation);
                    context.event(GuidePointerEvent::Pressed {
                        generation: request.generation,
                        position,
                        edge,
                        modifiers: u16::from(event.state) & 0xff,
                    });
                }
                Event::MotionNotify(event)
                    if event.response_type & 0x80 == 0 && self.ids.contains(&event.event) =>
                {
                    if event.root != self.root {
                        self.cancel_gesture(context);
                        continue;
                    }
                    if !self.visible.iter().any(|visible| *visible) {
                        continue;
                    }
                    context.event(GuidePointerEvent::Moved {
                        generation: request.generation,
                        position: PhysicalPosition {
                            x: i32::from(event.root_x),
                            y: i32::from(event.root_y),
                        },
                    });
                }
                Event::ButtonRelease(event)
                    if event.response_type & 0x80 == 0
                        && event.detail == 1
                        && self.ids.contains(&event.event) =>
                {
                    if let Some(generation) = self.pressed.take() {
                        context.event(GuidePointerEvent::Released {
                            generation,
                            position: PhysicalPosition {
                                x: i32::from(event.root_x),
                                y: i32::from(event.root_y),
                            },
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl Drop for Windows<'_, '_> {
    fn drop(&mut self) {
        self.connection.stream().begin_cleanup();
        for id in &self.ids {
            let _ = self.connection.destroy_window(*id);
        }
        let _ = self.connection.flush();
        let _ = self.connection.sync();
    }
}

struct Region<'a, 'c> {
    connection: &'a Client<'c>,
    id: u32,
}
impl<'a, 'c> Region<'a, 'c> {
    fn new(connection: &'a Client<'c>, rectangle: Option<Rectangle>) -> Result<Self, String> {
        let id = connection.generate_id().map_err(native_error)?;
        connection
            .xfixes_create_region(id, rectangle.as_slice())
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        Ok(Self { connection, id })
    }
}
impl Drop for Region<'_, '_> {
    fn drop(&mut self) {
        let _ = self.connection.xfixes_destroy_region(self.id);
    }
}

fn local(rect: PhysicalRect, strip: PhysicalRect) -> Result<Rectangle, String> {
    Ok(Rectangle {
        x: i16::try_from(i64::from(rect.origin().x) - i64::from(strip.origin().x))
            .map_err(native_error)?,
        y: i16::try_from(i64::from(rect.origin().y) - i64::from(strip.origin().y))
            .map_err(native_error)?,
        width: u16::try_from(rect.size().width()).map_err(native_error)?,
        height: u16::try_from(rect.size().height()).map_err(native_error)?,
    })
}

fn hit_edge(index: usize, point: PhysicalPosition, region: PhysicalRect) -> GuideEdge {
    let corner = i64::from(region.size().width().min(region.size().height()).min(32)) / 2;
    let left = i64::from(point.x) < i64::from(region.origin().x) + corner;
    let right = i64::from(point.x)
        >= i64::from(region.origin().x) + i64::from(region.size().width()) - corner;
    let top = i64::from(point.y) < i64::from(region.origin().y) + corner;
    let bottom = i64::from(point.y)
        >= i64::from(region.origin().y) + i64::from(region.size().height()) - corner;
    match index {
        0 if left => GuideEdge::TopLeft,
        0 if right => GuideEdge::TopRight,
        0 => GuideEdge::Top,
        1 if left => GuideEdge::BottomLeft,
        1 if right => GuideEdge::BottomRight,
        1 => GuideEdge::Bottom,
        2 if top => GuideEdge::TopLeft,
        2 if bottom => GuideEdge::BottomLeft,
        2 => GuideEdge::Left,
        _ if top => GuideEdge::TopRight,
        _ if bottom => GuideEdge::BottomRight,
        _ => GuideEdge::Right,
    }
}

fn root_color(screen: &x11rb::protocol::xproto::Screen) -> Result<u32, String> {
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| &depth.visuals)
        .find(|visual| visual.visual_id == screen.root_visual)
        .ok_or("The X11 root visual is unavailable.")?;
    let mut pixel = 0;
    for (value, mask) in [
        (242_u32, visual.red_mask),
        (153, visual.green_mask),
        (74, visual.blue_mask),
    ] {
        if mask == 0 {
            return Err("Recorder guides require an RGB root visual.".into());
        }
        let shift = mask.trailing_zeros();
        let range = mask >> shift;
        if range & range.wrapping_add(1) != 0 {
            return Err("Recorder guide root RGB masks are unsupported.".into());
        }
        let channel = (u64::from(value) * u64::from(range) + 127) / 255;
        pixel |= u32::try_from(channel).map_err(native_error)? << shift;
    }
    Ok(pixel)
}

fn native_error(error: impl std::fmt::Display) -> String {
    format!("Recorder guide native operation failed: {error}")
}

fn check_root_dimensions(width: u16, height: u16, expected: PhysicalSize) -> Result<(), String> {
    if u32::from(width) != expected.width() || u32::from(height) != expected.height() {
        return Err("The X11 root dimensions changed. Recorder guides stopped; reselect the source before creating another guide.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_geometry_change_requires_reselection_instead_of_stale_clipping() {
        let expected = PhysicalSize::new(400, 300).unwrap();
        assert!(check_root_dimensions(400, 300, expected).is_ok());
        assert!(
            check_root_dimensions(399, 300, expected)
                .unwrap_err()
                .contains("reselect")
        );
        assert!(check_root_dimensions(400, 301, expected).is_err());
    }
}
