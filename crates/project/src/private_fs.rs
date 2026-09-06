//! Project pixels and input annotations are private to the creating user.
//! Opening existing entries does not chmod them; atomic replacement files are private too.

use std::{
    fs::{DirBuilder, OpenOptions},
    io,
    path::Path,
};

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub(crate) fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn new_data_is_private_without_changing_existing_permissions() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("new/assets");
        create_dir_all(&private).unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o077,
            0
        );
        let data = private.join("pixels");
        file_options()
            .write(true)
            .create_new(true)
            .open(&data)
            .unwrap();
        assert_eq!(fs::metadata(&data).unwrap().permissions().mode() & 0o077, 0);
        fs::set_permissions(&data, fs::Permissions::from_mode(0o640)).unwrap();
        file_options().append(true).open(&data).unwrap();
        assert_eq!(
            fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::set_permissions(&private, fs::Permissions::from_mode(0o750)).unwrap();
        create_dir_all(&private).unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
}
