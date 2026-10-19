use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::support::write_text;

pub struct IncludedPluginConfig {
    files: Vec<(PathBuf, String)>,
    external: PathBuf,
}

impl IncludedPluginConfig {
    pub fn seed(state: &Path, external: &Path) -> Self {
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
                json!({"paths": [state.join("extensions/demo"), external]}).to_string(),
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
        Self {
            files,
            external: external.to_path_buf(),
        }
    }

    pub fn assert_source_unchanged(&self, state: &Path) {
        for (path, contents) in &self.files {
            assert_eq!(fs::read_to_string(state.join(path)).unwrap(), *contents);
        }
    }

    pub fn assert_relocated(&self, state: &Path) {
        let read = |path: &str| -> Value {
            json5::from_str(&fs::read_to_string(state.join(path)).unwrap()).unwrap()
        };
        assert_eq!(
            read("config/load.json5")["paths"],
            json!([state.join("extensions/demo"), self.external])
        );
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
}
