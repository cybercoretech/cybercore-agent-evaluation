//! Content-addressed evidence object storage.

use crate::canonical::{is_sha256_hex, sha256_hex};
use crate::error::{StoreError, StoreResult};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Object storage port (ADR 0002). Objects are addressed by the lowercase hex
/// SHA-256 of their content, so writes are idempotent and reads verifiable.
/// Hosted adapters (R2) implement the same contract.
pub trait ArtifactStore: Send + Sync {
    /// Stores `bytes` and returns their SHA-256 (the object key). Storing the
    /// same content again is a cheap no-op.
    fn put(&self, bytes: &[u8]) -> StoreResult<String>;
    /// Returns the object's bytes after checking them against `sha256`.
    fn get(&self, sha256: &str) -> StoreResult<Vec<u8>>;
    /// True when an object with this digest exists and its content matches.
    fn verify(&self, sha256: &str) -> StoreResult<bool>;
    /// Every stored object digest (for orphan detection).
    fn list(&self) -> StoreResult<Vec<String>>;
}

/// Filesystem implementation: `<root>/objects/<aa>/<rest-of-digest>`.
/// Writes go to `<root>/tmp`, are fsynced, then atomically renamed into place,
/// so a crash leaves either no object or a complete one.
pub struct FsArtifactStore {
    root: PathBuf,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl FsArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> StoreResult<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Self { root })
    }

    fn object_path(&self, sha256: &str) -> StoreResult<PathBuf> {
        if !is_sha256_hex(sha256) {
            return Err(StoreError::InvalidRecord(format!(
                "object key {sha256:?} is not a lowercase hex SHA-256"
            )));
        }
        Ok(self
            .root
            .join("objects")
            .join(&sha256[..2])
            .join(&sha256[2..]))
    }

    fn temp_path(&self) -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.root
            .join("tmp")
            .join(format!("{}-{n}.partial", std::process::id()))
    }
}

impl ArtifactStore for FsArtifactStore {
    fn put(&self, bytes: &[u8]) -> StoreResult<String> {
        let sha256 = sha256_hex(bytes);
        let destination = self.object_path(&sha256)?;
        if destination.is_file() && self.verify(&sha256)? {
            return Ok(sha256);
        }
        let parent = destination
            .parent()
            .ok_or_else(|| StoreError::Integrity("object path has no parent".into()))?;
        fs::create_dir_all(parent)?;

        let temp = self.temp_path();
        let result = (|| -> StoreResult<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &destination)?;
            sync_directory(parent)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map(|()| sha256)
    }

    fn get(&self, sha256: &str) -> StoreResult<Vec<u8>> {
        let path = self.object_path(sha256)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(StoreError::NotFound(format!("object {sha256}")))
            }
            Err(error) => return Err(error.into()),
        };
        if sha256_hex(&bytes) != sha256 {
            return Err(StoreError::Integrity(format!(
                "object {sha256} content does not match its digest"
            )));
        }
        Ok(bytes)
    }

    fn verify(&self, sha256: &str) -> StoreResult<bool> {
        match self.get(sha256) {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound(_) | StoreError::Integrity(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn list(&self) -> StoreResult<Vec<String>> {
        let mut digests = Vec::new();
        for prefix in fs::read_dir(self.root.join("objects"))? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            let head = prefix.file_name().to_string_lossy().into_owned();
            for object in fs::read_dir(prefix.path())? {
                let digest = format!("{head}{}", object?.file_name().to_string_lossy());
                if is_sha256_hex(&digest) {
                    digests.push(digest);
                }
            }
        }
        digests.sort();
        Ok(digests)
    }
}

fn sync_directory(path: &Path) -> StoreResult<()> {
    // Persist the rename itself; without this a power loss can drop the entry.
    File::open(path)?.sync_all()?;
    Ok(())
}
