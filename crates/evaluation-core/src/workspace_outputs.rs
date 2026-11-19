//! Collection of files a sandboxed task wrote to `/workspace/output`.
//!
//! The host walks the run's output directory only after every process in the
//! sandbox is gone, and never follows links: directories are entered by
//! metadata type, files are opened with `O_NOFOLLOW`, and anything that is not
//! a regular file or directory (symlinks, FIFOs, sockets, devices) is rejected
//! and reported instead of read. Hard links to host files cannot exist there
//! (`link(2)` across mounts fails with `EXDEV`).

use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Deepest directory nesting that is walked.
pub const MAX_OUTPUT_DEPTH: usize = 32;

/// One collected regular file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedFile {
    /// `/`-separated path relative to `/workspace/output`.
    pub path: String,
    pub byte_length: u64,
    pub sha256_hex: String,
}

/// Result of walking an output directory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CollectedOutputs {
    pub files: Vec<CollectedFile>,
    /// Entries that were not read, with the reason.
    pub rejected: Vec<(String, &'static str)>,
    pub total_bytes: u64,
    /// Set when the byte or file-count limit was exceeded; collection stops.
    pub limit_exceeded: Option<String>,
}

/// Where a request's collected files are staged under the runner's artifact
/// root. Named by the SHA-256 of the request ID, so arbitrary request IDs can
/// never escape the root. The recorder uses the same function to find them.
pub fn artifact_staging_dir(artifact_root: &Path, request_id: &str) -> PathBuf {
    artifact_root.join(hex(&Sha256::digest(request_id.as_bytes())))
}

/// Walks `dir` (which must not be a symlink) and hashes every regular file,
/// stopping as soon as `max_bytes` or `max_files` would be exceeded.
pub fn collect_outputs(
    dir: &Path,
    max_bytes: u64,
    max_files: usize,
) -> io::Result<CollectedOutputs> {
    collect(dir, None, max_bytes, max_files)
}

/// Like [`collect_outputs`], and also copies every accepted file to the same
/// relative path under `dest` (created with mode 0700/0600). Used when the
/// outputs live somewhere the host must not keep reading from, such as a
/// paused container's tmpfs. On a limit breach `dest` may hold a partial
/// copy; the caller discards it.
pub fn collect_outputs_into(
    dir: &Path,
    dest: &Path,
    max_bytes: u64,
    max_files: usize,
) -> io::Result<CollectedOutputs> {
    collect(dir, Some(dest), max_bytes, max_files)
}

fn collect(
    dir: &Path,
    dest: Option<&Path>,
    max_bytes: u64,
    max_files: usize,
) -> io::Result<CollectedOutputs> {
    let mut out = CollectedOutputs::default();
    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
        return Err(io::Error::other("output directory is not a directory"));
    }
    let mut stack = vec![(dir.to_path_buf(), String::new(), 0_usize)];
    while let Some((current, prefix, depth)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&current)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                out.rejected.push((
                    format!("{prefix}{}", name.to_string_lossy()),
                    "file name is not valid UTF-8",
                ));
                continue;
            };
            let relative = format!("{prefix}{name}");
            let file_type = fs::symlink_metadata(entry.path())?.file_type();
            if file_type.is_dir() {
                if depth + 1 >= MAX_OUTPUT_DEPTH {
                    out.rejected
                        .push((relative, "directory nesting is too deep"));
                } else {
                    stack.push((entry.path(), format!("{relative}/"), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                let reason = if file_type.is_symlink() {
                    "symbolic links are not collected"
                } else {
                    "only regular files are collected"
                };
                out.rejected.push((relative, reason));
                continue;
            }
            if out.files.len() >= max_files {
                out.limit_exceeded = Some(format!("more than {max_files} output files"));
                return Ok(out);
            }
            let remaining = max_bytes.saturating_sub(out.total_bytes);
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(entry.path())?;
            let mut copy = match dest {
                Some(dest) => {
                    let target = dest.join(&relative);
                    if let Some(parent) = target.parent() {
                        fs::DirBuilder::new()
                            .recursive(true)
                            .mode(0o700)
                            .create(parent)?;
                    }
                    Some(
                        OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(target)?,
                    )
                }
                None => None,
            };
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            let mut length = 0_u64;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                length += read as u64;
                if length > remaining {
                    out.limit_exceeded =
                        Some(format!("output files exceed {max_bytes} bytes in total"));
                    return Ok(out);
                }
                hasher.update(&buffer[..read]);
                if let Some(copy) = copy.as_mut() {
                    copy.write_all(&buffer[..read])?;
                }
            }
            if let Some(copy) = copy.as_mut() {
                copy.sync_all()?;
            }
            out.total_bytes += length;
            out.files.push(CollectedFile {
                path: relative,
                byte_length: length,
                sha256_hex: hex(&hasher.finalize()),
            });
        }
    }
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cybercore-outputs-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collects_nested_regular_files_with_digests() {
        let dir = temp("nested");
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("top.txt"), b"abc").unwrap();
        fs::write(dir.join("a/b/deep.txt"), b"").unwrap();
        let out = collect_outputs(&dir, 1024, 10).unwrap();
        let paths: Vec<_> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a/b/deep.txt", "top.txt"]);
        assert_eq!(
            out.files[1].sha256_hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(out.total_bytes, 3);
        assert!(out.rejected.is_empty() && out.limit_exceeded.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn symlinks_and_fifos_are_reported_never_read() {
        let dir = temp("links");
        let secret = temp("links-secret").join("secret.txt");
        fs::write(&secret, b"host secret").unwrap();
        symlink(&secret, dir.join("leak.txt")).unwrap();
        symlink(secret.parent().unwrap(), dir.join("leakdir")).unwrap();
        let made_fifo = std::process::Command::new("mkfifo")
            .arg(dir.join("pipe"))
            .status()
            .unwrap();
        assert!(made_fifo.success());
        let out = collect_outputs(&dir, 1024, 10).unwrap();
        assert!(out.files.is_empty());
        let rejected: Vec<_> = out.rejected.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(rejected, vec!["leak.txt", "leakdir", "pipe"]);
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(secret.parent().unwrap()).unwrap();
    }

    #[test]
    fn byte_and_file_limits_stop_collection() {
        let dir = temp("limits");
        fs::write(dir.join("a"), vec![b'x'; 600]).unwrap();
        fs::write(dir.join("b"), vec![b'x'; 600]).unwrap();
        let bytes = collect_outputs(&dir, 1000, 10).unwrap();
        assert!(bytes.limit_exceeded.unwrap().contains("1000 bytes"));
        let count = collect_outputs(&dir, 10_000, 1).unwrap();
        assert!(count
            .limit_exceeded
            .unwrap()
            .contains("more than 1 output files"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copying_collection_reproduces_accepted_files_only() {
        let dir = temp("copy-src");
        let dest = temp("copy-dest").join("out");
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/x.txt"), b"x").unwrap();
        symlink("/etc/passwd", dir.join("leak")).unwrap();
        let out = collect_outputs_into(&dir, &dest, 1024, 10).unwrap();
        assert_eq!(out.files.len(), 1);
        assert_eq!(fs::read(dest.join("a/x.txt")).unwrap(), b"x");
        assert!(!dest.join("leak").exists());
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(dest.parent().unwrap()).unwrap();
    }

    #[test]
    fn staging_dirs_cannot_escape_the_root() {
        let root = Path::new("/srv/artifacts");
        let dir = artifact_staging_dir(root, "../../etc/passwd");
        assert_eq!(dir.parent(), Some(root));
        assert_eq!(dir.file_name().unwrap().len(), 64);
    }
}
