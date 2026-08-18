use std::fs::{self, File};
use std::io;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use flate2::read::GzDecoder;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256, Sha512};

const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// ureq 3 Agent defaults leave timeout_connect and timeout_global unset.
static HTTP_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_connect(Some(HTTP_TIMEOUT))
            .timeout_global(Some(HTTP_TIMEOUT))
            .build(),
    )
});

pub fn artifact_file_name_from_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("runtime URL is required".to_string());
    }

    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let segment = without_query
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("runtime URL must include a file name: {trimmed}"))?;

    if segment == "."
        || segment == ".."
        || segment.contains('\\')
        || segment.contains(':')
        || segment.contains('\0')
        || Path::new(segment).components().count() != 1
    {
        return Err(format!("runtime URL must include a file name: {trimmed}"));
    }

    Ok(segment.to_string())
}

pub fn download_to_file(url: &str, destination: &Path) -> Result<(), String> {
    let mut reader = open_url_reader(url, None, false)?;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let mut file = File::create(destination).map_err(|error| error.to_string())?;
    copy_capped(&mut reader, &mut file, MAX_DOWNLOAD_BYTES).map_err(|error| error.to_string())?;
    Ok(())
}

fn copy_capped<R: io::Read + ?Sized, W: io::Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
) -> io::Result<u64> {
    let copied = {
        let mut limited = reader.take(max_bytes);
        io::copy(&mut limited, writer)?
    };
    if copied == max_bytes {
        let mut extra = [0_u8; 1];
        if reader.read(&mut extra)? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("download exceeded {max_bytes} bytes"),
            ));
        }
    }
    Ok(copied)
}

pub fn fetch_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let reader = open_url_reader(url, None, true)?;
    parse_json_reader(reader, url)
}

pub fn fetch_json_with_accept<T: DeserializeOwned>(url: &str, accept: &str) -> Result<T, String> {
    let reader = open_url_reader(url, Some(accept), true)?;
    parse_json_reader(reader, url)
}

fn parse_json_reader<T: DeserializeOwned>(
    reader: Box<dyn io::Read>,
    url: &str,
) -> Result<T, String> {
    serde_json::from_reader(reader)
        .map_err(|error| format!("failed to parse runtime URL \"{}\": {error}", url.trim()))
}

fn open_url_reader(
    url: &str,
    accept: Option<&str>,
    compressed_json: bool,
) -> Result<Box<dyn io::Read>, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("runtime URL is required".to_string());
    }

    let mut request = HTTP_AGENT.get(trimmed);
    if compressed_json {
        request = request.header("Accept-Encoding", "gzip");
    }
    let response = match accept {
        Some(accept) => request.header("Accept", accept).call(),
        None => request.call(),
    }
    .map_err(|error| format!("failed to download runtime URL \"{trimmed}\": {error}"))?;
    let gzip_encoded = response
        .headers()
        .get("Content-Encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("gzip"));
    let reader = response.into_body().into_reader();
    if compressed_json && gzip_encoded {
        Ok(Box::new(GzDecoder::new(reader)))
    } else {
        Ok(Box::new(reader))
    }
}

pub fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn normalize_sha256(expected: &str) -> Result<String, String> {
    let value = expected.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err("runtime artifact sha256 is required".to_string());
    }
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("runtime artifact sha256 is invalid: {expected}"));
    }
    Ok(value)
}

pub fn verify_file_sha256(path: &Path, expected: &str) -> Result<String, String> {
    let expected = normalize_sha256(expected)?;
    let actual = file_sha256(path)?;
    if actual != expected {
        return Err(format!(
            "runtime artifact sha256 mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(actual)
}

pub fn verify_file_integrity(path: &Path, expected: &str) -> Result<(), String> {
    let expected = normalize_file_integrity(expected)?;
    let Some((algorithm, encoded)) = expected.split_once('-') else {
        unreachable!("normalized integrity includes an algorithm");
    };

    match algorithm {
        "sha512" => verify_file_sha512_base64(path, encoded, &expected),
        _ => unreachable!("normalized integrity uses a supported algorithm"),
    }
}

pub fn normalize_file_integrity(expected: &str) -> Result<String, String> {
    let expected = expected.trim();
    if expected.is_empty() {
        return Err("runtime artifact integrity is required".to_string());
    }

    let Some((algorithm, encoded)) = expected.split_once('-') else {
        return Err(format!("runtime artifact integrity is invalid: {expected}"));
    };
    if encoded.trim().is_empty() {
        return Err(format!("runtime artifact integrity is invalid: {expected}"));
    }

    match algorithm {
        "sha512" => {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .map_err(|_| format!("runtime artifact integrity is invalid: {expected}"))?;
            if decoded.len() != 64 {
                return Err(format!("runtime artifact integrity is invalid: {expected}"));
            }
            Ok(format!("sha512-{}", encoded.trim()))
        }
        _ => Err(format!(
            "runtime artifact integrity algorithm is unsupported: {algorithm}"
        )),
    }
}

fn verify_file_sha512_base64(
    path: &Path,
    expected_base64: &str,
    raw_expected: &str,
) -> Result<(), String> {
    let expected = base64::engine::general_purpose::STANDARD
        .decode(expected_base64)
        .map_err(|_| format!("runtime artifact integrity is invalid: {raw_expected}"))?;
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha512::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    let actual = hasher.finalize();
    if actual.as_slice() != expected.as_slice() {
        return Err("runtime artifact integrity mismatch".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{artifact_file_name_from_url, copy_capped};
    use std::io::{self, Read};

    #[test]
    fn artifact_file_name_rejects_cross_platform_path_components() {
        for url in [
            "https://example.test/releases/..",
            "https://example.test/releases/C:\\temp\\openclaw.exe",
            "https://example.test/releases/..\\..\\openclaw.exe",
            "https://example.test/releases/share:openclaw",
        ] {
            assert!(artifact_file_name_from_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn artifact_file_name_accepts_one_portable_component() {
        assert_eq!(
            artifact_file_name_from_url(
                "https://example.test/releases/openclaw.tar.gz?download=1#asset"
            )
            .unwrap(),
            "openclaw.tar.gz"
        );
    }

    #[test]
    fn copy_capped_errors_when_reader_exceeds_max() {
        let mut reader = io::repeat(b'a').take(16);
        let mut writer = Vec::new();
        let error = copy_capped(&mut reader, &mut writer, 8).unwrap_err();
        assert!(
            error.to_string().contains("exceeded"),
            "unexpected error: {error}"
        );
        assert!(
            writer.len() <= 8,
            "fail closed must not keep more than the cap: {}",
            writer.len()
        );
    }

    #[test]
    fn copy_capped_copies_when_reader_is_within_max() {
        let mut reader = &b"hello"[..];
        let mut writer = Vec::new();
        let copied = copy_capped(&mut reader, &mut writer, 8).unwrap();
        assert_eq!(copied, 5);
        assert_eq!(writer, b"hello");
    }

    #[test]
    fn copy_capped_allows_a_reader_that_hits_the_max_exactly() {
        let mut reader = &b"hello"[..];
        let mut writer = Vec::new();
        let copied = copy_capped(&mut reader, &mut writer, 5).unwrap();
        assert_eq!(copied, 5);
        assert_eq!(writer, b"hello");
    }
}
