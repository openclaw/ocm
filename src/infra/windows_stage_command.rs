//! Native launch data for the private runtime-stage command builders.
//!
//! This is deliberately not a general `Command` conversion. Its callers use
//! regular arguments, inherited environment plus set/remove deltas, optional
//! cwd, null stdin and captured stdout/stderr. They must not use `env_clear`,
//! `raw_arg`, custom creation flags or other platform-specific command options:
//! the public `Command` getters cannot recover those settings.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::Globalization::CompareStringOrdinal;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW, GetFullPathNameW, INVALID_FILE_ATTRIBUTES,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};

const QUOTE: u16 = b'"' as u16;
const SLASH: u16 = b'\\' as u16;

pub(crate) struct PreparedCommand {
    pub(crate) application: Vec<u16>,
    pub(crate) command_line: Vec<u16>,
    pub(crate) environment: Vec<u16>,
    pub(crate) cwd: Option<Vec<u16>>,
}

impl PreparedCommand {
    /// Prepare only an owner-built stage command satisfying the module contract.
    /// Every returned string is NUL-terminated; environment has two final NULs.
    pub(crate) fn from_stage_command(command: &Command) -> io::Result<Self> {
        let mut environment = Environment::default();
        for (key, value) in std::env::vars_os() {
            environment.set(&key, Some(&value))?;
        }
        let parent_path = environment.get(OsStr::new("PATH"))?;
        let mut changed_path = false;
        for (key, value) in command.get_envs() {
            changed_path |=
                compare_keys(&units(key)?, &units(OsStr::new("PATH"))?)? == Ordering::Equal;
            environment.set(key, value)?;
        }
        let child_path = if changed_path {
            environment.get(OsStr::new("PATH"))?
        } else {
            None
        };
        let resolved = resolve_program(
            command.get_program(),
            parent_path.as_deref(),
            child_path.as_deref(),
        )?;
        let args = command
            .get_args()
            .map(units)
            .collect::<io::Result<Vec<_>>>()?;
        let (application, mut command_line) = if is_batch(&resolved) {
            let application = application_path(&system_directory()?.join("cmd.exe"))?;
            (
                application,
                batch_command_line(&resolved[..resolved.len() - 1], &args)?,
            )
        } else {
            let program = units(command.get_program())?;
            (resolved, executable_command_line(&program, &args)?)
        };
        command_line.push(0);
        if command_line.len() > 32_767 {
            return Err(invalid(
                "runtime stage command line exceeds the Windows limit",
            ));
        }
        let cwd = command
            .get_current_dir()
            .map(|path| conventional_path(terminated(path.as_os_str())?, false))
            .transpose()?;
        Ok(Self {
            application,
            command_line,
            environment: environment.block(),
            cwd,
        })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn units(value: &OsStr) -> io::Result<Vec<u16>> {
    let value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("Windows process strings cannot contain NUL"));
    }
    Ok(value)
}

fn terminated(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value = units(value)?;
    value.push(0);
    Ok(value)
}

fn read_wide(mut read: impl FnMut(*mut u16, u32) -> u32) -> io::Result<Vec<u16>> {
    let mut buffer = vec![0; 260];
    loop {
        let length = read(buffer.as_mut_ptr(), buffer.len() as u32) as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            buffer.truncate(length);
            buffer.push(0);
            return Ok(buffer);
        }
        let capacity = length
            .checked_add(1)
            .filter(|length| *length <= u32::MAX as usize)
            .ok_or_else(|| invalid("Windows path exceeds the API buffer limit"))?;
        buffer.resize(capacity, 0);
    }
}

fn full_path(path: &[u16]) -> io::Result<Vec<u16>> {
    // The input is terminated and remains live while Windows fills the buffer.
    read_wide(|buffer, length| unsafe {
        GetFullPathNameW(path.as_ptr(), length, buffer, std::ptr::null_mut())
    })
}

fn system_directory() -> io::Result<PathBuf> {
    let path = read_wide(|buffer, length| unsafe { GetSystemDirectoryW(buffer, length) })?;
    Ok(PathBuf::from(OsString::from_wide(&path[..path.len() - 1])))
}

fn windows_directory() -> io::Result<PathBuf> {
    let path = read_wide(|buffer, length| unsafe { GetWindowsDirectoryW(buffer, length) })?;
    Ok(PathBuf::from(OsString::from_wide(&path[..path.len() - 1])))
}

/// Strip a verbatim prefix only when normal Windows parsing preserves the path.
/// cmd cannot use verbatim paths; this follows std's conservative conversion.
fn conventional_path(path: Vec<u16>, application: bool) -> io::Result<Vec<u16>> {
    if application && path.len() > 260 {
        return Ok(path);
    }
    let candidate = if path.starts_with(&r"\\?\UNC\".encode_utf16().collect::<Vec<_>>()) {
        let mut candidate = vec![SLASH, SLASH];
        candidate.extend_from_slice(&path[8..]);
        Some(candidate)
    } else if path.starts_with(&r"\\?\".encode_utf16().collect::<Vec<_>>())
        && path.get(5) == Some(&(b':' as u16))
        && path.get(6) == Some(&SLASH)
    {
        Some(path[4..].to_vec())
    } else {
        None
    };
    if let Some(candidate) = candidate
        && full_path(&candidate)? == candidate
    {
        return Ok(candidate);
    }
    Ok(path)
}

fn application_path(path: &Path) -> io::Result<Vec<u16>> {
    let encoded = conventional_path(terminated(path.as_os_str())?, true)?;
    if encoded.starts_with(&r"\\?\".encode_utf16().collect::<Vec<_>>())
        || encoded.starts_with(&r"\??\".encode_utf16().collect::<Vec<_>>())
    {
        return Ok(encoded);
    }
    let absolute = full_path(&encoded)?;
    if absolute.len() < 248 {
        return Ok(absolute);
    }
    // Match std's long-path treatment without asking the filesystem to resolve
    // symlinks, or changing the executable selection to its final target.
    let mut extended = if absolute.starts_with(&[SLASH, SLASH]) {
        let mut prefix: Vec<_> = r"\\?\UNC\".encode_utf16().collect();
        prefix.extend_from_slice(&absolute[2..]);
        prefix
    } else {
        let mut prefix: Vec<_> = r"\\?\".encode_utf16().collect();
        prefix.extend_from_slice(&absolute);
        prefix
    };
    // Device paths already have their own namespace.
    if absolute.starts_with(&[SLASH, SLASH, b'.' as u16, SLASH]) {
        extended = r"\\?\"
            .encode_utf16()
            .chain(absolute[4..].iter().copied())
            .collect();
    }
    Ok(extended)
}

fn candidate_program(path: &Path) -> Option<Vec<u16>> {
    let path = application_path(path).ok()?;
    // Like std, inspect the named entry instead of following a symlink here.
    (unsafe { GetFileAttributesW(path.as_ptr()) } != INVALID_FILE_ATTRIBUTES).then_some(path)
}

fn resolve_program(
    program: &OsStr,
    parent_path: Option<&OsStr>,
    child_path: Option<&OsStr>,
) -> io::Result<Vec<u16>> {
    let encoded = units(program)?;
    if encoded.is_empty() || matches!(encoded.last(), Some(&SLASH) | Some(&47)) {
        return Err(invalid("program path has no file name"));
    }
    let has_separator = encoded.iter().any(|unit| matches!(*unit, 47 | 92));
    if has_separator {
        if ascii_suffix(&encoded, b".exe") {
            return application_path(Path::new(program));
        }
        let mut with_exe = program.to_os_string();
        with_exe.push(".exe");
        return candidate_program(Path::new(&with_exe))
            .map(Ok)
            .unwrap_or_else(|| application_path(Path::new(program)));
    }
    let mut filename = program.to_os_string();
    if !encoded.contains(&(b'.' as u16)) {
        filename.push(".exe");
    }
    // Rust 1.88 intentionally searches an overridden child PATH first, then
    // the executable/system directories, then the original PATH. No PATHEXT.
    let mut search = Vec::new();
    if let Some(paths) = child_path {
        search.extend(std::env::split_paths(paths).filter(|path| !path.as_os_str().is_empty()));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        search.push(parent.to_path_buf());
    }
    search.extend(system_directory());
    search.extend(windows_directory());
    if let Some(paths) = parent_path {
        search.extend(std::env::split_paths(paths).filter(|path| !path.as_os_str().is_empty()));
    }
    search
        .into_iter()
        .find_map(|dir| candidate_program(&dir.join(&filename)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "program not found"))
}

fn is_batch(application: &[u16]) -> bool {
    let path = &application[..application.len() - 1];
    ascii_suffix(path, b".cmd") || ascii_suffix(path, b".bat")
}

fn ascii_suffix(value: &[u16], suffix: &[u8]) -> bool {
    value
        .get(value.len().saturating_sub(suffix.len())..)
        .is_some_and(|ending| {
            ending.len() == suffix.len()
                && ending.iter().zip(suffix).all(|(left, right)| {
                    u8::try_from(*left).is_ok_and(|left| left.eq_ignore_ascii_case(right))
                })
        })
}

fn executable_command_line(program: &[u16], args: &[Vec<u16>]) -> io::Result<Vec<u16>> {
    if program.contains(&QUOTE) {
        return Err(invalid("executable names cannot contain quotes"));
    }
    let mut output = vec![QUOTE];
    output.extend_from_slice(program);
    output.push(QUOTE);
    for arg in args {
        output.push(b' ' as u16);
        append_argument(&mut output, arg, false, false);
    }
    Ok(output)
}

// Behavior reference: Rust 1.88 std/sys/args/windows.rs. Batch arguments need
// cmd escaping in addition to the backslash/quote rules for executable argv.
// https://github.com/rust-lang/rust/blob/1.88.0/library/std/src/sys/args/windows.rs
fn append_argument(output: &mut Vec<u16>, arg: &[u16], batch: bool, force_quote: bool) {
    let quote = force_quote
        || arg.is_empty()
        || if batch {
            arg.last() == Some(&SLASH)
                || arg.iter().any(|unit| {
                    char::from_u32(u32::from(*unit)).is_some_and(|ch| {
                        ch.is_control()
                            || (ch.is_ascii()
                                && !ch.is_ascii_alphanumeric()
                                && !r"#$*+-./:?@\_".contains(ch))
                    })
                })
        } else {
            arg.iter().any(|unit| matches!(*unit, 32 | 9))
        };
    if quote {
        output.push(QUOTE);
    }
    let mut slashes = 0;
    for unit in arg {
        if *unit == SLASH {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            SLASH,
            if *unit == QUOTE { slashes * 2 } else { slashes },
        ));
        slashes = 0;
        if *unit == QUOTE {
            output.push(if batch { QUOTE } else { SLASH });
        } else if batch && *unit == b'%' as u16 {
            // The empty substring prevents cmd from expanding a following
            // environment-variable name; /e:ON enables this syntax.
            output.extend("%%cd:~,".encode_utf16());
        }
        output.push(*unit);
    }
    output.extend(std::iter::repeat_n(
        SLASH,
        if quote { slashes * 2 } else { slashes },
    ));
    if quote {
        output.push(QUOTE);
    }
}

fn batch_command_line(script: &[u16], args: &[Vec<u16>]) -> io::Result<Vec<u16>> {
    if script.contains(&QUOTE)
        || script.last() == Some(&SLASH)
        || script
            .iter()
            .chain(args.iter().flatten())
            .any(|unit| matches!(*unit, 0 | 10 | 13))
    {
        return Err(invalid("batch file path or arguments are invalid"));
    }
    let mut output: Vec<_> = "cmd.exe /e:ON /v:OFF /d /c \"".encode_utf16().collect();
    // Escape the script path too: it can contain literal percent signs.
    append_argument(&mut output, script, true, true);
    for arg in args {
        output.push(b' ' as u16);
        append_argument(&mut output, arg, true, false);
    }
    output.push(QUOTE);
    Ok(output)
}

fn compare_keys(left: &[u16], right: &[u16]) -> io::Result<Ordering> {
    let left_len = i32::try_from(left.len()).map_err(|_| invalid("environment key too long"))?;
    let right_len = i32::try_from(right.len()).map_err(|_| invalid("environment key too long"))?;
    // Compare with the OS uppercase table, preserving lone UTF-16 surrogates.
    match unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) } {
        1 => Ok(Ordering::Less),
        2 => Ok(Ordering::Equal),
        3 => Ok(Ordering::Greater),
        _ => Err(io::Error::last_os_error()),
    }
}

#[derive(Default)]
struct Environment(Vec<(Vec<u16>, Vec<u16>)>);

impl Environment {
    fn position(&self, key: &[u16]) -> io::Result<Result<usize, usize>> {
        let (mut low, mut high) = (0, self.0.len());
        while low < high {
            let middle = low + (high - low) / 2;
            match compare_keys(&self.0[middle].0, key)? {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Ok(Ok(middle)),
            }
        }
        Ok(Err(low))
    }

    fn set(&mut self, key: &OsStr, value: Option<&OsStr>) -> io::Result<()> {
        let key = units(key)?;
        // The leading '=' in inherited drive-current-directory keys is valid.
        if key.is_empty() || key[1..].contains(&(b'=' as u16)) {
            return Err(invalid("invalid Windows environment key"));
        }
        let value = value.map(units).transpose()?;
        match (self.position(&key)?, value) {
            (Ok(index), Some(value)) => self.0[index].1 = value,
            (Err(index), Some(value)) => self.0.insert(index, (key, value)),
            (Ok(index), None) => {
                self.0.remove(index);
            }
            (Err(_), None) => {}
        }
        Ok(())
    }

    fn get(&self, key: &OsStr) -> io::Result<Option<OsString>> {
        Ok(self
            .position(&units(key)?)?
            .ok()
            .map(|index| OsString::from_wide(&self.0[index].1)))
    }

    fn block(self) -> Vec<u16> {
        let mut block = Vec::new();
        for (key, value) in self.0 {
            block.extend(key);
            block.push(b'=' as u16);
            block.extend(value);
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        block
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn encoded(value: &str, batch: bool) -> String {
        let mut output = Vec::new();
        append_argument(
            &mut output,
            &value.encode_utf16().collect::<Vec<_>>(),
            batch,
            false,
        );
        String::from_utf16(&output).unwrap()
    }

    #[test]
    fn executable_arguments_preserve_quotes_empty_and_trailing_backslashes() {
        for (value, expected) in [
            ("", "\"\""),
            ("plain", "plain"),
            ("a b\\", "\"a b\\\\\""),
            ("a\\\"b", "a\\\\\\\"b"),
            ("%PATH%!&", "%PATH%!&"),
        ] {
            assert_eq!(encoded(value, false), expected, "{value:?}");
        }
    }

    #[test]
    fn batch_arguments_protect_expansion_and_shell_metacharacters() {
        for (value, expected) in [
            ("", "\"\""),
            ("a\\", "\"a\\\\\""),
            ("a\"b", "\"a\"\"b\""),
            ("%PATH%", "\"%%cd:~,%PATH%%cd:~,%\""),
            ("a!b&c|d^e(f)", "\"a!b&c|d^e(f)\""),
        ] {
            assert_eq!(encoded(value, true), expected, "{value:?}");
        }
        for value in ["line\nnext", "line\rnext", "nul\0next"] {
            assert!(
                batch_command_line(
                    &"npm.cmd".encode_utf16().collect::<Vec<_>>(),
                    &[value.encode_utf16().collect()]
                )
                .is_err()
            );
        }
        assert!(units(OsStr::new("nul\0next")).is_err());
        let script = r"C:\100%\npm.CMD".encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            String::from_utf16(&batch_command_line(&script, &[]).unwrap()).unwrap(),
            "cmd.exe /e:ON /v:OFF /d /c \"\"C:\\100%%cd:~,%\\npm.CMD\"\""
        );
        assert!(is_batch(&terminated(OsStr::new(r"C:\npm.CMD")).unwrap()));
        assert!(!is_batch(
            &terminated(OsStr::new(r"C:\npm.cmd.exe")).unwrap()
        ));
    }

    #[test]
    fn environment_deltas_are_case_insensitive_and_keep_drive_and_utf16_values() {
        let mut env = Environment::default();
        let unusual = OsString::from_wide(&[0xd800, b'X' as u16]);
        env.set(OsStr::new("Path"), Some(OsStr::new("old")))
            .unwrap();
        env.set(OsStr::new("PATH"), Some(OsStr::new("new")))
            .unwrap();
        env.set(OsStr::new("=C:"), Some(OsStr::new(r"C:\work")))
            .unwrap();
        env.set(OsStr::new("VALUE"), Some(&unusual)).unwrap();
        assert_eq!(
            env.get(OsStr::new("path")).unwrap(),
            Some(OsString::from("new"))
        );
        assert_eq!(env.get(OsStr::new("value")).unwrap(), Some(unusual));
        env.set(OsStr::new("pAtH"), None).unwrap();
        assert!(env.get(OsStr::new("Path")).unwrap().is_none());
        let block = env.block();
        assert!(block.starts_with(&"=C:=C:\\work\0".encode_utf16().collect::<Vec<_>>()));
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        assert_eq!(Environment::default().block(), vec![0, 0]);
    }

    #[test]
    fn program_selection_preserves_child_path_and_explicit_path_exe_precedence() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child path");
        let parent = root.path().join("parent path");
        std::fs::create_dir(&child).unwrap();
        std::fs::create_dir(&parent).unwrap();
        for directory in [&child, &parent] {
            std::fs::write(directory.join("stage-command-fixture.exe"), b"fixture").unwrap();
        }
        assert_eq!(
            resolve_program(
                OsStr::new("stage-command-fixture"),
                Some(parent.as_os_str()),
                Some(child.as_os_str())
            )
            .unwrap(),
            application_path(&child.join("stage-command-fixture.exe")).unwrap()
        );

        let batch = child.join("npm.cmd");
        std::fs::write(&batch, b"fixture").unwrap();
        assert_eq!(
            resolve_program(batch.as_os_str(), None, None).unwrap(),
            application_path(&batch).unwrap()
        );
        let executable = child.join("npm.cmd.exe");
        std::fs::write(&executable, b"fixture").unwrap();
        assert_eq!(
            resolve_program(batch.as_os_str(), None, None).unwrap(),
            application_path(&executable).unwrap()
        );
    }
}
