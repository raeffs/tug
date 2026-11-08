//! The environment model: environment name, compose file layering,
//! dotenv loading, and persistence files.
//!
//! Everything here takes the process environment as *data* rather than reading
//! globals, so resolution stays deterministic and testable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Resolve the environment name for this invocation.
///
/// `CI` set in the process environment (to any value) resolves to `ci`.
/// Otherwise `ENVIRONMENT_NAME` is read from, highest precedence first, the
/// process environment, `<repoRoot>/.env.local`, `<repoRoot>/.env`; default
/// `local`. Project-level dotenv files never take part.
///
/// # Errors
///
/// Fails when a repo-root dotenv file exists but cannot be read.
pub fn environment_name(
    process_env: &BTreeMap<String, String>,
    repo_root: &Path,
) -> Result<String> {
    if process_env.contains_key("CI") {
        return Ok("ci".to_owned());
    }
    let layers = [repo_root.join(".env.local"), repo_root.join(".env")];
    let env = load_layers(process_env, &layers)?;
    Ok(env
        .get("ENVIRONMENT_NAME")
        .cloned()
        .unwrap_or_else(|| "local".to_owned()))
}

/// The `--file` arguments for a project's compose invocations:
/// the base `docker-compose.yml`, plus the `docker-compose.<envName>.yml`
/// overlay iff it exists. Paths are relative to the repo root, as compose
/// runs with cwd = repo root.
#[must_use]
pub fn compose_files(repo_root: &Path, project_dir: &Path, env_name: &str) -> Vec<PathBuf> {
    let mut files = vec![project_dir.join("docker-compose.yml")];
    let overlay = project_dir.join(format!("docker-compose.{env_name}.yml"));
    if repo_root.join(&overlay).is_file() {
        files.push(overlay);
    }
    files
}

/// The environment of every spawned `docker` process: the merge
/// of, highest precedence first, the CLI's own process environment, the
/// project's `.env.local` and `.env`, then the repo root's. A variable set by
/// a higher layer is never overridden.
///
/// # Errors
///
/// Fails when a dotenv file exists but cannot be read.
pub fn merged_env(
    process_env: &BTreeMap<String, String>,
    repo_root: &Path,
    project_dir: &Path,
) -> Result<BTreeMap<String, String>> {
    let project = repo_root.join(project_dir);
    let layers = [
        project.join(".env.local"),
        project.join(".env"),
        repo_root.join(".env.local"),
        repo_root.join(".env"),
    ];
    load_layers(process_env, &layers)
}

/// `base` with every dotenv file in `layers` merged under it, in order: a
/// variable set by an earlier source is never overridden. Missing files are
/// skipped.
fn load_layers(
    base: &BTreeMap<String, String>,
    layers: &[PathBuf],
) -> Result<BTreeMap<String, String>> {
    let mut merged = base.clone();
    for path in layers {
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        for (name, value) in parse_dotenv(&text) {
            merged.entry(name).or_insert(value);
        }
    }
    Ok(merged)
}

/// Append `NAME='value'` to the root `.env.local`. Append-only:
/// existing lines are never rewritten or deleted.
///
/// # Errors
///
/// Fails when `value` contains a single quote (it would be written
/// unescaped) or the file cannot be opened or written.
pub fn append_env_local(repo_root: &Path, name: &str, value: &str) -> Result<()> {
    use std::io::Write;
    if value.contains('\'') {
        anyhow::bail!("value of {name} contains a single quote and cannot be persisted");
    }
    let path = repo_root.join(".env.local");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    writeln!(file, "{name}='{value}'")
        .with_context(|| format!("failed to append to {}", path.display()))
}

/// Write a secret to `<repoRoot>/.secrets/<NAME>`: the raw
/// value, no trailing newline, file mode `0600`, directory mode `0700`.
///
/// # Errors
///
/// Fails when the directory or file cannot be created or written.
pub fn write_secret(repo_root: &Path, name: &str, value: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = repo_root.join(".secrets");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", dir.display()))?;
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(value.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Parse dotenv text: `NAME=value` lines, single-quote-wrapped
/// values stripped, `#` comment lines and blank lines ignored.
fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let mut variables = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap_or(value);
        variables.push((name.to_owned(), value.to_owned()));
    }
    variables
}
