use std::collections::BTreeMap;
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;

use ocm::cli::Cli;

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "ocm: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32, String> {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env_map = env::vars_os()
        .map(|(key, value)| {
            let key = key
                .into_string()
                .map_err(|_| "environment contains a non-UTF-8 variable name".to_string())?;
            let value = value
                .into_string()
                .map_err(|_| format!("environment variable {key} contains a non-UTF-8 value"))?;
            Ok((key, value))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let args = env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "command arguments must be valid UTF-8".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    if ocm::runtime::is_internal_npm_proxy(&env_map) {
        return ocm::runtime::run_internal_npm_proxy(&args, &env_map, &cwd);
    }

    Ok(Cli { env: env_map, cwd }.run(args))
}
