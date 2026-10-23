use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

// Match the native producer's ordered manifest fields and revision encoding.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    path: String,
    sha256: String,
    size_bytes: usize,
    executable: bool,
}

pub struct SkillLibrary {
    revisions: Vec<(String, String, Vec<(String, Vec<u8>, bool)>)>,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

const SKILL_ID: &str = "00000000-0000-0000-0000-000000000001";

impl SkillLibrary {
    pub fn seed(state: &Path) -> Self {
        fs::create_dir_all(state.join("state")).unwrap();
        let payload = tempfile::NamedTempFile::new_in(state).unwrap();
        let payload_db = Connection::open(payload.path()).unwrap();
        payload_db.execute_batch("CREATE TABLE example(value TEXT); INSERT INTO example VALUES ('authored payload');").unwrap();
        drop(payload_db);
        let sqlite_bytes = fs::read(payload.path()).unwrap();
        drop(payload);
        let db = Connection::open(state.join("state/openclaw.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE skill_library_entries (
                skill_id TEXT PRIMARY KEY, owner_profile_id TEXT,
                author_profile_id TEXT NOT NULL, slug TEXT NOT NULL,
                current_revision TEXT NOT NULL, shared INT NOT NULL,
                enabled INT NOT NULL, removed INT NOT NULL,
                created_at INT NOT NULL, updated_at INT NOT NULL) STRICT;
             CREATE TABLE skill_library_revisions (
                skill_id TEXT NOT NULL, revision TEXT NOT NULL, description TEXT NOT NULL,
                files_json TEXT NOT NULL, created_at INT NOT NULL,
                PRIMARY KEY (skill_id, revision)) STRICT;",
        )
        .unwrap();
        let mut revisions = Vec::new();
        for (index, instruction) in [
            "Use the ordinary saved procedure.".to_string(),
            format!("Read {}/workspace/notes.txt", state.display()),
        ]
        .into_iter()
        .enumerate()
        {
            let config = if index == 0 {
                serde_json::json!({"paths": []})
            } else {
                serde_json::json!({"paths": [state.join("workspace")]})
            };
            let files = vec![
                (
                    "SKILL.md".to_string(),
                    format!(
                        "---\nname: fixture\ndescription: Saved procedure\n---\n{instruction}\n"
                    )
                    .into_bytes(),
                    false,
                ),
                (
                    "scripts/example.sh".to_string(),
                    format!("#!/bin/sh\n# {instruction}\nexit 0\n").into_bytes(),
                    true,
                ),
                (
                    "support/config.json5".to_string(),
                    config.to_string().into_bytes(),
                    false,
                ),
                (
                    "support/example.sqlite".to_string(),
                    sqlite_bytes.clone(),
                    false,
                ),
                (
                    "support/example.sqlite-journal".to_string(),
                    b"authored supporting bytes".to_vec(),
                    false,
                ),
                // A name that would be volatile in runtime state is authored here.
                (
                    "support/package.lock".to_string(),
                    instruction.into_bytes(),
                    false,
                ),
            ];
            let manifest = files
                .iter()
                .map(|(path, bytes, executable)| ManifestFile {
                    path: path.clone(),
                    sha256: hash(bytes),
                    size_bytes: bytes.len(),
                    executable: *executable,
                })
                .collect::<Vec<_>>();
            let raw = serde_json::to_string(&manifest).unwrap();
            let revision = hash(format!("[\"openclaw.skill-library.tree.v1\",{raw}]").as_bytes());
            let directory = state
                .join("skill-library")
                .join(SKILL_ID)
                .join("revisions")
                .join(&revision);
            for (name, bytes, executable) in &files {
                let path = directory.join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, bytes).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(if *executable { 0o500 } else { 0o400 }),
                    )
                    .unwrap();
                }
                #[cfg(not(unix))]
                let _ = executable;
            }
            db.execute(
                "INSERT INTO skill_library_revisions VALUES (?1, ?2, 'Saved procedure', ?3, 1)",
                params![SKILL_ID, revision, raw],
            )
            .unwrap();
            revisions.push((revision, raw, files));
        }
        db.execute("INSERT INTO skill_library_entries VALUES (?1, 'fixture-profile', 'fixture-profile', 'fixture', ?2, 0, 1, 0, 1, 1)", params![SKILL_ID, revisions.last().unwrap().0]).unwrap();
        Self { revisions }
    }

    pub fn config_include(&self, state: &Path, needs_rewrite: bool) -> PathBuf {
        state
            .join("skill-library")
            .join(SKILL_ID)
            .join("revisions")
            .join(&self.revisions[usize::from(needs_rewrite)].0)
            .join("support/config.json5")
    }

    pub fn configure_include(&self, state: &Path, include: &Path) {
        fs::create_dir_all(state.join("workspace")).unwrap();
        fs::write(
            state.join("openclaw.json"),
            serde_json::json!({
                "agents": {"defaults": {"workspace": state.join("workspace")}},
                "plugins": {"entries": {"fixture": {"config": {"$include": include}}}}
            })
            .to_string(),
        )
        .unwrap();
    }

    pub fn assert_preserved(&self, state: &Path) {
        let db = Connection::open(state.join("state/openclaw.sqlite")).unwrap();
        let current: String = db
            .query_row(
                "SELECT current_revision FROM skill_library_entries WHERE skill_id = ?1",
                [SKILL_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current, self.revisions.last().unwrap().0);
        let count: i64 = db
            .query_row("SELECT count(*) FROM skill_library_revisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, self.revisions.len() as i64);
        for (revision, manifest, files) in &self.revisions {
            let actual: String = db.query_row("SELECT files_json FROM skill_library_revisions WHERE skill_id = ?1 AND revision = ?2", params![SKILL_ID, revision], |row| row.get(0)).unwrap();
            assert_eq!(&actual, manifest);
            assert_eq!(
                &hash(format!("[\"openclaw.skill-library.tree.v1\",{actual}]").as_bytes()),
                revision
            );
            for (name, bytes, executable) in files {
                let path = state
                    .join("skill-library")
                    .join(SKILL_ID)
                    .join("revisions")
                    .join(revision)
                    .join(name);
                let actual =
                    fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                assert_eq!(&actual, bytes, "{}", path.display());
                assert_eq!(hash(&actual), hash(bytes));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        if *executable { 0o500 } else { 0o400 },
                        "{}",
                        path.display()
                    );
                }
                #[cfg(not(unix))]
                let _ = executable;
            }
        }
    }
}
