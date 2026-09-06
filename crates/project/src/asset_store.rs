use std::{
    fs,
    path::{Path, PathBuf},
};

use gif_from_screen_domain::AssetId;

use crate::{ProjectError, atomic_file::atomic_write};

#[derive(Clone, Debug)]
pub struct AssetStore {
    directory: PathBuf,
}

impl AssetStore {
    /// Computes the stable content identity without reading or writing the store.
    pub fn id_for_bytes(bytes: &[u8]) -> AssetId {
        digest(bytes)
    }

    pub fn open(project_root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let directory = project_root.as_ref().join("assets");
        crate::private_fs::create_dir_all(&directory)
            .map_err(|error| ProjectError::io("create asset directory", &directory, error))?;
        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn asset_path(&self, asset_id: AssetId) -> PathBuf {
        self.directory.join(format!("{asset_id}.frame"))
    }

    /// Stores immutable bytes under their BLAKE3 digest. Existing identical
    /// content is reused; an existing mismatching file is reported as damage.
    pub fn put(&self, bytes: &[u8]) -> Result<AssetId, ProjectError> {
        let asset_id = digest(bytes);
        let path = self.asset_path(asset_id);
        if path.exists() {
            self.verify(asset_id)?;
            return Ok(asset_id);
        }
        atomic_write(&path, bytes)?;
        self.verify(asset_id)?;
        Ok(asset_id)
    }

    pub fn contains(&self, asset_id: AssetId) -> bool {
        self.asset_path(asset_id).is_file()
    }

    /// Reads and verifies the content digest before returning bytes.
    pub fn read(&self, asset_id: AssetId) -> Result<Vec<u8>, ProjectError> {
        let path = self.asset_path(asset_id);
        let bytes =
            fs::read(&path).map_err(|error| ProjectError::io("read asset", &path, error))?;
        if digest(&bytes) != asset_id {
            return Err(ProjectError::CorruptAsset { asset_id, path });
        }
        Ok(bytes)
    }

    pub fn verify(&self, asset_id: AssetId) -> Result<u64, ProjectError> {
        let bytes = self.read(asset_id)?;
        Ok(bytes.len() as u64)
    }
}

fn digest(bytes: &[u8]) -> AssetId {
    AssetId::from_digest(*blake3::hash(bytes).as_bytes())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn identical_bytes_are_content_deduplicated() {
        let root = tempdir().unwrap();
        let store = AssetStore::open(root.path()).unwrap();
        let first = store.put(b"same pixels").unwrap();
        assert_eq!(first, AssetStore::id_for_bytes(b"same pixels"));
        let second = store.put(b"same pixels").unwrap();
        assert_eq!(first, second);
        assert_eq!(store.read(first).unwrap(), b"same pixels");
        assert_eq!(fs::read_dir(store.directory()).unwrap().count(), 1);
    }

    #[test]
    fn tampering_is_detected() {
        let root = tempdir().unwrap();
        let store = AssetStore::open(root.path()).unwrap();
        let id = store.put(b"original").unwrap();
        fs::write(store.asset_path(id), b"changed").unwrap();
        assert!(matches!(
            store.read(id),
            Err(ProjectError::CorruptAsset { .. })
        ));
    }
}
