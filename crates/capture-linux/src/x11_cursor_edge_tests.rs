//! Regression for X server's inclusive cursor-hotspot creation boundary.
//!
//! Core and Render creation reject only `x > width || y > height`:
//! <https://github.com/XQuartz/xorg-server/blob/master/dix/dispatch.c>
//! <https://github.com/XQuartz/xorg-server/blob/master/render/render.c>
//! `xfixes/cursor.c` returns that stored hotspot without clamping it. This
//! isolated reproduction does not identify the cause of any earlier transient
//! host-display failure whose cursor geometry was not retained.

use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use gif_from_screen_capture::{CaptureCadence, CaptureSourceKind, CursorCaptureMode, FramePoll};
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::ConnectionExt as _,
        xproto::{
            ChangeWindowAttributesAux, ConnectionExt as _, CreateGCAux, MOTION_NOTIFY_EVENT,
            Rectangle,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

use super::*;

pub(super) struct PrivateXvfb {
    child: Arc<Mutex<Child>>,
    pub(super) display: String,
    watchdog_stop: Option<mpsc::SyncSender<()>>,
    watchdog: Option<thread::JoinHandle<()>>,
}

impl PrivateXvfb {
    pub(super) fn start() -> Self {
        let mut child = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "320x240x24",
                "-nolisten",
                "tcp",
                "-ac",
                "-noreset",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("install Xvfb for this explicit private-display regression");
        let stdout = child.stdout.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        let (stop, stopped) = mpsc::sync_channel(1);
        let owned_child = Arc::clone(&child);
        let watchdog = thread::spawn(move || {
            if stopped.recv_timeout(Duration::from_secs(20)).is_err() {
                let mut child = owned_child.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
            }
        });
        // RAII ownership exists before parsing startup output, so every failure
        // path (including malformed display numbers) reaps only this server.
        let mut server = Self {
            child,
            display: String::new(),
            watchdog_stop: Some(stop),
            watchdog: Some(watchdog),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout.take(32))
                .read_line(&mut line)
                .map(|_| line);
            let _ = sender.send(result);
        });
        let line = match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(line)) => line,
            error => {
                server.stop();
                reader.join().unwrap();
                panic!("private Xvfb startup failed: {error:?}");
            }
        };
        reader.join().unwrap();
        let number = line
            .trim()
            .parse::<u16>()
            .expect("private Xvfb must return its chosen display number");
        server.display = format!(":{number}");
        server
    }

    fn stop(&mut self) {
        if let Some(stop) = self.watchdog_stop.take() {
            let _ = stop.send(());
        }
        {
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.join().unwrap();
        }
    }
}

impl Drop for PrivateXvfb {
    fn drop(&mut self) {
        self.stop();
    }
}

fn install_edge_cursor(connection: &RustConnection, root: u32) {
    let bitmap = connection.generate_id().unwrap();
    connection
        .create_pixmap(1, bitmap, root, 1, 1)
        .unwrap()
        .check()
        .unwrap();
    let gc = connection.generate_id().unwrap();
    connection
        .create_gc(gc, bitmap, &CreateGCAux::new().foreground(1))
        .unwrap()
        .check()
        .unwrap();
    connection
        .poly_fill_rectangle(
            bitmap,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
        )
        .unwrap()
        .check()
        .unwrap();
    let cursor = connection.generate_id().unwrap();
    // A solid red one-pixel core cursor with its hotspot at the outer corner.
    connection
        .create_cursor(cursor, bitmap, bitmap, u16::MAX, 0, 0, 0, 0, 0, 1, 1)
        .unwrap()
        .check()
        .expect("X server accepts hotspot equal to the source dimensions");
    connection
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .background_pixel(0)
                .cursor(cursor),
        )
        .unwrap()
        .check()
        .unwrap();
    connection
        .clear_area(false, root, 0, 0, 0, 0)
        .unwrap()
        .check()
        .unwrap();
    connection
        .xtest_fake_input(MOTION_NOTIFY_EVENT, 0, x11rb::CURRENT_TIME, root, 16, 16, 0)
        .unwrap()
        .check()
        .unwrap();
    connection
        .xfixes_query_version(5, 0)
        .unwrap()
        .reply()
        .unwrap();
    let geometry = connection
        .xfixes_get_cursor_image()
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (
            geometry.width,
            geometry.height,
            geometry.xhot,
            geometry.yhot
        ),
        (1, 1, 1, 1)
    );
    // Known fixture only; never query or operate on the ambient host DISPLAY.
    assert_eq!((geometry.x, geometry.y), (16, 16));
    connection.free_gc(gc).unwrap().check().unwrap();
    connection.free_pixmap(bitmap).unwrap().check().unwrap();
    connection.free_cursor(cursor).unwrap().check().unwrap();
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_edge_hotspot_retains_metadata_and_embeds_at_unclamped_origin() {
    let server = PrivateXvfb::start();
    let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
    install_edge_cursor(&connection, connection.setup().roots[screen].root);
    let backend = X11CaptureBackend::connect(Some(&server.display)).unwrap();
    assert!(backend.capabilities().global_shortcuts.is_ready());
    let source = backend
        .list_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.kind() == CaptureSourceKind::Monitor)
        .unwrap();
    let target = CaptureTarget::Region {
        source: source.id().clone(),
        region: PhysicalRect::new(12, 12, 8, 8).unwrap(),
    };
    for mode in [
        CursorCaptureMode::Metadata,
        CursorCaptureMode::Embedded,
        CursorCaptureMode::Automatic,
    ] {
        let mut request = CaptureRequest::new(target.clone(), CaptureCadence::Manual);
        request.cursor = mode;
        let mut session = backend.start_session(request).unwrap();
        let FramePoll::Frame(frame) = session
            .poll_frame(Duration::ZERO)
            .expect("server-accepted edge hotspot must not abort capture")
        else {
            panic!("manual capture must emit a fixture frame");
        };
        let metadata = frame.cursor().unwrap();
        assert_eq!(metadata.hotspot, PhysicalPosition { x: 1, y: 1 });
        assert_eq!(metadata.position, PhysicalPosition { x: 4, y: 4 });
        assert!(metadata.visible);
        assert_eq!(
            frame.cursor_image().unwrap().size(),
            PhysicalSize::new(1, 1).unwrap()
        );
        let embedded = mode != CursorCaptureMode::Metadata;
        assert_eq!(frame.cursor_embedded(), embedded);
        // The image occupies (pointer - hotspot)=(3,3), not a clamped (4,4).
        let pixel = &frame.pixels()[(3 * 8 + 3) * 4..][..4];
        assert_eq!(
            pixel,
            if embedded {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 0, 255]
            }
        );
        assert_eq!(&frame.pixels()[(4 * 8 + 4) * 4..][..4], &[0, 0, 0, 255]);
        session.stop().unwrap();
    }
}
