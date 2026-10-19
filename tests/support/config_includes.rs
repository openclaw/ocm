use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::support::write_text;

pub struct IncludedPluginConfig {
    files: Vec<(PathBuf, String)>,
    external: PathBuf,
    external_traversal: PathBuf,
}

impl IncludedPluginConfig {
    pub fn seed(state: &Path, external: &Path) -> Self {
        let external_traversal = state.join("../../external/demo");
        let files = vec![
            (
                PathBuf::from("openclaw.json"),
                json!({
                    "plugins": {
                        "$include": ["./plugins.json5", "./unchanged.json5"],
                        "entries": {"demo": {"config": {"$include": "./config/load.json5"}}}
                    }
                })
                .to_string(),
            ),
            (
                PathBuf::from("plugins.json5"),
                "{ load: { $include: './config/load.json5' } }\n".to_string(),
            ),
            (
                PathBuf::from("config/load.json5"),
                json!({"paths": [state.join("extensions/demo"), external, external_traversal]})
                    .to_string(),
            ),
            (
                PathBuf::from("unchanged.json5"),
                "// Keep unchanged include bytes.\n{ enabled: true }\n".to_string(),
            ),
            (
                PathBuf::from("extensions/demo/index.js"),
                "export default { id: 'demo', register() {} };\n".to_string(),
            ),
            (
                PathBuf::from("extensions/demo/openclaw.plugin.json"),
                json!({"id": "demo", "configSchema": {"type": "object", "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}}
                }}})
                .to_string(),
            ),
        ];
        for (path, contents) in &files {
            write_text(&state.join(path), contents);
        }
        fs::create_dir_all(&external_traversal).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                state.join("config/load.json5"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap();
        }
        Self {
            files,
            external: external.to_path_buf(),
            external_traversal,
        }
    }

    pub fn assert_source_unchanged(&self, state: &Path) {
        for (path, contents) in &self.files {
            assert_eq!(fs::read_to_string(state.join(path)).unwrap(), *contents);
        }
        self.assert_include_mode(state);
    }

    pub fn assert_relocated(&self, state: &Path) {
        let read = |path: &str| -> Value {
            json5::from_str(&fs::read_to_string(state.join(path)).unwrap()).unwrap()
        };
        let included = read("config/load.json5");
        let paths = included["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 3);
        assert_eq!(
            Path::new(paths[0].as_str().unwrap()),
            state.join("extensions/demo")
        );
        assert_eq!(paths[1], json!(self.external));
        assert_eq!(paths[2], json!(self.external_traversal));
        self.assert_include_mode(state);
        assert_eq!(
            read("openclaw.json")["plugins"]["$include"],
            json!(["./plugins.json5", "./unchanged.json5"])
        );
        assert_eq!(
            read("openclaw.json")["plugins"]["entries"]["demo"]["config"]["$include"],
            "./config/load.json5"
        );
        // Unchanged fragments retain comments and authored relative directives.
        for path in [
            "plugins.json5",
            "unchanged.json5",
            "extensions/demo/index.js",
        ] {
            let original = self
                .files
                .iter()
                .find(|(p, _)| p == Path::new(path))
                .unwrap();
            assert_eq!(fs::read_to_string(state.join(path)).unwrap(), original.1);
        }
    }

    fn assert_include_mode(&self, state: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(state.join("config/load.json5"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o640
            );
        }
        #[cfg(not(unix))]
        let _ = state;
    }
}
