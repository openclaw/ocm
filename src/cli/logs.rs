use std::io::{self, Write};

use super::render::RenderProfile;
use super::{Cli, render};

// Keep stdout's error kind across LogService's string-error boundary, while
// still returning the error so a follower stops reading immediately.
struct LogStdout<'a, W> {
    inner: &'a mut W,
    header: Option<String>,
    broken_pipe: bool,
}

impl<W: Write> LogStdout<'_, W> {
    fn write_header(&mut self) -> io::Result<()> {
        if let Some(header) = self.header.take() {
            self.inner
                .write_all(header.as_bytes())
                .inspect_err(|error| {
                    self.broken_pipe |= error.kind() == io::ErrorKind::BrokenPipe;
                })?;
        }
        Ok(())
    }
}

impl<W: Write> Write for LogStdout<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_header()?;
        self.inner.write(buf).inspect_err(|error| {
            self.broken_pipe |= error.kind() == io::ErrorKind::BrokenPipe;
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write_header()?;
        self.inner.flush().inspect_err(|error| {
            self.broken_pipe |= error.kind() == io::ErrorKind::BrokenPipe;
        })
    }
}

struct PrettyLogWriter<'a, W: Write> {
    inner: &'a mut W,
    profile: RenderProfile,
    buffer_partial: bool,
    pending: Vec<u8>,
}

impl<'a, W: Write> PrettyLogWriter<'a, W> {
    fn new(inner: &'a mut W, profile: RenderProfile, buffer_partial: bool) -> Self {
        Self {
            inner,
            profile,
            buffer_partial,
            pending: Vec::new(),
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = String::from_utf8_lossy(&self.pending);
        let rendered = render::logs::render_log_text(&pending, self.profile);
        self.inner
            .write_all(rendered.as_bytes())
            .map_err(|error| error.to_string())?;
        self.pending.clear();
        self.inner.flush().map_err(|error| error.to_string())
    }
}

impl<W: Write> Write for PrettyLogWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.buffer_partial {
            self.inner.write_all(buf)?;
            return Ok(buf.len());
        }
        self.pending.extend_from_slice(buf);
        while let Some(newline_index) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&self.pending[..=newline_index]);
            let rendered = render::logs::render_log_text(&line, self.profile);
            self.inner.write_all(rendered.as_bytes())?;
            self.pending.drain(..=newline_index);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Cli {
    pub(super) fn handle_logs_command(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "logs")?;
        let (args, follow_long) = Self::consume_flag(args, "--follow");
        let (args, follow_short) = Self::consume_flag(args, "-f");
        let follow = follow_long || follow_short;
        let (args, stream_raw) = Self::consume_option(args, "--stream")?;
        let (args, tail_raw) = Self::consume_option(args, "--tail")?;
        let tail_lines = match tail_raw.as_deref() {
            Some(raw) => Some(Self::parse_positive_u32(raw, "--tail")? as usize),
            None => Some(50),
        };

        if json_flag && follow {
            return Err("logs cannot combine --json with --follow".to_string());
        }

        let Some(name) = args.first() else {
            return Err("logs requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let stream = match stream_raw.as_deref() {
            None => "all",
            Some("info") => "stdout",
            Some("error") => "stderr",
            Some(other) => {
                return Err(format!(
                    "unsupported log stream level: {other}; use --stream info or --stream error"
                ));
            }
        };
        if follow {
            let header = if profile.pretty {
                Some(
                    render::logs::log_header(
                        name,
                        &self.log_service().follow_components(name, stream)?,
                        tail_lines,
                        true,
                        profile,
                    )
                    .join("\n")
                        + "\n",
                )
            } else {
                None
            };
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            let mut output = LogStdout {
                inner: &mut handle,
                // The follower validates initial reads before writing/flushing
                // this header, including when the source is empty or pending.
                header,
                broken_pipe: false,
            };
            let result = if profile.pretty {
                // Merged streams need complete records for ordering and rendering.
                // A single stream must forward partial bytes immediately.
                let mut writer = PrettyLogWriter::new(&mut output, profile, stream == "all");
                self.log_service()
                    .follow(name, stream, tail_lines, &mut writer)
                    .and_then(|_| writer.finish())
            } else {
                self.log_service()
                    .follow(name, stream, tail_lines, &mut output)
                    .map(|_| ())
            };
            if output.broken_pipe {
                Self::record_stdout_error(io::Error::from(io::ErrorKind::BrokenPipe));
            } else {
                result?;
            }
            return Ok(0);
        }

        let summary = self.log_service().read(name, stream, tail_lines)?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }
        if profile.pretty {
            self.stdout_lines(render::logs::log_header(
                &summary.env_name,
                &summary.components,
                summary.tail_lines,
                false,
                profile,
            ));
        }

        let content = if profile.pretty {
            render::logs::render_log_text(&summary.content, profile)
        } else {
            summary.content
        };
        self.stdout_text(&content)?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{LogStdout, PrettyLogWriter};
    use crate::cli::render::RenderProfile;
    use std::io::Write;

    #[test]
    fn log_stdout_preserves_write_and_flush_error_kinds() {
        struct FailingWriter {
            kind: std::io::ErrorKind,
            fail_write: bool,
        }
        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.fail_write {
                    Err(self.kind.into())
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(self.kind.into())
            }
        }
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::PermissionDenied,
        ] {
            for header in [None, Some("header\n".to_string())] {
                for fail_write in [false, true] {
                    let mut sink = FailingWriter { kind, fail_write };
                    let mut output = LogStdout {
                        inner: &mut sink,
                        header: header.clone(),
                        broken_pipe: false,
                    };
                    let error = output
                        .write_all(b"record\n")
                        .and_then(|_| output.flush())
                        .unwrap_err();
                    assert_eq!(error.kind(), kind);
                    assert_eq!(output.broken_pipe, kind == std::io::ErrorKind::BrokenPipe);
                }
            }
        }
    }

    #[test]
    fn pretty_writer_preserves_utf8_split_across_writes() {
        let mut output = Vec::new();
        let mut writer = PrettyLogWriter::new(&mut output, RenderProfile::raw(), true);
        writer.write_all(&[b'a', 0xe2]).unwrap();
        writer.write_all(&[0x82, 0xac, b'\n']).unwrap();
        writer.finish().unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "a€\n");
    }

    #[test]
    fn pretty_writer_replaces_invalid_utf8_without_failing() {
        let mut output = Vec::new();
        let mut writer = PrettyLogWriter::new(&mut output, RenderProfile::raw(), true);
        writer.write_all(b"valid\ninvalid \xff\n").unwrap();
        writer.finish().unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "valid\ninvalid \u{fffd}\n"
        );
    }

    #[test]
    fn single_stream_writer_forwards_unterminated_bytes_immediately() {
        let mut output = Vec::new();
        let mut writer = PrettyLogWriter::new(&mut output, RenderProfile::pretty(false), false);
        writer.write_all(b"progress \xff").unwrap();
        writer.flush().unwrap();
        assert_eq!(output, b"progress \xff");
    }
}
