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
const MAX_ASR_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;
// Raw ASR text may expand sixfold when JSON escapes control characters.
const MAX_ASR_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ASR_ATTEMPTS: usize = 10_000;

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
    /// Immutable capture slots, including incomplete output awaiting recovery.
    /// These paths are declarations, not CAS identifiers or replay authority.
    pub asr_attempts: Vec<BackupObject>,
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

    /// Backup caller references plus declared ASR capture slots and their CAS
    /// closure. Capture can precede a failed DB commit, so DB references alone
    /// do not cover it. No CAS-wide scan or garbage collection is implied.
    pub fn backup_declaration(&self, references: &[ArtifactRef]) -> io::Result<BackupDeclaration> {
        let mut unique = BTreeMap::<String, ArtifactRef>::new();
        let (asr_attempts, captured_references) = self.asr_backup_inventory()?;
        for reference in references.iter().chain(captured_references.iter()) {
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
            asr_attempts,
        })
    }

    fn asr_backup_inventory(&self) -> io::Result<(Vec<BackupObject>, Vec<ArtifactRef>)> {
        let root = self.root.join("asr-attempts");
        reject_reparse_components(&root)?;
        match fs::symlink_metadata(&root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((vec![], vec![])),
            Err(error) => return Err(error),
            Ok(metadata) if !metadata.is_dir() => return Err(invalid("ASR attempts must be a directory")),
            Ok(_) => {}
        }
        let mut inventory = Vec::new();
        let mut references = Vec::new();
        let mut attempts = 0_usize;
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            attempts += 1;
            if attempts > MAX_ASR_ATTEMPTS {
                return Err(invalid("ASR backup attempt count exceeds limit"));
            }
            let attempt = entry.path();
            reject_reparse_components(&attempt)?;
            let attempt_key = entry.file_name().into_string().map_err(|_| invalid("Invalid ASR attempt name"))?;
            validate_ref(&ArtifactRef { sha256: attempt_key.clone(), bytes: 0 })?;
            if !fs::symlink_metadata(&attempt)?.is_dir() {
                return Err(invalid("ASR attempt must be a directory"));
            }
            for sidecar in fs::read_dir(&attempt)? {
                let sidecar = sidecar?;
                let name = sidecar.file_name().into_string().map_err(|_| invalid("Invalid ASR sidecar name"))?;
                if !valid_asr_sidecar_name(&name) {
                    return Err(invalid("Unexpected ASR sidecar member"));
                }
                let path = sidecar.path();
                let mut file = self.open_object(&path)?;
                let expected_bytes = file.metadata()?.len();
                if expected_bytes > MAX_ASR_SIDECAR_BYTES {
                    return Err(invalid("ASR sidecar exceeds byte limit"));
                }
                let mut bytes = Vec::new();
                Read::by_ref(&mut file).take(MAX_ASR_SIDECAR_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_ASR_SIDECAR_BYTES || bytes.len() as u64 != expected_bytes
                    || file.metadata()?.len() != expected_bytes {
                    return Err(invalid("ASR sidecar changed during backup declaration"));
                }
                inventory.push(BackupObject {
                    reference: ArtifactRef { sha256: format!("{:x}", Sha256::digest(&bytes)), bytes: expected_bytes },
                    relative_path: path.strip_prefix(&self.root).map_err(|_| invalid("ASR sidecar outside store"))?.to_owned(),
                });
                // Preserve torn captures as evidence. Only parsed declared
                // references add CAS closure; backup never admits ASR reuse.
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value.get("result").is_none() && value.get("segment").is_none() { continue; }
                    let binding = &value["binding"];
                    validate_asr_binding(binding)?;
                    if value["schemaVersion"] != 1 || format!("{:x}", Sha256::digest(json!([binding["companyId"], binding["manifestKey"]]).to_string().as_bytes())) != attempt_key {
                        return Err(invalid("ASR sidecar binding differs from declared attempt"));
                    }
                    if name == "full.json" {
                        if let Some(result) = value.get("result") {
                            let manifest = ArtifactRef::from_json(&result["manifest"])?;
                            let normalized = ArtifactRef::from_json(&result["normalizedOutput"])?;
                            let normalized_value = self.read_asr_backup_document(&normalized, binding, None)?;
                            if !normalized_value["audio"].is_object() { return Err(invalid("ASR full output needs audio")); }
                            references.push(normalized);
                            let manifest_value = self.read_asr_backup_document(&manifest, binding, None)?;
                            references.push(manifest);
                            if manifest_value["normalizedOutput"] != result["normalizedOutput"] { return Err(invalid("ASR full output differs from manifest")); }
                            references.push(ArtifactRef::from_json(&manifest_value["normalizedOutput"])?);
                            let segments = manifest_value["segments"].as_array().ok_or_else(|| invalid("ASR manifest needs segments"))?;
                            if segments.len() > 16 { return Err(invalid("ASR manifest segment count exceeds limit")); }
                            for segment in segments {
                                self.append_asr_segment_references(segment, binding, &mut references)?;
                            }
                        } else { return Err(invalid("ASR full sidecar needs result")); }
                    } else if let Some(segment) = value.get("segment") {
                        if segment["outputBinding"] != *binding || name != format!("segment-{:03}.json", segment["index"].as_u64().ok_or_else(|| invalid("ASR segment needs index"))?) {
                            return Err(invalid("ASR segment differs from declared slot"));
                        }
                        self.append_asr_segment_references(segment, binding, &mut references)?;
                    } else { return Err(invalid("ASR segment sidecar needs segment")); }
                }
            }
        }
        inventory.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok((inventory, references))
    }

    fn read_asr_backup_document(&self, reference: &ArtifactRef, binding: &Value, index: Option<u64>) -> io::Result<Value> {
        let value: Value = serde_json::from_slice(&self.read_bytes(reference, MAX_ASR_OUTPUT_BYTES)?).map_err(io::Error::other)?;
        if value["schemaVersion"] != 1 || value["binding"] != *binding || index.is_some_and(|index| value["index"] != index) {
            return Err(invalid("ASR CAS output binding differs from declaration"));
        }
        Ok(value)
    }

    fn append_asr_segment_references(&self, segment: &Value, binding: &Value, references: &mut Vec<ArtifactRef>) -> io::Result<()> {
        let original = &segment["outputBinding"];
        validate_asr_binding(original)?;
        // Recovered segments may retain an earlier fenced owner/attempt.
        for key in ["companyId", "verifiedFile", "specSha256", "segments", "durationMs"] {
            if original[key] != binding[key] { return Err(invalid("ASR segment immutable identity changed")); }
        }
        let index = segment["index"].as_u64().filter(|index| *index < 16).ok_or_else(|| invalid("ASR segment index exceeds limit"))?;
        for field in ["rawOutput", "normalizedOutput"] {
            let reference = ArtifactRef::from_json(&segment[field])?;
            let value = self.read_asr_backup_document(&reference, original, Some(index))?;
            if !value["text"].is_string() { return Err(invalid("ASR segment output needs text")); }
            references.push(reference);
        }
        Ok(())
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

fn valid_asr_sidecar_name(name: &str) -> bool {
    if name == "full.json" { return true; }
    name.strip_prefix("segment-").and_then(|name| name.strip_suffix(".json"))
        .is_some_and(|index| index.len() == 3 && index.bytes().all(|c| c.is_ascii_digit())
            && index.parse::<usize>().is_ok_and(|index| index < 16))
}

fn validate_asr_binding(binding: &Value) -> io::Result<()> {
    for key in ["companyId", "manifestKey", "attemptId", "owner", "specSha256"] {
        if binding[key].as_str().is_none_or(str::is_empty) { return Err(invalid("ASR capture binding is incomplete")); }
    }
    if binding["epoch"].as_u64().is_none() { return Err(invalid("ASR capture binding needs epoch")); }
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

    fn asr_binding(company: &str) -> Value {
        json!({"companyId":company,"manifestKey":"immutable-analysis-key","attemptId":"attempt-1","owner":"worker-1","epoch":1,
            "verifiedFile":{"sha256":"a".repeat(64),"bytes":1234,"receiptSha256":"b".repeat(64)},"specSha256":"c".repeat(64),
            "segments":[{"index":0,"startMs":0,"endMs":1000}],"durationMs":1000})
    }

    fn asr_segment(store: &ArtifactStore, binding: &Value) -> Value {
        let raw = store.put_bytes(json!({"schemaVersion":1,"binding":binding,"index":0,"text":"raw paid words"}).to_string().as_bytes()).unwrap();
        let normalized = store.put_bytes(json!({"schemaVersion":1,"binding":binding,"index":0,"text":"normalized paid words"}).to_string().as_bytes()).unwrap();
        json!({"index":0,"outputBinding":binding,"rawOutput":raw.to_json(),"normalizedOutput":normalized.to_json()})
    }

    fn asr_sidecar(store: &ArtifactStore, binding: &Value, name: &str, bytes: &[u8]) -> PathBuf {
        let key = format!("{:x}", Sha256::digest(json!([binding["companyId"],binding["manifestKey"]]).to_string().as_bytes()));
        let dir = store.root().join("asr-attempts").join(key);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);fs::write(&path, bytes).unwrap();path
    }

    #[test]
    fn backup_captures_asr_before_database_commit_and_excludes_file_metadata_roots() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let binding = asr_binding("company-a");
        let segment = asr_segment(&store, &binding);
        let bytes = json!({"schemaVersion":1,"binding":binding,"segment":segment}).to_string().into_bytes();
        let path = asr_sidecar(&store,&binding,"segment-000.json",&bytes);
        let declaration = store.backup_declaration(&[]).unwrap();
        assert_eq!(declaration.objects.len(),2);
        assert_eq!(declaration.asr_attempts.len(),1);
        assert_eq!(declaration.asr_attempts[0].relative_path.as_path(),path.strip_prefix(store.root()).unwrap());
        assert_eq!(declaration.asr_attempts[0].reference.bytes,bytes.len() as u64);
        assert_eq!(declaration.asr_attempts[0].reference.sha256,format!("{:x}",Sha256::digest(&bytes)));
        assert!(!declaration.objects.iter().any(|object|object.reference.sha256 == "a".repeat(64)));
        let raw = ArtifactRef::from_json(&segment["rawOutput"]).unwrap();
        fs::write(store.path(&raw).unwrap(), b"changed paid words").unwrap();
        assert!(store.backup_declaration(&[]).is_err());
    }

    #[test]
    fn backup_full_output_recovers_original_segment_closure_without_segment_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let original = asr_binding("company-a");
        let segment = asr_segment(&store,&original);
        let mut binding = original.clone();
        binding["manifestKey"] = json!("recovery-manifest");binding["attemptId"] = json!("attempt-2");binding["owner"] = json!("worker-2");binding["epoch"] = json!(2);
        let normalized = store.put_bytes(json!({"schemaVersion":1,"binding":binding,"audio":{"text":"complete"}}).to_string().as_bytes()).unwrap();
        let manifest = store.put_bytes(json!({"schemaVersion":1,"binding":binding,"segments":[segment],"normalizedOutput":normalized.to_json()}).to_string().as_bytes()).unwrap();
        let closure = json!({"schemaVersion":1,"binding":binding,"result":{"manifest":manifest.to_json(),"normalizedOutput":normalized.to_json()}});
        asr_sidecar(&store,&binding,"full.json",closure.to_string().as_bytes());
        let declaration = store.backup_declaration(&[manifest]).unwrap();
        assert_eq!(declaration.objects.len(),4);
        assert_eq!(declaration.asr_attempts.len(),1);
    }

    #[test]
    fn backup_bounds_sidecars_separately_from_json_escaped_paid_output() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let binding = asr_binding("company-a");
        let mut segment = asr_segment(&store, &binding);
        let escaped = json!({"schemaVersion":1,"binding":binding,"index":0,"text":"\0".repeat(1024*1024)}).to_string();
        assert!(escaped.len() as u64 > MAX_ASR_SIDECAR_BYTES);
        let raw = store.put_bytes(escaped.as_bytes()).unwrap();segment["rawOutput"] = raw.to_json();
        asr_sidecar(&store,&binding,"segment-000.json",json!({"schemaVersion":1,"binding":binding,"segment":segment}).to_string().as_bytes());
        let declaration = store.backup_declaration(&[]).unwrap();
        assert_eq!(declaration.objects.len(),2);assert_eq!(declaration.asr_attempts.len(),1);
    }

    #[test]
    fn backup_preserves_torn_asr_evidence_but_rejects_large_or_undeclared_slots() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let binding = asr_binding("company-a");
        let torn = asr_sidecar(&store,&binding,"segment-015.json",b"{\"binding\":");
        let declaration = store.backup_declaration(&[]).unwrap();
        assert!(declaration.objects.is_empty());assert_eq!(declaration.asr_attempts.len(),1);
        assert_eq!(declaration.asr_attempts[0].reference.bytes,11);
        let invalid_slot = torn.parent().unwrap().join("segment-016.json");
        fs::write(&invalid_slot,b"{}").unwrap();assert!(store.backup_declaration(&[]).is_err());fs::remove_file(invalid_slot).unwrap();
        let large = torn.parent().unwrap().join("full.json");
        File::create(&large).unwrap().set_len(MAX_ASR_SIDECAR_BYTES+1).unwrap();
        assert!(store.backup_declaration(&[]).is_err());
    }

    #[test]
    fn backup_rejects_cross_company_and_attempt_hash_retargeting() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let company_a = asr_binding("company-a");let company_b = asr_binding("company-b");
        let segment = asr_segment(&store,&company_b);
        let closure = json!({"schemaVersion":1,"binding":company_a,"segment":segment});
        let path = asr_sidecar(&store,&company_a,"segment-000.json",closure.to_string().as_bytes());
        assert!(store.backup_declaration(&[]).is_err());fs::remove_file(path).unwrap();
        let mut segment = asr_segment(&store,&company_b);segment["outputBinding"] = company_a.clone();
        let closure = json!({"schemaVersion":1,"binding":company_a,"segment":segment});
        let path = asr_sidecar(&store,&company_a,"segment-000.json",closure.to_string().as_bytes());
        assert!(store.backup_declaration(&[]).is_err());fs::remove_file(path).unwrap();
        let segment = asr_segment(&store,&company_a);
        let closure = json!({"schemaVersion":1,"binding":company_a,"segment":segment});
        asr_sidecar(&store,&company_b,"segment-000.json",closure.to_string().as_bytes());
        assert!(store.backup_declaration(&[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn backup_rejects_asr_sidecar_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("artifacts")).unwrap();
        let binding = asr_binding("company-a");
        let path = asr_sidecar(&store,&binding,"full.json",b"{");fs::remove_file(&path).unwrap();
        let external = dir.path().join("external.json");fs::write(&external,b"private").unwrap();
        std::os::unix::fs::symlink(external,path).unwrap();
        assert!(store.backup_declaration(&[]).is_err());
    }

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
