use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use serde::Serialize;
use time::OffsetDateTime;

use crate::env::EnvironmentService;
use crate::store::{derive_env_paths, display_path, supervisor_logs_dir};

const FOLLOW_POLL_INTERVAL_MS: u64 = 250;
const TAIL_READ_CHUNK_SIZE: usize = 8 * 1024;
const RETIRED_FILE_LIMIT: usize = 4;
const RETIRED_FILE_QUIET_POLLS: u8 = 4;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogComponentSummary {
    pub stream: String,
    pub source_kind: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSummary {
    pub env_name: String,
    pub stream: String,
    pub source_kind: String,
    pub path: String,
    pub tail_lines: Option<usize>,
    pub content: String,
    pub components: Vec<LogComponentSummary>,
}

#[derive(Clone, Debug)]
pub struct LogTarget {
    pub env_name: String,
    pub stream: String,
    pub source_kind: String,
    pub path: PathBuf,
}

pub struct LogService<'a> {
    env: &'a BTreeMap<String, String>,
    cwd: &'a Path,
}

impl<'a> LogService<'a> {
    pub fn new(env: &'a BTreeMap<String, String>, cwd: &'a Path) -> Self {
        Self { env, cwd }
    }

    pub fn read(
        &self,
        name: &str,
        stream: &str,
        tail_lines: Option<usize>,
    ) -> Result<LogSummary, String> {
        let targets = self.targets(name, stream)?;
        if targets.len() == 1 {
            let target = &targets[0];
            if !target.path.exists() {
                return Err(format!(
                    "{} log does not exist for env \"{}\": {}",
                    target.stream,
                    target.env_name,
                    display_path(&target.path)
                ));
            }

            return Ok(LogSummary {
                env_name: target.env_name.clone(),
                stream: target.stream.clone(),
                source_kind: target.source_kind.clone(),
                path: display_path(&target.path),
                tail_lines,
                content: read_log_text(&target.path, tail_lines)?,
                components: vec![LogComponentSummary {
                    stream: target.stream.clone(),
                    source_kind: target.source_kind.clone(),
                    path: display_path(&target.path),
                }],
            });
        }

        let mut contents = Vec::new();
        let mut components = Vec::new();
        for target in &targets {
            components.push(LogComponentSummary {
                stream: target.stream.clone(),
                source_kind: target.source_kind.clone(),
                path: display_path(&target.path),
            });
            if target.path.exists() {
                contents.push((
                    target.stream.clone(),
                    read_log_text(&target.path, tail_lines)?,
                ));
            }
        }
        if contents.is_empty() {
            return Err(format!(
                "no logs exist for env \"{}\" across stdout or stderr",
                name
            ));
        }

        Ok(LogSummary {
            env_name: name.to_string(),
            stream: "stdout + stderr".to_string(),
            source_kind: summarize_sources(&targets),
            path: "multiple".to_string(),
            tail_lines,
            content: merge_log_texts(contents, tail_lines),
            components,
        })
    }

    pub fn target(&self, name: &str, stream: &str) -> Result<LogTarget, String> {
        let stream = normalize_stream(stream)?.to_string();
        let meta = EnvironmentService::new(self.env, self.cwd).get(name)?;
        let env_paths = derive_env_paths(Path::new(&meta.root));
        let gateway_path = env_paths
            .state_dir
            .join("logs")
            .join(match stream.as_str() {
                "stdout" => "gateway.log",
                "stderr" => "gateway.err.log",
                _ => unreachable!("stream validated by normalize_stream"),
            });
        let supervisor_path =
            supervisor_logs_dir(self.env, self.cwd)?.join(format!("{}.{}.log", name, stream));

        let gateway_modified = modified_at(&gateway_path);
        let supervisor_modified = modified_at(&supervisor_path);

        let (source_kind, path) = match (gateway_modified, supervisor_modified) {
            (Some(gateway_time), Some(supervisor_time)) => {
                if supervisor_time > gateway_time {
                    ("service", supervisor_path)
                } else {
                    ("gateway", gateway_path)
                }
            }
            (Some(_), None) => ("gateway", gateway_path),
            (None, Some(_)) => ("service", supervisor_path),
            (None, None) => ("gateway", gateway_path),
        };

        Ok(LogTarget {
            env_name: name.to_string(),
            stream,
            source_kind: source_kind.to_string(),
            path,
        })
    }

    pub fn targets(&self, name: &str, stream: &str) -> Result<Vec<LogTarget>, String> {
        match normalize_stream(stream)? {
            "stdout" | "stderr" => Ok(vec![self.target(name, stream)?]),
            "all" => Ok(vec![
                self.target(name, "stdout")?,
                self.target(name, "stderr")?,
            ]),
            _ => unreachable!("stream validated by normalize_stream"),
        }
    }

    pub fn follow<W: std::io::Write>(
        &self,
        name: &str,
        stream: &str,
        tail_lines: Option<usize>,
        writer: &mut W,
    ) -> Result<LogTarget, String> {
        let stream = normalize_stream(stream)?;
        let targets = self.targets(name, stream)?;
        let merge_streams = stream == "all";
        self.follow_targets(name, targets, tail_lines, merge_streams, writer)?;
        if merge_streams {
            Ok(LogTarget {
                env_name: name.to_string(),
                stream: "all".to_string(),
                source_kind: "mixed".to_string(),
                path: PathBuf::new(),
            })
        } else {
            self.target(name, stream)
        }
    }

    fn follow_targets<W: Write>(
        &self,
        name: &str,
        targets: Vec<LogTarget>,
        tail_lines: Option<usize>,
        merge_streams: bool,
        writer: &mut W,
    ) -> Result<(), String> {
        let mut snapshots = Vec::new();
        let mut cursors = targets
            .into_iter()
            .map(FollowCursor::new)
            .collect::<Vec<_>>();
        for cursor in &mut cursors {
            if let Some(snapshot) = cursor.snapshot(tail_lines, merge_streams)? {
                snapshots.push(snapshot);
            }
        }
        if merge_streams && snapshots.is_empty() {
            return Err(format!(
                "no logs exist for env \"{}\" across stdout or stderr",
                name
            ));
        }
        if merge_streams {
            // Give the trailing record one poll to receive continuation lines,
            // then perform one global sort and tail decision for the startup batch.
            sleep(Duration::from_millis(FOLLOW_POLL_INTERVAL_MS));
            let bytes = collect_merged_follow_startup(&mut cursors, snapshots, tail_lines)?;
            if !bytes.is_empty() {
                writer
                    .write_all(&bytes)
                    .map_err(|error| error.to_string())?;
                writer.flush().map_err(|error| error.to_string())?;
            }
        } else if !snapshots.is_empty() {
            write_follow_snapshots(writer, snapshots, tail_lines, merge_streams)?;
        }

        loop {
            let mut records = Vec::new();
            let mut wrote = false;
            for cursor in &mut cursors {
                if !cursor.is_initialized() {
                    let Some(snapshot) = cursor.snapshot(tail_lines, merge_streams)? else {
                        continue;
                    };
                    if merge_streams {
                        records.extend(log_records(snapshot.bytes));
                    } else {
                        writer
                            .write_all(&snapshot.bytes)
                            .map_err(|error| error.to_string())?;
                        wrote |= !snapshot.bytes.is_empty();
                    }
                    continue;
                }
                let update = cursor.read_update()?;
                if merge_streams {
                    if let Some(update) = update {
                        records.extend(cursor.collect_complete_records(update));
                    } else {
                        records.extend(cursor.collect_quiet_records());
                    }
                } else if let Some(update) = update {
                    let bytes = cursor.single_stream_bytes(update);
                    writer
                        .write_all(&bytes)
                        .map_err(|error| error.to_string())?;
                    wrote |= !bytes.is_empty();
                }
            }

            if merge_streams && !records.is_empty() {
                let merged = merge_log_records(records);
                writer
                    .write_all(&merged)
                    .map_err(|error| error.to_string())?;
                wrote = true;
            }
            if wrote {
                writer.flush().map_err(|error| error.to_string())?;
            }

            sleep(Duration::from_millis(FOLLOW_POLL_INTERVAL_MS));
        }
    }
}

struct FollowCursor {
    target: LogTarget,
    offset: u64,
    identity: Option<FileIdentity>,
    modified: Option<std::time::SystemTime>,
    initialized: bool,
    pending: Vec<u8>,
    file: Option<File>,
    retired_files: VecDeque<RetiredFile>,
    single_line_open: bool,
    single_separator_pending: bool,
    #[cfg(test)]
    read_metrics: FollowReadMetrics,
}

impl FollowCursor {
    fn new(target: LogTarget) -> Self {
        Self {
            target,
            offset: 0,
            identity: None,
            modified: None,
            initialized: false,
            pending: Vec::new(),
            file: None,
            retired_files: VecDeque::new(),
            single_line_open: false,
            single_separator_pending: false,
            #[cfg(test)]
            read_metrics: FollowReadMetrics::default(),
        }
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn snapshot(
        &mut self,
        tail_lines: Option<usize>,
        hold_partial_record: bool,
    ) -> Result<Option<LogSnapshot>, String> {
        let Some((mut file, metadata)) = open_log(&self.target.path)? else {
            return Ok(None);
        };
        let (mut bytes, offset, _bytes_read) = read_log_snapshot_from(&mut file, tail_lines)?;
        #[cfg(test)]
        {
            self.read_metrics.snapshot_bytes += _bytes_read;
        }
        self.offset = offset;
        self.identity = Some(file_identity(&file, &metadata)?);
        self.modified = modified_time(&metadata);
        self.initialized = true;
        self.file = Some(file);
        self.single_line_open = bytes.last().is_some_and(|byte| *byte != b'\n');
        let trailing_timestamp = last_timestamp_start(&bytes);
        let should_hold_trailing =
            hold_partial_record && (!bytes.ends_with(b"\n") || trailing_timestamp.is_some());
        if should_hold_trailing {
            // Hold the trailing record for one poll so continuation lines can arrive
            // before another stream is merged. Single-stream follow stays immediate.
            let partial_start = trailing_timestamp.unwrap_or(0);
            self.pending.extend_from_slice(&bytes[partial_start..]);
            bytes.truncate(partial_start);
        }
        Ok(Some(LogSnapshot { bytes }))
    }

    fn read_update(&mut self) -> Result<Option<FollowUpdate>, String> {
        let Some((mut path_file, metadata)) = open_log(&self.target.path)? else {
            let mut segments = self.drain_retired_files()?;
            if self.file.is_some() {
                self.retire_current_file();
                let mut current_segments = self.drain_newest_retired_file()?;
                current_segments.extend(segments);
                segments = current_segments;
            }
            return Ok(FollowUpdate::from_segments(segments));
        };
        let identity = file_identity(&path_file, &metadata)?;
        let reattached = self.file.is_none() && self.reattach_retired_source(&identity);
        let mut segments = self.drain_retired_files()?;
        let Some(seen_identity) = self.identity.as_ref() else {
            let (bytes, offset) = self.read_range(&mut path_file, 0, metadata.len())?;
            self.adopt_file(path_file, offset, &metadata)?;
            if !bytes.is_empty() {
                segments.push(FollowSegment::append(bytes));
            }
            return Ok(FollowUpdate::from_segments(segments));
        };

        if seen_identity != &identity || (self.file.is_none() && !reattached) {
            // Keep the renamed handle for a bounded quiet window so delayed writer
            // handoff cannot drop the last records during rename-create rotation.
            if self.file.is_some() {
                self.retire_current_file();
                let mut current_segments = self.drain_newest_retired_file()?;
                current_segments.extend(segments);
                segments = current_segments;
            }
            self.seal_retired_sources();
            let (new_bytes, offset) = self.read_range(&mut path_file, 0, metadata.len())?;
            self.adopt_file(path_file, offset, &metadata)?;
            segments.push(FollowSegment::reset(new_bytes));
            return Ok(FollowUpdate::from_segments(segments));
        }

        // A same-size in-place change cannot be distinguished from a touch without
        // rescanning the file. Replaying is the bounded choice for this rare transition.
        let reset = metadata.len() < self.offset
            || (metadata.len() == self.offset && modified_time(&metadata) != self.modified);
        let start = if reset { 0 } else { self.offset };
        let (bytes, offset) = self.read_range(&mut path_file, start, metadata.len())?;
        self.adopt_file(path_file, offset, &metadata)?;

        if bytes.is_empty() && !reset {
            Ok(FollowUpdate::from_segments(segments))
        } else {
            let current = if reset {
                FollowSegment::reset(bytes)
            } else {
                FollowSegment::append(bytes)
            };
            let mut ordered = Vec::with_capacity(segments.len() + 1);
            ordered.push(current);
            ordered.extend(segments);
            Ok(FollowUpdate::from_segments(ordered))
        }
    }

    fn collect_complete_records(&mut self, update: FollowUpdate) -> Vec<TimedLogRecord> {
        let mut records = Vec::new();
        for segment in update.segments {
            if segment.reset_before && !self.pending.is_empty() {
                records.extend(log_records(std::mem::take(&mut self.pending)));
            }
            self.pending.extend_from_slice(&segment.bytes);
            records.extend(self.take_preceding_records());
        }
        records
    }

    fn collect_quiet_records(&mut self) -> Vec<TimedLogRecord> {
        if !self.pending.is_empty() && !self.has_shared_retired_source() {
            log_records(std::mem::take(&mut self.pending))
        } else {
            Vec::new()
        }
    }

    fn single_stream_bytes(&mut self, update: FollowUpdate) -> Vec<u8> {
        let mut output = Vec::new();
        for segment in update.segments {
            if segment.reset_before {
                self.single_separator_pending |= self.single_line_open;
            }
            self.append_single_stream_segment(&mut output, &segment.bytes);
        }
        output
    }

    fn append_single_stream_segment(&mut self, output: &mut Vec<u8>, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.single_separator_pending {
            if bytes.first() != Some(&b'\n') {
                output.push(b'\n');
            }
            self.single_separator_pending = false;
        }
        output.extend_from_slice(bytes);
        self.single_line_open = !bytes.ends_with(b"\n");
    }

    fn take_preceding_records(&mut self) -> Vec<TimedLogRecord> {
        let complete_len = last_timestamp_start(&self.pending).unwrap_or(0);
        if complete_len > 0 {
            let complete = self.pending.drain(..complete_len).collect::<Vec<_>>();
            log_records(complete)
        } else {
            Vec::new()
        }
    }

    fn adopt_file(
        &mut self,
        file: File,
        offset: u64,
        observed_metadata: &fs::Metadata,
    ) -> Result<(), String> {
        self.offset = offset;
        self.identity = Some(file_identity(&file, observed_metadata)?);
        self.modified = modified_time(observed_metadata);
        self.file = Some(file);
        Ok(())
    }

    fn retire_current_file(&mut self) {
        let Some(identity) = self.identity.clone() else {
            return;
        };
        let Some(file) = self.file.take() else {
            return;
        };
        if self.retired_files.len() == RETIRED_FILE_LIMIT {
            self.retired_files.pop_front();
        }
        self.retired_files.push_back(RetiredFile {
            file,
            identity,
            offset: self.offset,
            modified: self.modified,
            quiet_polls: 0,
            shares_pending: true,
        });
    }

    fn drain_retired_files(&mut self) -> Result<Vec<FollowSegment>, String> {
        let mut segments = Vec::new();
        let mut pending_source = VecDeque::new();
        let mut independent_sources = VecDeque::new();
        while let Some(retired) = self.retired_files.pop_front() {
            if retired.shares_pending {
                pending_source.push_back(retired);
            } else {
                independent_sources.push_back(retired);
            }
        }
        pending_source.extend(independent_sources);
        let mut retained = VecDeque::with_capacity(pending_source.len());
        while let Some(mut retired) = pending_source.pop_front() {
            let (file_segments, keep, _bytes_read) = drain_retired_file(&mut retired)?;
            #[cfg(test)]
            {
                self.read_metrics.range_bytes += _bytes_read;
            }
            segments.extend(file_segments);
            if keep {
                retained.push_back(retired);
            }
        }
        self.retired_files = retained;
        Ok(segments)
    }

    fn reattach_retired_source(&mut self, identity: &FileIdentity) -> bool {
        let Some(index) = self
            .retired_files
            .iter()
            .rposition(|retired| retired.shares_pending && &retired.identity == identity)
        else {
            return false;
        };
        let retired = self
            .retired_files
            .remove(index)
            .expect("retired source index remains valid");
        self.offset = retired.offset;
        self.modified = retired.modified;
        self.identity = Some(identity.clone());
        true
    }

    fn has_shared_retired_source(&self) -> bool {
        self.retired_files
            .iter()
            .any(|retired| retired.shares_pending)
    }

    fn seal_retired_sources(&mut self) {
        for retired in &mut self.retired_files {
            retired.shares_pending = false;
        }
    }

    fn drain_newest_retired_file(&mut self) -> Result<Vec<FollowSegment>, String> {
        let Some(mut retired) = self.retired_files.pop_back() else {
            return Ok(Vec::new());
        };
        let (segments, keep, _bytes_read) = drain_retired_file(&mut retired)?;
        #[cfg(test)]
        {
            self.read_metrics.range_bytes += _bytes_read;
        }
        if keep {
            self.retired_files.push_back(retired);
        }
        Ok(segments)
    }

    fn read_range(
        &mut self,
        file: &mut File,
        start: u64,
        end: u64,
    ) -> Result<(Vec<u8>, u64), String> {
        let (bytes, offset) = read_file_range(file, start, end)?;
        #[cfg(test)]
        {
            self.read_metrics.range_bytes += bytes.len() as u64;
        }
        Ok((bytes, offset))
    }
}

#[cfg(test)]
#[derive(Default)]
struct FollowReadMetrics {
    snapshot_bytes: u64,
    range_bytes: u64,
}

struct RetiredFile {
    file: File,
    identity: FileIdentity,
    offset: u64,
    modified: Option<std::time::SystemTime>,
    quiet_polls: u8,
    shares_pending: bool,
}

fn drain_retired_file(
    retired: &mut RetiredFile,
) -> Result<(Vec<FollowSegment>, bool, u64), String> {
    let metadata = retired.file.metadata().map_err(|error| error.to_string())?;
    let end = metadata.len();
    let reset = end < retired.offset
        || (end == retired.offset && modified_time(&metadata) != retired.modified);
    let start = if reset { 0 } else { retired.offset };
    let (bytes, offset) = read_file_range(&mut retired.file, start, end)?;
    let bytes_read = bytes.len() as u64;
    retired.offset = offset;
    retired.modified = modified_time(&metadata);
    let segments = if !bytes.is_empty() {
        retired.quiet_polls = 0;
        if retired.shares_pending {
            vec![if reset {
                FollowSegment::reset(bytes)
            } else {
                FollowSegment::append(bytes)
            }]
        } else {
            vec![
                FollowSegment::reset(bytes),
                FollowSegment::reset(Vec::new()),
            ]
        }
    } else if reset {
        retired.quiet_polls = 0;
        if retired.shares_pending {
            retired.shares_pending = false;
            vec![FollowSegment::reset(Vec::new())]
        } else {
            Vec::new()
        }
    } else {
        retired.quiet_polls += 1;
        Vec::new()
    };
    Ok((
        segments,
        retired.quiet_polls < RETIRED_FILE_QUIET_POLLS,
        bytes_read,
    ))
}

struct FollowUpdate {
    segments: Vec<FollowSegment>,
}

struct FollowSegment {
    bytes: Vec<u8>,
    reset_before: bool,
}

impl FollowUpdate {
    fn from_segments(segments: Vec<FollowSegment>) -> Option<Self> {
        (!segments.is_empty()).then_some(Self { segments })
    }

    #[cfg(test)]
    fn reset(bytes: Vec<u8>) -> Self {
        Self {
            segments: vec![FollowSegment::reset(bytes)],
        }
    }
}

impl FollowSegment {
    fn append(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            reset_before: false,
        }
    }

    fn reset(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            reset_before: true,
        }
    }
}

struct LogSnapshot {
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows {
        volume_serial_number: u32,
        file_index: u64,
    },
    #[cfg(not(any(unix, windows)))]
    Portable {
        created: Option<std::time::SystemTime>,
    },
}

#[cfg(unix)]
fn file_identity(_file: &File, metadata: &fs::Metadata) -> Result<FileIdentity, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(file: &File, _metadata: &fs::Metadata) -> Result<FileIdentity, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // The handle and output pointer remain valid for the duration of the call.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(FileIdentity::Windows {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File, metadata: &fs::Metadata) -> Result<FileIdentity, String> {
    Ok(FileIdentity::Portable {
        created: metadata.created().ok(),
    })
}

fn open_log(path: &Path) -> Result<Option<(File, fs::Metadata)>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    Ok(Some((file, metadata)))
}

fn read_file_range(file: &mut File, start: u64, end: u64) -> Result<(Vec<u8>, u64), String> {
    file.seek(SeekFrom::Start(start))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(end.saturating_sub(start))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let offset = start
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| "log cursor offset overflowed".to_string())?;
    Ok((bytes, offset))
}

fn modified_at(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn modified_time(metadata: &fs::Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

fn normalize_stream(stream: &str) -> Result<&str, String> {
    match stream {
        "stdout" | "stderr" | "all" => Ok(stream),
        _ => Err(format!("unsupported log stream: {stream}")),
    }
}

fn read_log_text(path: &Path, tail_lines: Option<usize>) -> Result<String, String> {
    let (mut file, _) =
        open_log(path)?.ok_or_else(|| format!("log does not exist: {}", display_path(path)))?;
    let (bytes, _, _) = read_log_snapshot_from(&mut file, tail_lines)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_log_snapshot_from(
    file: &mut File,
    tail_lines: Option<usize>,
) -> Result<(Vec<u8>, u64, u64), String> {
    match tail_lines {
        Some(limit) => read_log_tail_snapshot(file, limit),
        None => {
            let snapshot_offset = file.metadata().map_err(|error| error.to_string())?.len();
            file.seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            (&mut *file)
                .take(snapshot_offset)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            let offset = file.stream_position().map_err(|error| error.to_string())?;
            Ok((bytes, offset, offset))
        }
    }
}

fn read_log_tail_snapshot(
    file: &mut File,
    tail_lines: usize,
) -> Result<(Vec<u8>, u64, u64), String> {
    if tail_lines == 0 {
        let offset = file
            .seek(SeekFrom::End(0))
            .map_err(|error| error.to_string())?;
        return Ok((Vec::new(), offset, 0));
    }

    let mut position = file
        .seek(SeekFrom::End(0))
        .map_err(|error| error.to_string())?;
    let snapshot_offset = position;
    let mut bytes_read = 0_u64;
    let mut chunks = Vec::new();
    let mut newline_count = 0_usize;

    while position > 0 && newline_count <= tail_lines {
        let next_chunk = position.min(TAIL_READ_CHUNK_SIZE as u64) as usize;
        position -= next_chunk as u64;
        file.seek(SeekFrom::Start(position))
            .map_err(|error| error.to_string())?;

        let mut chunk = vec![0_u8; next_chunk];
        file.read_exact(&mut chunk)
            .map_err(|error| error.to_string())?;
        newline_count += chunk.iter().filter(|byte| **byte == b'\n').count();
        bytes_read += chunk.len() as u64;
        chunks.push(chunk);
    }

    let mut bytes = Vec::new();
    for mut chunk in chunks.into_iter().rev() {
        bytes.append(&mut chunk);
    }
    Ok((tail_bytes(&bytes, tail_lines), snapshot_offset, bytes_read))
}

fn tail_bytes(bytes: &[u8], tail_lines: usize) -> Vec<u8> {
    if tail_lines == 0 {
        return Vec::new();
    }
    let lines = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(tail_lines);
    lines[start..].concat()
}

fn summarize_sources(targets: &[LogTarget]) -> String {
    let mut kinds = targets
        .iter()
        .map(|target| target.source_kind.as_str())
        .collect::<Vec<_>>();
    kinds.sort_unstable();
    kinds.dedup();
    if kinds.len() == 1 {
        kinds[0].to_string()
    } else {
        "mixed".to_string()
    }
}

fn merge_log_texts(contents: Vec<(String, String)>, tail_lines: Option<usize>) -> String {
    let snapshots = contents
        .into_iter()
        .map(|(_stream, content)| LogSnapshot {
            bytes: content.into_bytes(),
        })
        .collect::<Vec<_>>();
    String::from_utf8(merge_log_snapshots(snapshots, tail_lines))
        .expect("merging valid UTF-8 preserves valid UTF-8")
}

fn write_follow_snapshots<W: Write>(
    writer: &mut W,
    snapshots: Vec<LogSnapshot>,
    tail_lines: Option<usize>,
    merge_streams: bool,
) -> Result<(), String> {
    let bytes = if merge_streams {
        merge_log_snapshots(snapshots, tail_lines)
    } else {
        snapshots
            .into_iter()
            .next()
            .map_or_else(Vec::new, |snapshot| snapshot.bytes)
    };
    if !bytes.is_empty() {
        writer
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn collect_merged_follow_startup(
    cursors: &mut [FollowCursor],
    snapshots: Vec<LogSnapshot>,
    tail_lines: Option<usize>,
) -> Result<Vec<u8>, String> {
    let mut records = snapshots
        .into_iter()
        .flat_map(|snapshot| log_records(snapshot.bytes))
        .collect::<Vec<_>>();
    for cursor in cursors {
        if !cursor.is_initialized()
            && let Some(snapshot) = cursor.snapshot(tail_lines, true)?
        {
            records.extend(log_records(snapshot.bytes));
        }
        if cursor.is_initialized() {
            if let Some(update) = cursor.read_update()? {
                records.extend(cursor.collect_complete_records(update));
            }
            records.extend(cursor.collect_quiet_records());
        }
    }
    Ok(merge_log_records_with_tail(records, tail_lines))
}

fn merge_log_snapshots(snapshots: Vec<LogSnapshot>, tail_lines: Option<usize>) -> Vec<u8> {
    let records = snapshots
        .into_iter()
        .flat_map(|snapshot| log_records(snapshot.bytes))
        .collect::<Vec<_>>();
    merge_log_records_with_tail(records, tail_lines)
}

fn merge_log_records_with_tail(records: Vec<TimedLogRecord>, tail_lines: Option<usize>) -> Vec<u8> {
    let mut output = merge_log_records(records);
    if let Some(limit) = tail_lines {
        output = tail_bytes(&output, limit);
    }
    output
}

fn log_records(bytes: Vec<u8>) -> Vec<TimedLogRecord> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut records = Vec::new();
    let mut current: Option<TimedLogRecord> = None;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let timestamp = parse_log_timestamp(line);
        if timestamp.is_some() {
            if let Some(record) = current.take() {
                records.push(record);
            }
            current = Some(TimedLogRecord::new(timestamp, line.to_vec()));
        } else if let Some(record) = &mut current {
            record.bytes.extend_from_slice(line);
        } else {
            current = Some(TimedLogRecord::new(None, line.to_vec()));
        }
    }
    if let Some(record) = current {
        records.push(record);
    }
    records
}

fn last_timestamp_start(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0;
    let mut last_timestamp_start = None;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if parse_log_timestamp(line).is_some() {
            last_timestamp_start = Some(offset);
        }
        offset += line.len();
    }
    last_timestamp_start
}

fn merge_log_records(mut records: Vec<TimedLogRecord>) -> Vec<u8> {
    records.sort_by(|left, right| match (left.timestamp, right.timestamp) {
        (Some(left_time), Some(right_time)) => left_time
            .cmp(&right_time)
            .then_with(|| left.sequence.cmp(&right.sequence)),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => left.sequence.cmp(&right.sequence),
    });
    let mut output = Vec::new();
    for record in records {
        output.extend_from_slice(&record.bytes);
        if !output.ends_with(b"\n") {
            output.push(b'\n');
        }
    }
    output
}

#[derive(Clone, Debug)]
struct TimedLogRecord {
    timestamp: Option<OffsetDateTime>,
    sequence: usize,
    bytes: Vec<u8>,
}

impl TimedLogRecord {
    fn new(timestamp: Option<OffsetDateTime>, bytes: Vec<u8>) -> Self {
        Self {
            timestamp,
            sequence: next_log_sequence(),
            bytes,
        }
    }
}

fn parse_log_timestamp(line: &[u8]) -> Option<OffsetDateTime> {
    let token_start = line.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let token_end = line[token_start..]
        .iter()
        .position(|byte| byte.is_ascii_whitespace())
        .map_or(line.len(), |offset| token_start + offset);
    let token = std::str::from_utf8(&line[token_start..token_end]).ok()?;
    OffsetDateTime::parse(token, &time::format_description::well_known::Rfc3339)
        .ok()
        .or_else(|| {
            time::format_description::parse_borrowed::<1>(
                "[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]",
            )
            .ok()
            .and_then(|format| OffsetDateTime::parse(token, &format).ok())
        })
}

fn next_log_sequence() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    NEXT_SEQUENCE.fetch_add(1, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::{
        FollowCursor, FollowUpdate, LogService, LogSnapshot, LogTarget, merge_log_snapshots,
        merge_log_texts, tail_bytes,
    };
    use crate::store::ensure_dir;
    use std::collections::BTreeMap;
    use std::fs::{self, OpenOptions};
    use std::io::{Seek, Write};
    use std::path::{Path, PathBuf};
    use std::thread::sleep;
    use std::time::Duration;

    fn update_bytes(update: &FollowUpdate) -> Vec<u8> {
        update
            .segments
            .iter()
            .flat_map(|segment| segment.bytes.iter().copied())
            .collect()
    }

    fn update_has_reset(update: &FollowUpdate) -> bool {
        update.segments.iter().any(|segment| segment.reset_before)
    }

    fn collect_after_quiet(
        cursor: &mut FollowCursor,
        update: FollowUpdate,
    ) -> Vec<super::TimedLogRecord> {
        let mut records = cursor.collect_complete_records(update);
        records.extend(cursor.collect_quiet_records());
        records
    }

    #[test]
    fn tail_bytes_keeps_the_last_requested_lines() {
        assert_eq!(tail_bytes(b"one\ntwo\nthree\n", 2), b"two\nthree\n");
        assert_eq!(tail_bytes(b"one\ntwo\nthree", 1), b"three");
    }

    #[test]
    fn merge_log_texts_orders_stdout_and_stderr_by_timestamp() {
        let merged = merge_log_texts(
            vec![
                (
                    "stdout".to_string(),
                    concat!(
                        "2026-04-20T00:13:45.497+01:00 [gateway] one\n",
                        "2026-04-20T00:13:47.497+01:00 [gateway] three\n"
                    )
                    .to_string(),
                ),
                (
                    "stderr".to_string(),
                    "2026-04-20T00:13:46.497+01:00 error gateway two\n".to_string(),
                ),
            ],
            Some(10),
        );
        assert_eq!(
            merged,
            concat!(
                "2026-04-20T00:13:45.497+01:00 [gateway] one\n",
                "2026-04-20T00:13:46.497+01:00 error gateway two\n",
                "2026-04-20T00:13:47.497+01:00 [gateway] three\n",
            )
        );
    }

    #[test]
    fn merge_log_snapshots_keeps_multiline_records_with_their_parent() {
        let merged = merge_log_snapshots(
            vec![
                LogSnapshot {
                    bytes: concat!(
                        "2026-04-20T00:13:45Z [gateway] request failed\n",
                        "    at first frame\n",
                        "    at second frame\n",
                        "2026-04-20T00:13:47Z [gateway] recovered\n"
                    )
                    .as_bytes()
                    .to_vec(),
                },
                LogSnapshot {
                    bytes: b"2026-04-20T00:13:46Z error worker retrying\n".to_vec(),
                },
            ],
            None,
        );
        assert_eq!(
            merged,
            concat!(
                "2026-04-20T00:13:45Z [gateway] request failed\n",
                "    at first frame\n",
                "    at second frame\n",
                "2026-04-20T00:13:46Z error worker retrying\n",
                "2026-04-20T00:13:47Z [gateway] recovered\n"
            )
            .as_bytes()
        );
    }

    #[test]
    fn merge_log_snapshots_separates_unterminated_records_and_preserves_bytes() {
        let merged = merge_log_snapshots(
            vec![
                LogSnapshot {
                    bytes: b"2026-04-20T00:13:46Z [gateway] invalid \xff".to_vec(),
                },
                LogSnapshot {
                    bytes: b"2026-04-20T00:13:45Z error worker first\n".to_vec(),
                },
            ],
            None,
        );
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z error worker first\n\
              2026-04-20T00:13:46Z [gateway] invalid \xff\n"
        );
    }

    #[test]
    fn merge_log_snapshots_ignores_empty_sources() {
        let merged = merge_log_snapshots(
            vec![
                LogSnapshot { bytes: Vec::new() },
                LogSnapshot {
                    bytes: b"2026-04-20T00:13:45Z info present\n".to_vec(),
                },
            ],
            None,
        );
        assert_eq!(merged, b"2026-04-20T00:13:45Z info present\n");
    }

    #[test]
    fn merge_log_snapshots_recognizes_indented_timestamps() {
        let merged = merge_log_snapshots(
            vec![
                LogSnapshot {
                    bytes: b"  2026-04-20T00:13:46Z info indented\n".to_vec(),
                },
                LogSnapshot {
                    bytes: b"2026-04-20T00:13:45Z info first\n".to_vec(),
                },
            ],
            None,
        );
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z info first\n  2026-04-20T00:13:46Z info indented\n"
        );
    }

    #[test]
    fn merge_log_snapshots_totally_orders_dated_and_undated_records() {
        let merged = merge_log_snapshots(
            vec![
                LogSnapshot {
                    bytes: b"2026-04-20T00:13:47Z info later\n".to_vec(),
                },
                LogSnapshot {
                    bytes: b"undated record\n2026-04-20T00:13:45Z info earlier\n".to_vec(),
                },
            ],
            None,
        );
        assert_eq!(
            merged,
            b"undated record\n\
              2026-04-20T00:13:45Z info earlier\n\
              2026-04-20T00:13:47Z info later\n"
        );
    }

    #[test]
    fn tail_bytes_preserves_invalid_utf8() {
        assert_eq!(tail_bytes(b"first\nsecond \xff\n", 1), b"second \xff\n");
    }

    #[test]
    fn follow_cursor_buffers_split_records_as_bytes() {
        let root = test_root("cursor-split");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(None, false).unwrap().unwrap();
        assert_eq!(snapshot.bytes, b"initial\n");

        append(&path, b"2026-04-20T00:13:45Z info split \xe2");
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        append(&path, b"\x82\xac\n");
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(merged, "2026-04-20T00:13:45Z info split €\n".as_bytes());
        assert_eq!(cursor.offset, fs::metadata(&path).unwrap().len());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_buffers_the_whole_partial_multiline_record() {
        let root = test_root("cursor-multiline-partial");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, true).unwrap().unwrap();

        append(
            &path,
            b"2026-04-20T00:13:45Z error request failed\n    partial frame",
        );
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        append(&path, b" completed\n");
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(
            merged,
            concat!(
                "2026-04-20T00:13:45Z error request failed\n",
                "    partial frame completed\n"
            )
            .as_bytes()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merged_follow_waits_one_quiet_poll_for_continuation_lines() {
        let root = test_root("cursor-multiline-grace");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, true).unwrap().unwrap();

        append(&path, b"2026-04-20T00:13:45Z error request failed\n");
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        append(&path, b"    at delayed frame\n");
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());
        let merged = super::merge_log_records(cursor.collect_quiet_records());
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z error request failed\n    at delayed frame\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merged_follow_flushes_an_unterminated_record_after_one_quiet_poll() {
        let root = test_root("cursor-unterminated-grace");
        let path = root.join("gateway.log");
        fs::write(&path, b"2026-04-20T00:13:45Z final record").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(None, true).unwrap().unwrap();
        assert!(snapshot.bytes.is_empty());

        let merged = super::merge_log_records(cursor.collect_quiet_records());
        assert_eq!(merged, b"2026-04-20T00:13:45Z final record\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merged_follow_startup_globally_orders_and_tails_held_records() {
        let root = test_root("cursor-startup-order");
        let stdout_path = root.join("stdout.log");
        let stderr_path = root.join("stderr.log");
        fs::write(&stdout_path, b"2026-04-20T00:13:20Z stdout later\n").unwrap();
        fs::write(&stderr_path, b"2026-04-20T00:13:10Z stderr earlier\n").unwrap();

        let mut cursors = vec![
            FollowCursor::new(log_target(stdout_path.clone())),
            FollowCursor::new(log_target(stderr_path.clone())),
        ];
        let snapshots = cursors
            .iter_mut()
            .map(|cursor| cursor.snapshot(None, true).unwrap().unwrap())
            .collect::<Vec<_>>();
        let merged = super::collect_merged_follow_startup(&mut cursors, snapshots, None).unwrap();
        assert_eq!(
            merged,
            b"2026-04-20T00:13:10Z stderr earlier\n\
              2026-04-20T00:13:20Z stdout later\n"
        );

        let mut cursors = vec![
            FollowCursor::new(log_target(stdout_path.clone())),
            FollowCursor::new(log_target(stderr_path.clone())),
        ];
        let snapshots = cursors
            .iter_mut()
            .map(|cursor| cursor.snapshot(Some(1), true).unwrap().unwrap())
            .collect::<Vec<_>>();
        let tailed =
            super::collect_merged_follow_startup(&mut cursors, snapshots, Some(1)).unwrap();
        assert_eq!(tailed, b"2026-04-20T00:13:20Z stdout later\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_keeps_undated_partial_multiline_records_together() {
        let root = test_root("cursor-undated-multiline-partial");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, true).unwrap().unwrap();

        append(&path, b"frame one\nframe two");
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        append(&path, b" completed\n");
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(merged, b"frame one\nframe two completed\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merged_follow_retains_a_partial_startup_record() {
        let root = test_root("cursor-startup-partial");
        let path = root.join("gateway.log");
        fs::write(
            &path,
            concat!(
                "2026-04-20T00:13:44Z info previous\n",
                "2026-04-20T00:13:45Z error failed\n",
                "    split "
            )
            .as_bytes()
            .iter()
            .copied()
            .chain([0xe2])
            .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(None, true).unwrap().unwrap();
        assert_eq!(snapshot.bytes, b"2026-04-20T00:13:44Z info previous\n");

        append(&path, b"\x82\xac\n");
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(
            merged,
            concat!("2026-04-20T00:13:45Z error failed\n", "    split €\n").as_bytes()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delayed_follow_snapshot_applies_the_requested_tail() {
        let root = test_root("cursor-delayed-tail");
        let path = root.join("gateway.log");
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        assert!(cursor.snapshot(Some(1), false).unwrap().is_none());

        fs::write(&path, b"first\nsecond\nthird\n").unwrap();
        let snapshot = cursor.snapshot(Some(1), false).unwrap().unwrap();
        assert_eq!(snapshot.bytes, b"third\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_detects_replacement_and_separates_pending_bytes() {
        let root = test_root("cursor-replace");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        append(&path, b"old partial");
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        fs::rename(&path, root.join("gateway.log.1")).unwrap();
        fs::write(&path, b"2026-04-20T00:13:46Z info new file\n").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        assert!(update_has_reset(&update));
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(merged, b"old partial\n2026-04-20T00:13:46Z info new file\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_drains_a_renamed_file_before_reading_its_replacement() {
        let root = test_root("cursor-rename-drain");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, true).unwrap().unwrap();

        append(&path, b"2026-04-20T00:13:45Z info old final\n");
        fs::rename(&path, root.join("gateway.log.1")).unwrap();
        fs::write(&path, b"2026-04-20T00:13:46Z info new first\n").unwrap();

        let update = cursor.read_update().unwrap().unwrap();
        assert!(update_has_reset(&update));
        assert_eq!(
            update_bytes(&update),
            b"2026-04-20T00:13:45Z info old final\n\
              2026-04-20T00:13:46Z info new first\n"
        );
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z info old final\n\
              2026-04-20T00:13:46Z info new first\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_drains_late_writes_across_a_missing_rotation_gap() {
        let root = test_root("cursor-missing-rotation-drain");
        let path = root.join("gateway.log");
        let rotated_path = root.join("gateway.log.1");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, true).unwrap().unwrap();

        fs::rename(&path, &rotated_path).unwrap();
        assert!(cursor.read_update().unwrap().is_none());

        append(&rotated_path, b"2026-04-20T00:13:45Z info late old write\n");
        fs::write(&path, b"2026-04-20T00:13:46Z info new first\n").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z info late old write\n\
              2026-04-20T00:13:46Z info new first\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_reattaches_a_source_that_returns_after_a_missing_gap() {
        let root = test_root("cursor-missing-reattach");
        let path = root.join("gateway.log");
        let rotated_path = root.join("gateway.log.1");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        fs::rename(&path, &rotated_path).unwrap();
        assert!(cursor.read_update().unwrap().is_none());
        append(&rotated_path, b"late write\n");
        fs::rename(&rotated_path, &path).unwrap();

        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"late write\n");
        assert!(cursor.retired_files.is_empty());
        assert!(cursor.read_update().unwrap().is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_drains_late_writes_after_the_replacement_appears() {
        let root = test_root("cursor-post-replacement-drain");
        let path = root.join("gateway.log");
        let rotated_path = root.join("gateway.log.1");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        fs::rename(&path, &rotated_path).unwrap();
        fs::write(&path, b"new first\n").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"new first\n");
        assert_eq!(cursor.retired_files.len(), 1);

        append(&rotated_path, b"late old write\n");
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"late old write\n");

        for _ in 0..super::RETIRED_FILE_QUIET_POLLS {
            assert!(cursor.read_update().unwrap().is_none());
        }
        assert!(cursor.retired_files.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn late_retired_writes_preserve_single_and_merged_source_boundaries() {
        let root = test_root("cursor-retired-boundaries");
        let single_path = root.join("single.log");
        let single_rotated = root.join("single.log.1");
        fs::write(&single_path, b"initial\n").unwrap();
        let mut single = FollowCursor::new(log_target(single_path.clone()));
        single.snapshot(None, false).unwrap().unwrap();

        fs::rename(&single_path, &single_rotated).unwrap();
        fs::write(&single_path, b"new partial").unwrap();
        let update = single.read_update().unwrap().unwrap();
        assert_eq!(single.single_stream_bytes(update), b"new partial");

        append(&single_path, b" continued\n");
        append(&single_rotated, b"old late\n");
        let update = single.read_update().unwrap().unwrap();
        assert_eq!(
            single.single_stream_bytes(update),
            b" continued\nold late\n"
        );

        let merged_path = root.join("merged.log");
        let merged_rotated = root.join("merged.log.1");
        fs::write(&merged_path, b"initial\n").unwrap();
        let mut merged = FollowCursor::new(log_target(merged_path.clone()));
        merged.snapshot(None, true).unwrap().unwrap();

        fs::rename(&merged_path, &merged_rotated).unwrap();
        fs::write(&merged_path, b"2026-04-20T00:13:46Z new partial").unwrap();
        let update = merged.read_update().unwrap().unwrap();
        assert!(merged.collect_complete_records(update).is_empty());

        append(&merged_path, b" continued\n");
        append(&merged_rotated, b"2026-04-20T00:13:45Z old late\n");
        let update = merged.read_update().unwrap().unwrap();
        let records = merged.collect_complete_records(update);
        assert_eq!(records.len(), 2);
        assert_eq!(
            super::merge_log_records(records),
            b"2026-04-20T00:13:45Z old late\n\
              2026-04-20T00:13:46Z new partial continued\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn independent_empty_resets_do_not_split_the_current_source() {
        let root = test_root("cursor-empty-retired-reset");
        let path = root.join("gateway.log");
        let rotated_path = root.join("gateway.log.1");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, true).unwrap().unwrap();

        fs::rename(&path, &rotated_path).unwrap();
        fs::write(&path, b"2026-04-20T00:13:46Z new partial").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        fs::write(&rotated_path, b"").unwrap();
        append(&path, b" continued");
        let update = cursor.read_update().unwrap().unwrap();
        assert!(cursor.collect_complete_records(update).is_empty());

        append(&path, b" to completion\n");
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(
            merged,
            b"2026-04-20T00:13:46Z new partial continued to completion\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_expires_a_handle_while_the_path_is_missing() {
        let root = test_root("cursor-missing-expiry");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        fs::remove_file(&path).unwrap();
        for _ in 0..super::RETIRED_FILE_QUIET_POLLS {
            assert!(cursor.read_update().unwrap().is_none());
            assert!(cursor.retired_files.len() <= super::RETIRED_FILE_LIMIT);
        }
        assert!(cursor.file.is_none());
        assert!(cursor.retired_files.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_caps_retired_file_state() {
        let root = test_root("cursor-retired-cap");
        let mut cursor = FollowCursor::new(log_target(root.join("gateway.log")));

        for index in 0..(super::RETIRED_FILE_LIMIT + 2) {
            let path = root.join(format!("gateway.log.{index}"));
            fs::write(&path, b"log\n").unwrap();
            let file = fs::File::open(path).unwrap();
            let metadata = file.metadata().unwrap();
            cursor.identity = Some(super::file_identity(&file, &metadata).unwrap());
            cursor.file = Some(file);
            cursor.offset = 4;
            cursor.retire_current_file();
        }

        assert_eq!(cursor.retired_files.len(), super::RETIRED_FILE_LIMIT);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_detects_truncation_while_the_path_is_missing() {
        let root = test_root("cursor-missing-truncate");
        let path = root.join("gateway.log");
        let rotated_path = root.join("gateway.log.1");
        fs::write(&path, b"2026-04-20T00:13:45Z info old partial").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(None, true).unwrap().unwrap();
        assert!(snapshot.bytes.is_empty());

        fs::rename(&path, &rotated_path).unwrap();
        assert!(cursor.read_update().unwrap().is_none());
        fs::write(&rotated_path, b"2026-04-20T00:13:46Z info rewritten\n").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        assert!(update_has_reset(&update));
        let mut records = cursor.collect_complete_records(update);
        for _ in 1..super::RETIRED_FILE_QUIET_POLLS {
            assert!(cursor.read_update().unwrap().is_none());
            assert!(cursor.collect_quiet_records().is_empty());
        }
        assert!(cursor.read_update().unwrap().is_none());
        records.extend(cursor.collect_quiet_records());
        let merged = super::merge_log_records(records);
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z info old partial\n\
              2026-04-20T00:13:46Z info rewritten\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn single_stream_follow_separates_rotated_unterminated_records() {
        let root = test_root("cursor-single-rotation-boundary");
        let path = root.join("gateway.log");
        fs::write(&path, b"initial\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        append(&path, b"old partial");
        fs::rename(&path, root.join("gateway.log.1")).unwrap();
        fs::write(&path, b"new record\n").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(
            cursor.single_stream_bytes(update),
            b"old partial\nnew record\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn single_stream_follow_does_not_duplicate_a_replacement_newline() {
        let root = test_root("cursor-single-newline-boundary");
        let path = root.join("gateway.log");
        fs::write(&path, b"old partial").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        assert_eq!(
            cursor.single_stream_bytes(FollowUpdate::reset(b"\nnew record\n".to_vec())),
            b"\nnew record\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_preserves_pending_records_while_the_path_is_missing() {
        let root = test_root("cursor-missing");
        let path = root.join("gateway.log");
        let rotated_path = root.join("gateway.log.1");
        fs::write(&path, b"2026-04-20T00:13:45Z error final failure").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(None, true).unwrap().unwrap();
        assert!(snapshot.bytes.is_empty());

        fs::rename(&path, &rotated_path).unwrap();
        assert!(cursor.read_update().unwrap().is_none());
        assert!(cursor.collect_quiet_records().is_empty());
        append(&rotated_path, b" recovered\n");
        fs::write(&path, b"2026-04-20T00:13:46Z info new file\n").unwrap();
        let update = cursor.read_update().unwrap().unwrap();
        let merged = super::merge_log_records(collect_after_quiet(&mut cursor, update));
        assert_eq!(
            merged,
            b"2026-04-20T00:13:45Z error final failure recovered\n\
              2026-04-20T00:13:46Z info new file\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_does_not_reapply_tail_after_a_missing_rotation_gap() {
        let root = test_root("cursor-rotation-gap");
        let path = root.join("gateway.log");
        fs::write(&path, b"old\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(Some(0), false).unwrap().unwrap();
        assert!(snapshot.bytes.is_empty());

        fs::remove_file(&path).unwrap();
        assert!(cursor.read_update().unwrap().is_none());
        fs::write(&path, b"new first\nnew second\n").unwrap();
        assert!(cursor.is_initialized());
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"new first\nnew second\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_reads_from_the_actual_file_length() {
        let root = test_root("cursor-offset");
        let path = root.join("gateway.log");
        fs::write(&path, b"first\n").unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        append(&path, b"second\n");
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"second\n");
        assert_eq!(cursor.offset, fs::metadata(&path).unwrap().len());

        append(&path, b"third\n");
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"third\n");
        assert_eq!(cursor.offset, fs::metadata(&path).unwrap().len());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_tail_snapshot_reads_only_the_tail_window() {
        let root = test_root("cursor-tail-read-budget");
        let path = root.join("gateway.log");
        let content = "line\n".repeat(500_000);
        fs::write(&path, &content).unwrap();

        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let snapshot = cursor.snapshot(Some(10), false).unwrap().unwrap();
        assert_eq!(snapshot.bytes, b"line\n".repeat(10));
        assert!(cursor.read_metrics.snapshot_bytes <= super::TAIL_READ_CHUNK_SIZE as u64);
        assert!(cursor.read_metrics.snapshot_bytes < content.len() as u64);

        let mut full_cursor = FollowCursor::new(log_target(path.clone()));
        full_cursor.snapshot(None, false).unwrap().unwrap();
        assert_eq!(
            full_cursor.read_metrics.snapshot_bytes,
            content.len() as u64
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_reads_only_new_bytes_for_unchanged_or_growing_files() {
        let root = test_root("cursor-steady-state-reads");
        let path = root.join("gateway.log");
        fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();
        cursor.read_metrics = Default::default();

        assert!(cursor.read_update().unwrap().is_none());
        assert_eq!(cursor.read_metrics.snapshot_bytes, 0);
        assert_eq!(cursor.read_metrics.range_bytes, 0);

        append(&path, b"appended\n");
        let update = cursor.read_update().unwrap().unwrap();
        assert_eq!(update_bytes(&update), b"appended\n");
        assert_eq!(cursor.read_metrics.snapshot_bytes, 0);
        assert_eq!(cursor.read_metrics.range_bytes, b"appended\n".len() as u64);

        assert!(cursor.read_update().unwrap().is_none());
        assert_eq!(cursor.read_metrics.snapshot_bytes, 0);
        assert_eq!(cursor.read_metrics.range_bytes, b"appended\n".len() as u64);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_resets_after_a_same_size_rewrite() {
        let root = test_root("cursor-rewrite");
        let path = root.join("gateway.log");
        let mut original = b"old-prefix\n".to_vec();
        original.extend(std::iter::repeat_n(b'x', 64));
        fs::write(&path, &original).unwrap();
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        cursor.snapshot(None, false).unwrap().unwrap();

        let mut rewritten = b"new-prefix\n".to_vec();
        rewritten.extend(std::iter::repeat_n(b'x', 64));
        let previous = cursor.modified;
        rewrite_with_new_modified_time(&path, &rewritten, previous);
        let update = cursor.read_update().unwrap().unwrap();
        assert!(update_has_reset(&update));
        assert_eq!(update_bytes(&update), rewritten);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follow_cursor_checkpoints_metadata_from_before_the_read() {
        let root = test_root("cursor-metadata-race");
        let path = root.join("gateway.log");
        fs::write(&path, b"old record\n").unwrap();
        let (mut file, metadata) = super::open_log(&path).unwrap().unwrap();
        let previous = metadata.modified().ok();

        rewrite_with_new_modified_time(&path, b"new record\n", previous);
        let mut cursor = FollowCursor::new(log_target(path.clone()));
        let offset = metadata.len();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        cursor.adopt_file(file, offset, &metadata).unwrap();

        let update = cursor.read_update().unwrap().unwrap();
        assert!(update_has_reset(&update));
        assert_eq!(update_bytes(&update), b"new record\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn target_prefers_the_newer_service_log_when_gateway_log_is_stale() {
        let root = std::env::temp_dir().join(format!(
            "ocm-log-target-{}-{}",
            std::process::id(),
            crate::store::now_utc().unix_timestamp_nanos()
        ));
        let cwd = root.join("workspace");
        let ocm_home = root.join("ocm-home");
        let env_root = ocm_home.join("envs/demo");
        ensure_dir(&cwd).unwrap();
        ensure_dir(&env_root.join(".openclaw/logs")).unwrap();
        ensure_dir(&ocm_home.join("supervisor/logs")).unwrap();
        fs::write(
            ocm_home.join("envs.json"),
            format!(
                "{{\"kind\":\"ocm-env-registry\",\"envs\":[{{\"kind\":\"ocm-env\",\"name\":\"demo\",\"root\":\"{}\",\"gatewayPort\":18789,\"serviceEnabled\":false,\"serviceRunning\":false,\"protected\":false,\"createdAt\":\"1970-01-01T00:00:00Z\",\"updatedAt\":\"1970-01-01T00:00:00Z\"}}]}}",
                env_root.display()
            ),
        )
        .unwrap();
        fs::write(env_root.join(".openclaw/logs/gateway.log"), "hello\n").unwrap();
        sleep(Duration::from_millis(1100));
        fs::write(
            ocm_home.join("supervisor/logs/demo.stdout.log"),
            "newer service output\n",
        )
        .unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert("OCM_HOME".to_string(), ocm_home.display().to_string());
        let target = LogService::new(&env, Path::new(&cwd))
            .target("demo", "stdout")
            .unwrap();
        assert_eq!(target.source_kind, "service");
        assert!(target.path.ends_with("supervisor/logs/demo.stdout.log"));

        let _ = fs::remove_dir_all(&root);
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ocm-log-{label}-{}-{}",
            std::process::id(),
            crate::store::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn log_target(path: PathBuf) -> LogTarget {
        LogTarget {
            env_name: "demo".to_string(),
            stream: "stdout".to_string(),
            source_kind: "gateway".to_string(),
            path,
        }
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn rewrite_with_new_modified_time(
        path: &Path,
        bytes: &[u8],
        previous: Option<std::time::SystemTime>,
    ) {
        for _ in 0..200 {
            fs::write(path, bytes).unwrap();
            if fs::metadata(path).unwrap().modified().ok() != previous {
                return;
            }
            sleep(Duration::from_millis(10));
        }
        panic!("filesystem modification time did not advance");
    }
}
