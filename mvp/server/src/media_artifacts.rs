//! Private, immutable content-addressed evidence objects.
//!
//! The caller must place the store in a private directory that other processes
//! cannot rename while an operation is in progress. Path checks reject observed
//! symlinks and Windows reparse points; portable `std::fs` cannot make a whole
//! directory walk race-free against a process with write access to that tree.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

const BUFFER_BYTES: usize = 64 * 1024;
const MAX_JSONL_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSONL_RANGE: usize = 10_000;
const MAX_JSONL_RANGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRef {
    pub sha256: String,
    pub bytes: u64,
}

impl ArtifactRef {
    pub fn to_json(&self) -> Value {
        json!({"sha256": self.sha256, "bytes": self.bytes})
    }

    pub fn from_json(value: &Value) -> io::Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("Artifact reference must be an object"))?;
        if object.len() != 2 {
            return Err(invalid("Artifact reference has unexpected fields"));
        }
        let sha256 = object
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Artifact reference needs sha256"))?
            .to_owned();
        let bytes = object
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("Artifact reference needs unsigned bytes"))?;
        let reference = Self { sha256, bytes };
        validate_ref(&reference)?;
        Ok(reference)
    }
}

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupObject {
    pub reference: ArtifactRef,
    pub relative_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupDeclaration {
    pub root: PathBuf,
    pub objects: Vec<BackupObject>,
}

impl ArtifactStore {
    pub fn open(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(invalid("Artifact root must be absolute"));
        }
        reject_reparse_components(root)?;
        fs::create_dir_all(root)?;
        reject_reparse_components(root)?;
        let root = fs::canonicalize(root)?;
        let store = Self { root };
        store.ensure_dir(&store.root.join("objects"))?;
        store.ensure_dir(&store.root.join("objects").join(".staging"))?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> io::Result<ArtifactRef> {
        let mut writer = self.begin()?;
        writer.write_raw(bytes)?;
        writer.finish()
    }

    pub fn put_file(&self, source: &Path) -> io::Result<ArtifactRef> {
        let mut input = File::open(source)?;
        let mut writer = self.begin()?;
        let mut buffer = [0_u8; BUFFER_BYTES];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_raw(&buffer[..read])?;
        }
        writer.finish()
    }

    /// Start a newline-delimited JSON inventory without collecting it in memory.
    pub fn begin_jsonl(&self) -> io::Result<JsonlWriter> {
        Ok(JsonlWriter {
            inner: self.begin()?,
        })
    }

    pub fn write_jsonl<I>(&self, values: I) -> io::Result<ArtifactRef>
    where
        I: IntoIterator<Item = Value>,
    {
        let mut writer = self.begin_jsonl()?;
        for value in values {
            writer.append(&value)?;
        }
        writer.finish()
    }

    /// Verify content before exposing its path. Callers should not persist this
    /// path as an external identifier; the reference is the durable identity.
    pub fn path(&self, reference: &ArtifactRef) -> io::Result<PathBuf> {
        self.verify(reference)?;
        self.object_path(reference)
    }

    pub fn verify(&self, reference: &ArtifactRef) -> io::Result<()> {
        let path = self.object_path(reference)?;
        let mut file = self.open_object(&path)?;
        let mut hash = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            bytes = bytes
                .checked_add(read as u64)
                .ok_or_else(|| invalid("Artifact size overflow"))?;
        }
        compare_content(reference, bytes, hash)
    }

    pub fn read_bytes(&self, reference: &ArtifactRef, max_bytes: u64) -> io::Result<Vec<u8>> {
        if reference.bytes > max_bytes || reference.bytes > usize::MAX as u64 {
            return Err(invalid("Artifact exceeds read limit"));
        }
        let path = self.object_path(reference)?;
        let mut file = self.open_object(&path)?;
        let mut result = Vec::new();
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            if (result.len() as u64).saturating_add(read as u64) > max_bytes {
                return Err(invalid("Artifact exceeds read limit"));
            }
            result.extend_from_slice(&buffer[..read]);
            hash.update(&buffer[..read]);
        }
        compare_content(reference, result.len() as u64, hash)?;
        Ok(result)
    }

    /// Parse every JSONL record and hash the complete object, including records
    /// outside the requested range. A truncated final line is always an error.
    pub fn read_jsonl_range(
        &self,
        reference: &ArtifactRef,
        start: usize,
        count: usize,
    ) -> io::Result<Vec<Value>> {
        if count > MAX_JSONL_RANGE {
            return Err(invalid("JSONL range exceeds limit"));
        }
        let end = start
            .checked_add(count)
            .ok_or_else(|| invalid("JSONL range overflow"))?;
        let path = self.object_path(reference)?;
        let mut reader = BufReader::with_capacity(BUFFER_BYTES, self.open_object(&path)?);
        let mut hash = Sha256::new();
        let mut bytes = 0_u64;
        let mut index = 0_usize;
        let mut line = Vec::new();
        let mut values = Vec::new();
        let mut retained_bytes = 0_usize;
        let mut buffer = [0_u8; BUFFER_BYTES];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            bytes = bytes
                .checked_add(read as u64)
                .ok_or_else(|| invalid("Artifact size overflow"))?;
            for byte in &buffer[..read] {
                if *byte == b'\n' {
                    let value: Value = serde_json::from_slice(&line).map_err(|error| {
                        invalid(format!("Invalid JSONL record {index}: {error}"))
                    })?;
                    if !value.is_object() {
                        return Err(invalid(format!("JSONL record {index} must be an object")));
                    }
                    if index >= start && index < end {
                        if retained_bytes.saturating_add(line.len()) > MAX_JSONL_RANGE_BYTES {
                            return Err(invalid("JSONL range exceeds byte limit"));
                        }
                        retained_bytes += line.len();
                        values.push(value);
                    }
                    index = index
                        .checked_add(1)
                        .ok_or_else(|| invalid("JSONL record count overflow"))?;
                    line.clear();
                } else {
                    if line.len() >= MAX_JSONL_LINE_BYTES {
                        return Err(invalid("JSONL record exceeds line limit"));
                    }
                    line.push(*byte);
                }
            }
        }
        if !line.is_empty() {
            return Err(invalid("JSONL ends with an unterminated record"));
        }
        compare_content(reference, bytes, hash)?;
        Ok(values)
    }

    /// Exact backup inventory for caller-supplied references. No store-wide
    /// scan or garbage collection is implied by this declaration.
    pub fn backup_declaration(&self, references: &[ArtifactRef]) -> io::Result<BackupDeclaration> {
        let mut unique = BTreeMap::<String, ArtifactRef>::new();
        for reference in references {
            validate_ref(reference)?;
            if let Some(previous) = unique.insert(reference.sha256.clone(), reference.clone()) {
                if previous.bytes != reference.bytes {
                    return Err(invalid("Conflicting artifact sizes for one hash"));
                }
            }
        }
        let mut objects = Vec::with_capacity(unique.len());
        for reference in unique.into_values() {
            self.verify(&reference)?;
            objects.push(BackupObject {
                relative_path: PathBuf::from("objects")
                    .join(&reference.sha256[..2])
                    .join(&reference.sha256),
                reference,
            });
        }
        Ok(BackupDeclaration {
            root: self.root.clone(),
            objects,
        })
    }

    fn begin(&self) -> io::Result<RawWriter> {
        let staging = self.root.join("objects").join(".staging");
        self.ensure_dir(&staging)?;
        for _ in 0..16 {
            let path = staging.join(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
            reject_reparse_components(&path)?;
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(RawWriter {
                        store: self.clone(),
                        path,
                        file: Some(file),
                        hash: Sha256::new(),
                        bytes: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Could not reserve artifact temp file",
        ))
    }

    fn ensure_dir(&self, dir: &Path) -> io::Result<()> {
        reject_reparse_components(dir)?;
        fs::create_dir_all(dir)?;
        reject_reparse_components(dir)?;
        if !fs::metadata(dir)?.is_dir() {
            return Err(invalid("Artifact path is not a directory"));
        }
        Ok(())
    }

    fn object_path(&self, reference: &ArtifactRef) -> io::Result<PathBuf> {
        validate_ref(reference)?;
        Ok(self
            .root
            .join("objects")
            .join(&reference.sha256[..2])
            .join(&reference.sha256))
    }

    fn open_object(&self, path: &Path) -> io::Result<File> {
        reject_reparse_components(path)?;
        if !fs::symlink_metadata(path)?.is_file() {
            return Err(invalid("Artifact object is not a regular file"));
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("Artifact object is not a regular file"));
        }
        Ok(file)
    }

    fn publish(&self, temp: &Path, reference: &ArtifactRef) -> io::Result<()> {
        let target = self.object_path(reference)?;
        self.ensure_dir(target.parent().expect("object has shard parent"))?;
        reject_reparse_components(&target)?;
        // A hard link creates the final name atomically and fails if it exists.
        // Unlike rename on Unix, it never replaces another writer's object.
        match fs::hard_link(temp, &target) {
            Ok(()) => self.verify(reference),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => self.verify(reference),
            Err(error) => Err(error),
        }
    }
}

pub struct JsonlWriter {
    inner: RawWriter,
}

impl JsonlWriter {
    pub fn append(&mut self, value: &Value) -> io::Result<()> {
        if !value.is_object() {
            return Err(invalid("JSONL record must be an object"));
        }
        let encoded = serde_json::to_vec(value).map_err(io::Error::other)?;
        if encoded.len() > MAX_JSONL_LINE_BYTES {
            return Err(invalid("JSONL record exceeds line limit"));
        }
        self.inner.write_raw(&encoded)?;
        self.inner.write_raw(b"\n")
    }

    pub fn finish(self) -> io::Result<ArtifactRef> {
        self.inner.finish()
    }
}

struct RawWriter {
    store: ArtifactStore,
    path: PathBuf,
    file: Option<File>,
    hash: Sha256,
    bytes: u64,
}

impl RawWriter {
    fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        let next = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("Artifact size overflow"))?;
        self.file
            .as_mut()
            .expect("unfinished writer")
            .write_all(bytes)?;
        self.hash.update(bytes);
        self.bytes = next;
        Ok(())
    }

    fn finish(mut self) -> io::Result<ArtifactRef> {
        let file = self.file.take().expect("unfinished writer");
        file.sync_all()?;
        drop(file);
        let reference = ArtifactRef {
            sha256: format!("{:x}", self.hash.clone().finalize()),
            bytes: self.bytes,
        };
        self.store.publish(&self.path, &reference)?;
        Ok(reference)
    }
}

impl Drop for RawWriter {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn validate_ref(reference: &ArtifactRef) -> io::Result<()> {
    if reference.sha256.len() != 64
        || !reference
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "Artifact sha256 must be 64 lowercase hexadecimal characters",
        ));
    }
    Ok(())
}

fn compare_content(reference: &ArtifactRef, bytes: u64, hash: Sha256) -> io::Result<()> {
    if bytes != reference.bytes || format!("{:x}", hash.finalize()) != reference.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Artifact object size or digest mismatch",
        ));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn reject_reparse_components(path: &Path) -> io::Result<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        // On Windows, `C:` and `\\?\C:` are drive designators, not complete
        // filesystem paths. Querying either with symlink_metadata can fail with
        // ERROR_INVALID_FUNCTION; the following RootDir yields `C:\`.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || is_windows_reparse(&metadata) {
                    return Err(invalid(format!(
                        "Artifact path contains a symlink or reparse point: {}",
                        prefix.display()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_windows_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

#[cfg(not(windows))]
fn is_windows_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immutable_replay_and_corruption_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let reference = store.put_bytes(b"evidence").unwrap();
        assert_eq!(store.put_bytes(b"evidence").unwrap(), reference);
        assert_eq!(store.read_bytes(&reference, 8).unwrap(), b"evidence");
        assert!(store.read_bytes(&reference, 7).is_err());
        let declaration = store
            .backup_declaration(&[reference.clone(), reference.clone()])
            .unwrap();
        assert_eq!(declaration.objects.len(), 1);
        fs::write(store.path(&reference).unwrap(), b"tampered").unwrap();
        assert!(store.verify(&reference).is_err());
        assert!(store.put_bytes(b"evidence").is_err());
        assert!(store.backup_declaration(&[reference]).is_err());
    }

    #[test]
    fn file_ingest_streams_to_same_content_address() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let source = dir.path().join("source.bin");
        let bytes = vec![42_u8; BUFFER_BYTES * 3 + 7];
        fs::write(&source, &bytes).unwrap();
        let reference = store.put_file(&source).unwrap();
        assert_eq!(store.put_bytes(&bytes).unwrap(), reference);
        assert_eq!(
            store.read_bytes(&reference, bytes.len() as u64).unwrap(),
            bytes
        );
    }

    #[test]
    fn jsonl_range_checks_skipped_records_and_entire_digest() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let mut writer = store.begin_jsonl().unwrap();
        writer.append(&json!({"frame": 0})).unwrap();
        writer.append(&json!({"frame": 1})).unwrap();
        writer.append(&json!({"frame": 2})).unwrap();
        let reference = writer.finish().unwrap();
        assert_eq!(
            store.read_jsonl_range(&reference, 1, 1).unwrap(),
            vec![json!({"frame": 1})]
        );
        assert_eq!(
            store.read_jsonl_range(&reference, 10, 1).unwrap(),
            Vec::<Value>::new()
        );
        let path = store.path(&reference).unwrap();
        fs::write(&path, b"{\"frame\":0}\n{\"frame\":1}\n{\"frame\":9}\n").unwrap();
        assert!(store.read_jsonl_range(&reference, 0, 1).is_err());
        fs::write(&path, b"{\"frame\":0}\n{broken}\n{\"frame\":2}\n").unwrap();
        assert!(store.read_jsonl_range(&reference, 2, 1).is_err());
        fs::write(&path, b"{\"frame\":0}").unwrap();
        assert!(store.read_jsonl_range(&reference, 0, 1).is_err());
    }

    #[test]
    fn rejects_path_traversal_and_bad_references() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        assert!(ArtifactRef::from_json(&json!({"sha256": "../other", "bytes": 0})).is_err());
        assert!(ArtifactRef::from_json(&json!({"sha256": "a".repeat(64), "bytes": -1})).is_err());
        assert!(
            ArtifactRef::from_json(&json!({"sha256": "a".repeat(64), "bytes": 1, "path": "x"}))
                .is_err()
        );
        assert!(
            store
                .verify(&ArtifactRef {
                    sha256: "../../x".into(),
                    bytes: 0
                })
                .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn canonical_verbatim_drive_root_is_checked_as_complete_components() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        let store = ArtifactStore::open(&canonical.join("objects")).unwrap();
        let reference = store.put_bytes(b"verbatim-root").unwrap();
        assert_eq!(store.read_bytes(&reference, 32).unwrap(), b"verbatim-root");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_object() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let reference = store.put_bytes(b"safe").unwrap();
        let path = store.path(&reference).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(dir.path().join("elsewhere"), &path).unwrap();
        assert!(store.verify(&reference).is_err());
        assert!(store.put_bytes(b"safe").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_symlinked_object_when_allowed() {
        use std::os::windows::fs::symlink_file;
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let reference = store.put_bytes(b"safe").unwrap();
        let path = store.path(&reference).unwrap();
        fs::remove_file(&path).unwrap();
        if symlink_file(dir.path().join("elsewhere"), &path).is_ok() {
            assert!(store.verify(&reference).is_err());
            assert!(store.put_bytes(b"safe").is_err());
        }
    }
}
