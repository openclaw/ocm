mod support;

use std::fs;
use std::io::Write;

use flate2::{Compression, write::GzEncoder};
use ocm::infra::download::{
    MAX_JSON_BYTES, artifact_file_name_from_url, download_to_file, fetch_json,
    fetch_json_with_accept, file_sha256, normalize_sha256, verify_file_sha256,
};
use serde_json::Value;

use crate::support::{TestDir, TestHttpServer};

#[test]
fn artifact_file_name_from_url_uses_the_last_path_segment() {
    let file_name =
        artifact_file_name_from_url("https://example.test/releases/openclaw-macos?sig=1#part")
            .unwrap();
    assert_eq!(file_name, "openclaw-macos");
}

#[test]
fn artifact_file_name_from_url_requires_a_file_name() {
    let error = artifact_file_name_from_url("https://example.test/releases/").unwrap_err();
    assert!(error.contains("runtime URL must include a file name"));
}

#[test]
fn download_to_file_fetches_the_http_response_body() {
    let root = TestDir::new("download-helper-success");
    let destination = root.child("downloads/openclaw");
    let server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw",
        "application/octet-stream",
        b"openclaw-binary",
    );

    download_to_file(&server.url(), &destination).unwrap();

    assert_eq!(fs::read(&destination).unwrap(), b"openclaw-binary");
}

#[test]
fn download_to_file_reports_http_errors() {
    let root = TestDir::new("download-helper-missing");
    let destination = root.child("downloads/openclaw");
    let server = TestHttpServer::serve_bytes(
        "/artifacts/openclaw",
        "application/octet-stream",
        b"openclaw-binary",
    );

    let error = download_to_file(&format!("{}/missing", server.url()), &destination).unwrap_err();

    assert!(error.contains("failed to download runtime URL"));
    assert!(error.contains("404"));
}

#[test]
fn download_to_file_can_reuse_a_repeatable_server() {
    let root = TestDir::new("download-helper-repeatable");
    let first_destination = root.child("downloads/openclaw-first");
    let second_destination = root.child("downloads/openclaw-second");
    let server = TestHttpServer::serve_bytes_times(
        "/artifacts/openclaw",
        "application/octet-stream",
        b"openclaw-binary",
        2,
    );

    download_to_file(&server.url(), &first_destination).unwrap();
    download_to_file(&server.url(), &second_destination).unwrap();

    assert_eq!(fs::read(&first_destination).unwrap(), b"openclaw-binary");
    assert_eq!(fs::read(&second_destination).unwrap(), b"openclaw-binary");
}

#[test]
fn download_to_file_preserves_content_encoded_artifact_bytes() {
    let root = TestDir::new("download-helper-encoded-artifact");
    let destination = root.child("downloads/openclaw.tar.gz");
    let encoded = gzip_bytes(b"archive-bytes");
    let server = TestHttpServer::serve_bytes_with_headers(
        "/artifacts/openclaw.tar.gz",
        "application/gzip",
        &encoded,
        &[("Content-Encoding", "gzip")],
    );

    download_to_file(&server.url(), &destination).unwrap();

    assert_eq!(fs::read(&destination).unwrap(), encoded);
    assert!(
        !server.requests()[0]
            .to_ascii_lowercase()
            .contains("accept-encoding: gzip")
    );
}

#[test]
fn fetch_json_can_consume_sequential_http_responses() {
    let server = TestHttpServer::serve_bytes_sequence(
        "/manifests/releases.json",
        "application/json",
        vec![
            br#"{"releases":[{"version":"0.2.0","url":"https://example.test/openclaw-0.2.0"}]}"#
                .to_vec(),
            br#"{"releases":[{"version":"0.3.0","url":"https://example.test/openclaw-0.3.0"}]}"#
                .to_vec(),
        ],
    );

    let first: Value = fetch_json(&server.url()).unwrap();
    let second: Value = fetch_json(&server.url()).unwrap();

    assert_eq!(first["releases"][0]["version"], "0.2.0");
    assert_eq!(second["releases"][0]["version"], "0.3.0");
}

#[test]
fn fetch_json_can_request_a_specific_representation() {
    let server = TestHttpServer::serve_bytes(
        "/manifests/releases.json",
        "application/json",
        br#"{"releases":[]}"#,
    );

    let _: Value =
        fetch_json_with_accept(&server.url(), "application/vnd.npm.install-v1+json").unwrap();

    assert!(
        server.requests()[0]
            .to_ascii_lowercase()
            .contains("accept: application/vnd.npm.install-v1+json")
    );
}

#[test]
fn fetch_json_requests_and_decodes_gzip_responses() {
    let encoded = gzip_bytes(br#"{"releases":[{"version":"0.3.0"}]}"#);
    let server = TestHttpServer::serve_bytes_with_headers(
        "/manifests/releases.json",
        "application/json",
        &encoded,
        &[("Content-Encoding", "gzip")],
    );

    let manifest: Value = fetch_json(&server.url()).unwrap();

    assert_eq!(manifest["releases"][0]["version"], "0.3.0");
    assert!(
        server.requests()[0]
            .to_ascii_lowercase()
            .contains("accept-encoding: gzip")
    );
}

#[test]
fn fetch_json_rejects_a_gzip_decoded_body_past_the_byte_cap() {
    // One byte past the decoded JSON cap. Compressed on the wire this is a few KB.
    let mut json = Vec::from(&b"{\"pad\":\""[..]);
    json.extend(std::iter::repeat(b'x').take(MAX_JSON_BYTES as usize + 1));
    json.extend_from_slice(b"\"}");
    let encoded = gzip_bytes(&json);
    let server = TestHttpServer::serve_bytes_with_headers(
        "/manifests/releases.json",
        "application/json",
        &encoded,
        &[("Content-Encoding", "gzip")],
    );

    let error = match fetch_json::<Value>(&server.url()) {
        Err(error) => error,
        Ok(_) => panic!("gzip-decoded JSON past the byte cap must be rejected"),
    };
    assert!(error.contains("exceeded"), "unexpected error: {error}");
}

#[test]
fn file_sha256_and_verify_file_sha256_report_the_expected_digest() {
    let root = TestDir::new("download-helper-sha256");
    let artifact = root.child("downloads/openclaw");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"openclaw-binary").unwrap();

    let digest = file_sha256(&artifact).unwrap();
    assert_eq!(
        digest,
        "27267fa2dc8f0e5b3e0d4d606bb5c2608fccfba413d87fa350598d5cb16545d1"
    );
    assert_eq!(
        verify_file_sha256(
            &artifact,
            "27267FA2DC8F0E5B3E0D4D606BB5C2608FCCFBA413D87FA350598D5CB16545D1"
        )
        .unwrap(),
        digest
    );
}

#[test]
fn normalize_sha256_rejects_invalid_hashes() {
    let error = normalize_sha256("1234").unwrap_err();
    assert!(error.contains("sha256 is invalid"));
}

#[test]
fn verify_file_sha256_reports_mismatches() {
    let root = TestDir::new("download-helper-sha256-mismatch");
    let artifact = root.child("downloads/openclaw");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"openclaw-binary").unwrap();

    let error = verify_file_sha256(
        &artifact,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap_err();
    assert!(error.contains("sha256 mismatch"));
    assert!(error.contains("expected aaaaaaaa"));
}

fn gzip_bytes(body: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}
