//! A scoped native video buffer and validated access to its SPA metadata.

use std::{mem::size_of, ptr::NonNull};

use gif_from_screen_capture::CaptureError;
use libspa::data::Data;
use pipewire::stream::Stream;

use super::video_crop::VideoCrop;

const MAX_METADATA_ENTRIES: u32 = 64;

/// Owns one dequeued buffer until every borrowed plane has been released.
pub(super) struct NativeVideoBuffer<'a> {
    stream: &'a Stream<()>,
    pointer: NonNull<pipewire_sys::pw_buffer>,
}

impl<'a> NativeVideoBuffer<'a> {
    /// Dequeues a buffer and arranges to return it to the same stream on drop.
    pub(super) fn dequeue(stream: &'a Stream<()>) -> Option<Self> {
        // SAFETY: the stream is live for the guard's lifetime. A null result is
        // handled here, and Drop returns each non-null result exactly once.
        #[allow(unsafe_code)]
        let pointer = unsafe { stream.dequeue_raw_buffer() };
        NonNull::new(pointer).map(|pointer| Self { stream, pointer })
    }

    /// Copies optional visible-content metadata while this buffer is owned.
    ///
    /// # Errors
    /// Returns an invalid-frame error for a malformed native buffer or crop
    /// metadata table/payload.
    pub(super) fn video_crop(&self) -> Result<Option<VideoCrop>, CaptureError> {
        let buffer = self.spa_buffer()?;
        // SAFETY: the guard owns the dequeued buffer and keeps the stream and
        // its native allocations alive. No references to metadata escape.
        #[allow(unsafe_code)]
        unsafe {
            read_video_crop(buffer)
        }
    }

    /// Borrows the sole packed-video plane without copying mapped pixels.
    ///
    /// # Errors
    /// Returns an invalid-frame error if the native buffer does not contain
    /// exactly one aligned data plane with a valid chunk pointer.
    pub(super) fn plane_mut(&mut self) -> Result<&mut Data, CaptureError> {
        let buffer = self.spa_buffer()?;
        if buffer.n_datas != 1 {
            return Err(CaptureError::invalid_frame(
                "PipeWire packed video buffer must contain exactly one data plane",
            ));
        }
        if buffer.datas.is_null() || !buffer.datas.is_aligned() {
            return Err(CaptureError::invalid_frame(
                "PipeWire video data plane pointer is null or misaligned",
            ));
        }
        // SAFETY: the native buffer advertises one live plane; its pointer was
        // checked for null and alignment, and the guard prevents requeueing.
        #[allow(unsafe_code)]
        let chunk = unsafe { (*buffer.datas).chunk };
        if chunk.is_null() || !chunk.is_aligned() {
            return Err(CaptureError::invalid_frame(
                "PipeWire video chunk pointer is null or misaligned",
            ));
        }
        // SAFETY: libspa 0.6 Data is repr(transparent) over spa_data. This
        // guard exclusively owns the dequeued plane; the returned borrow is
        // tied to &mut self and ends before the buffer can be requeued.
        #[allow(unsafe_code)]
        unsafe {
            Ok(&mut *buffer.datas.cast::<Data>())
        }
    }

    fn spa_buffer(&self) -> Result<&libspa_sys::spa_buffer, CaptureError> {
        if !self.pointer.as_ptr().is_aligned() {
            return Err(CaptureError::invalid_frame(
                "PipeWire buffer pointer is misaligned",
            ));
        }
        // SAFETY: this non-null, aligned pointer came from this live stream's
        // dequeue operation and remains owned until the guard is dropped.
        #[allow(unsafe_code)]
        let pointer = unsafe { self.pointer.as_ref().buffer };
        if pointer.is_null() || !pointer.is_aligned() {
            return Err(CaptureError::invalid_frame(
                "PipeWire SPA buffer pointer is null or misaligned",
            ));
        }
        // SAFETY: the live native buffer owns this SPA buffer; structural
        // null/alignment checks passed, and the borrow cannot outlive self.
        #[allow(unsafe_code)]
        unsafe {
            Ok(&*pointer)
        }
    }
}

impl Drop for NativeVideoBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: this is the exact non-null pointer dequeued from this stream.
        // The guard is its sole owner, and all plane borrows have ended.
        #[allow(unsafe_code)]
        unsafe {
            self.stream.queue_raw_buffer(self.pointer.as_ptr());
        }
    }
}

/// Copies crop metadata after checking the native table and payload shape.
///
/// # Safety
/// The buffer must come from an owned, live native buffer or a live fixture.
/// Non-null, aligned tables with at most 64 entries must reference allocations
/// covering the advertised entries. Non-null crop payloads with sufficient
/// advertised size must reference at least one initialized `spa_meta_region`.
/// The allocations must remain unchanged during this call. Pointers rejected
/// by structural checks are never dereferenced.
#[allow(unsafe_code)]
unsafe fn read_video_crop(
    buffer: &libspa_sys::spa_buffer,
) -> Result<Option<VideoCrop>, CaptureError> {
    if buffer.n_metas > MAX_METADATA_ENTRIES {
        return Err(CaptureError::invalid_frame(
            "PipeWire metadata table exceeds the supported entry count",
        ));
    }
    if buffer.n_metas == 0 {
        return Ok(None);
    }
    if buffer.metas.is_null() || !buffer.metas.is_aligned() {
        return Err(CaptureError::invalid_frame(
            "PipeWire metadata table pointer is null or misaligned",
        ));
    }
    let count = usize::try_from(buffer.n_metas)
        .map_err(|_| CaptureError::invalid_frame("PipeWire metadata count overflows usize"))?;
    // SAFETY: the caller guarantees the advertised table allocation is live;
    // count is bounded to 64, and null/alignment checks passed above.
    #[allow(unsafe_code)]
    let metadata = unsafe { std::slice::from_raw_parts(buffer.metas, count) };
    let mut crops = metadata
        .iter()
        .filter(|meta| meta.type_ == libspa_sys::SPA_META_VideoCrop);
    let Some(crop) = crops.next() else {
        return Ok(None);
    };
    if crops.next().is_some() {
        return Err(CaptureError::invalid_frame(
            "PipeWire buffer contains duplicate VideoCrop metadata",
        ));
    }
    if crop.data.is_null()
        || !usize::try_from(crop.size)
            .is_ok_and(|size| size >= size_of::<libspa_sys::spa_meta_region>())
    {
        return Err(CaptureError::invalid_frame(
            "PipeWire VideoCrop metadata payload is null or too short",
        ));
    }
    // SAFETY: the caller guarantees a live initialized payload when its
    // advertised size is sufficient. Size/null checks passed above. Metadata
    // payloads need not be aligned, so copy with read_unaligned, not a borrow.
    #[allow(unsafe_code)]
    let crop = unsafe {
        crop.data
            .cast::<libspa_sys::spa_meta_region>()
            .read_unaligned()
    };
    Ok(Some(VideoCrop {
        x: crop.region.position.x,
        y: crop.region.position.y,
        width: crop.region.size.width,
        height: crop.region.size.height,
    }))
}

#[cfg(test)]
mod tests {
    use std::{mem::size_of, ptr};

    use gif_from_screen_capture::{CaptureError, CaptureErrorKind};

    use super::{VideoCrop, read_video_crop};

    fn region(x: i32, y: i32, width: u32, height: u32) -> libspa_sys::spa_meta_region {
        libspa_sys::spa_meta_region {
            region: libspa_sys::spa_region {
                position: libspa_sys::spa_point { x, y },
                size: libspa_sys::spa_rectangle { width, height },
            },
        }
    }

    fn crop_meta(region: &mut libspa_sys::spa_meta_region) -> libspa_sys::spa_meta {
        libspa_sys::spa_meta {
            type_: libspa_sys::SPA_META_VideoCrop,
            size: u32::try_from(size_of::<libspa_sys::spa_meta_region>()).unwrap(),
            data: ptr::from_mut(region).cast(),
        }
    }

    fn buffer(metadata: &mut [libspa_sys::spa_meta]) -> libspa_sys::spa_buffer {
        libspa_sys::spa_buffer {
            n_metas: u32::try_from(metadata.len()).unwrap(),
            n_datas: 0,
            metas: metadata.as_mut_ptr(),
            datas: ptr::null_mut(),
        }
    }

    fn read_fixture(buffer: &libspa_sys::spa_buffer) -> Result<Option<VideoCrop>, CaptureError> {
        // SAFETY: callers keep each metadata table and advertised payload live
        // for the call. Malformed fixtures only use pointers/counts guaranteed
        // to be rejected before dereferencing, matching the helper's contract.
        #[allow(unsafe_code)]
        unsafe {
            read_video_crop(buffer)
        }
    }

    #[test]
    fn copies_the_actual_native_region_without_losing_offsets() {
        let mut payload = region(32, 48, 800, 600);
        let mut metadata = [crop_meta(&mut payload)];
        assert_eq!(
            read_fixture(&buffer(&mut metadata)).unwrap(),
            Some(VideoCrop {
                x: 32,
                y: 48,
                width: 800,
                height: 600,
            })
        );
    }

    #[test]
    fn copies_unaligned_native_payloads() {
        #[repr(C, align(8))]
        struct PayloadBytes([u8; size_of::<libspa_sys::spa_meta_region>() + 1]);

        let mut bytes = PayloadBytes([0; size_of::<libspa_sys::spa_meta_region>() + 1]);
        // The fixture intentionally exercises an unaligned native payload.
        #[allow(clippy::cast_ptr_alignment)]
        let pointer = bytes
            .0
            .as_mut_ptr()
            .wrapping_add(1)
            .cast::<libspa_sys::spa_meta_region>();
        assert!(!pointer.is_aligned());
        // SAFETY: the offset leaves exactly enough allocated bytes for the
        // initialized native value, and write_unaligned permits this address.
        #[allow(unsafe_code)]
        unsafe {
            pointer.write_unaligned(region(32, 48, 800, 600));
        }
        let mut metadata = [libspa_sys::spa_meta {
            type_: libspa_sys::SPA_META_VideoCrop,
            size: u32::try_from(size_of::<libspa_sys::spa_meta_region>()).unwrap(),
            data: pointer.cast(),
        }];
        assert_eq!(
            read_fixture(&buffer(&mut metadata)).unwrap(),
            Some(VideoCrop {
                x: 32,
                y: 48,
                width: 800,
                height: 600,
            })
        );
    }

    #[test]
    fn zero_dimensions_and_signed_offsets_are_preserved_for_geometry_validation() {
        for (width, height) in [(0, 0), (0, 600), (800, 0)] {
            let mut payload = region(-1, 48, width, height);
            let mut metadata = [crop_meta(&mut payload)];
            assert_eq!(
                read_fixture(&buffer(&mut metadata)).unwrap(),
                Some(VideoCrop {
                    x: -1,
                    y: 48,
                    width,
                    height,
                })
            );
        }
    }

    #[test]
    fn an_empty_table_needs_no_pointer() {
        let mut buffer = buffer(&mut []);
        buffer.metas = ptr::null_mut();
        assert_eq!(read_fixture(&buffer).unwrap(), None);
    }

    #[test]
    fn unrelated_metadata_does_not_require_a_crop_payload() {
        let mut metadata = [libspa_sys::spa_meta {
            type_: libspa_sys::SPA_META_Header,
            size: 0,
            data: ptr::null_mut(),
        }];
        assert_eq!(read_fixture(&buffer(&mut metadata)).unwrap(), None);
    }

    #[test]
    fn the_last_entry_of_a_maximum_size_table_can_hold_the_crop() {
        let mut payload = region(32, 48, 800, 600);
        let mut metadata = [libspa_sys::spa_meta {
            type_: libspa_sys::SPA_META_Header,
            size: 0,
            data: ptr::null_mut(),
        }; 64];
        metadata[63] = crop_meta(&mut payload);
        assert_eq!(
            read_fixture(&buffer(&mut metadata)).unwrap(),
            Some(VideoCrop {
                x: 32,
                y: 48,
                width: 800,
                height: 600,
            })
        );
    }

    #[test]
    fn a_short_crop_payload_is_rejected_before_reading_it() {
        let mut payload = region(32, 48, 800, 600);
        let mut metadata = [crop_meta(&mut payload)];
        metadata[0].size -= 1;
        let error = read_fixture(&buffer(&mut metadata)).unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
    }

    #[test]
    fn a_null_crop_payload_is_rejected() {
        let mut payload = region(32, 48, 800, 600);
        let mut metadata = [crop_meta(&mut payload)];
        metadata[0].data = ptr::null_mut();
        let error = read_fixture(&buffer(&mut metadata)).unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
    }

    #[test]
    fn duplicate_crop_metadata_is_rejected_even_when_both_payloads_are_valid() {
        let mut first = region(32, 48, 800, 600);
        let mut second = region(64, 96, 800, 600);
        let mut metadata = [crop_meta(&mut first), crop_meta(&mut second)];
        let error = read_fixture(&buffer(&mut metadata)).unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
        assert!(error.message().contains("duplicate"));
    }

    #[test]
    fn excessive_metadata_counts_are_rejected_before_borrowing_the_table() {
        for count in [65, u32::MAX] {
            let mut buffer = buffer(&mut []);
            buffer.n_metas = count;
            buffer.metas = ptr::null_mut();
            let error = read_fixture(&buffer).unwrap_err();
            assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
            assert!(error.message().contains("entry count"));
        }
    }

    #[test]
    fn nonempty_null_metadata_tables_are_rejected() {
        let mut buffer = buffer(&mut []);
        buffer.n_metas = 1;
        buffer.metas = ptr::null_mut();
        let error = read_fixture(&buffer).unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
    }

    #[test]
    fn misaligned_metadata_tables_are_rejected_before_borrowing() {
        let mut payload = region(32, 48, 800, 600);
        let mut metadata = [crop_meta(&mut payload)];
        let mut buffer = buffer(&mut metadata);
        buffer.metas = buffer.metas.wrapping_byte_add(1);
        let error = read_fixture(&buffer).unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
    }
}
