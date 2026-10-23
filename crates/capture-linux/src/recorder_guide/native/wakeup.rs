//! A private command pipe lets geometry changes interrupt the native input wait.

use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
};

pub(crate) struct WakeSignal {
    pub(super) reader: UnixStream,
    writer: UnixStream,
}

impl WakeSignal {
    pub(crate) fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self { reader, writer })
    }

    pub(crate) fn notify(&self) {
        // A full pipe already represents a pending wakeup. Requests coalesce
        // in Shared; the pipe carries neither coordinates nor a work backlog.
        let mut writer = &self.writer;
        let _ = writer.write(&[1]);
    }

    pub(super) fn drain(&self) {
        let mut reader = &self.reader;
        let mut buffer = [0; 256];
        while matches!(reader.read(&mut buffer), Ok(count) if count > 0) {}
    }
}
