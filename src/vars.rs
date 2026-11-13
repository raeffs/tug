//! The `create-variables` command: cascade collection, the five
//! variable types, prompting, `--use-defaults`, and persistence.
//!
//! Prompting flows through the [`Console`] line-IO boundary so tests can
//! script answers; host lookups (docker GID, hostname) arrive as
//! [`HostFacts`] data for the same reason.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::env;
use crate::repo::{Repo, VariableKind, VariableSpec};

/// The line-IO boundary: plain prompts and warnings, no TUI.
pub trait Console {
    /// Print `prompt` and read one answer line (without the newline).
    ///
    /// # Errors
    ///
    /// Fails when the terminal cannot be read or written.
    fn ask(&mut self, prompt: &str) -> Result<String>;

    /// Print a warning line.
    fn warn(&mut self, message: &str);
}

/// Host facts backing the `docker-group` and `hostname` types, resolved
/// natively — no shelling out — and injected as data.
#[derive(Debug, Clone)]
pub struct HostFacts {
    /// The GID of the local `docker` group, if there is one.
    pub docker_gid: Option<String>,
    /// The machine hostname.
    pub hostname: Option<String>,
}

impl HostFacts {
    /// Detect the facts natively: the `docker` group from `/etc/group` and
    /// the hostname from the OS API — no shelling out.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            docker_gid: std::fs::read_to_string("/etc/group")
                .ok()
                .as_deref()
                .and_then(docker_gid_from_group),
            hostname: hostname(),
        }
    }
}

/// The GID of the `docker` group in `/etc/group`-format text: lines of
/// `name:password:gid:members`.
#[must_use]
pub fn docker_gid_from_group(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next() != Some("docker") {
            return None;
        }
        // The password field sits between the name and the GID.
        fields.nth(1).map(ToOwned::to_owned)
    })
}

// The machine hostname comes from the OS API, not the `hostname` binary.
unsafe extern "C" {
    fn gethostname(name: *mut u8, len: usize) -> i32;
}

/// The machine hostname, if the OS reports one.
fn hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    let result = unsafe { gethostname(buffer.as_mut_ptr(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let len = buffer.iter().position(|byte| *byte == 0)?;
    Some(String::from_utf8_lossy(&buffer[..len]).into_owned())
}

/// The real console: prompts on stdout, answers from stdin, warnings on
/// stderr.
#[derive(Debug, Default)]
pub struct TerminalConsole {}

impl TerminalConsole {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Console for TerminalConsole {
    fn ask(&mut self, prompt: &str) -> Result<String> {
        use std::io::{BufRead, Write};
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(prompt.as_bytes())?;
        stdout.flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        Ok(answer.trim_end_matches(['\r', '\n']).to_owned())
    }

    fn warn(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

/// `create-variables <project>`: collect the cascade's variable
/// specs, obtain a value for each one not already set, and persist it.
///
/// # Errors
///
/// Fails on an unknown project name, unreadable dotenv files, or when the
/// console or a persistence file cannot be used.
pub fn create_variables(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    use_defaults: bool,
    host: &HostFacts,
    console: &mut dyn Console,
) -> Result<()> {
    let project = repo
        .project(name)
        .ok_or_else(|| anyhow::anyhow!("unknown project {name:?}"))?;
    let specs = repo.cascade(name, |project| project.variables.as_slice())?;
    let mut env = env::merged_env(process_env, repo.root(), &project.dir)?;
    for spec in specs {
        // Skip if already set: secrets by their file, everything
        // else by the merged environment.
        if spec.is_secret {
            if repo.root().join(".secrets").join(&spec.name).is_file() {
                continue;
            }
        } else if env.contains_key(&spec.name) {
            continue;
        }
        let value = if use_defaults {
            default_value(spec, &env, host, console)?
        } else {
            prompted_value(spec, &env, host, console)?
        };
        if spec.is_secret {
            env::write_secret(repo.root(), &spec.name, &value)?;
        } else {
            env::append_env_local(repo.root(), &spec.name, &value)?;
            // Values persisted this run stay visible to later variables, for
            // `${VAR}` expansion and skip-if-set.
            env.insert(spec.name.clone(), value);
        }
    }
    Ok(())
}

/// Obtain one variable's value without prompting — `--use-defaults`, the CI
/// mode: the computed default, else an existing environment
/// value, else the empty string with a warning for required variables.
fn default_value(
    spec: &VariableSpec,
    env: &BTreeMap<String, String>,
    host: &HostFacts,
    console: &mut dyn Console,
) -> Result<String> {
    let computed = match spec.kind {
        Some(VariableKind::Password | VariableKind::Generated) => Some(generate_secret()?),
        Some(VariableKind::DockerGroup) => host.docker_gid.clone(),
        Some(VariableKind::Hostname) => host.hostname.clone(),
        Some(VariableKind::Boolean) | None => spec
            .default_value
            .as_deref()
            .map(|default| expand(default, env)),
    };
    let value = computed
        .filter(|value| !value.is_empty())
        .or_else(|| env.get(&spec.name).cloned())
        .unwrap_or_default();
    if value.is_empty() && !spec.optional {
        console.warn(&format!(
            "warning: no default for required variable {}; persisting the empty string",
            spec.name
        ));
    }
    Ok(value)
}

/// Obtain one variable's value interactively, per its type.
fn prompted_value(
    spec: &VariableSpec,
    env: &BTreeMap<String, String>,
    host: &HostFacts,
    console: &mut dyn Console,
) -> Result<String> {
    match spec.kind {
        None => {
            let suggestion = spec
                .default_value
                .as_deref()
                .map_or_default(|default| expand(default, env));
            text_prompt(&spec.description, &suggestion, spec.optional, console)
        }
        Some(VariableKind::Password) => {
            let prompt = format!("{} (leave empty to generate): ", spec.description);
            let answer = console.ask(&prompt)?;
            if answer.is_empty() {
                generate_secret()
            } else {
                Ok(answer)
            }
        }
        Some(VariableKind::Generated) => generate_secret(),
        Some(VariableKind::Boolean) => boolean_prompt(&spec.description, console),
        Some(VariableKind::DockerGroup) => text_prompt(
            &spec.description,
            host.docker_gid.as_deref().unwrap_or_default(),
            spec.optional,
            console,
        ),
        Some(VariableKind::Hostname) => text_prompt(
            &spec.description,
            host.hostname.as_deref().unwrap_or_default(),
            spec.optional,
            console,
        ),
    }
}

/// The yes/no confirmation prompt of the `boolean` type:
/// persists `true`/`false`, re-prompting until the answer parses.
fn boolean_prompt(description: &str, console: &mut dyn Console) -> Result<String> {
    let prompt = format!("{description} (y/n): ");
    loop {
        match console.ask(&prompt)?.to_ascii_lowercase().as_str() {
            "y" | "yes" | "true" => return Ok("true".to_owned()),
            "n" | "no" | "false" => return Ok("false".to_owned()),
            _ => {}
        }
    }
}

/// Generate a random secret: 64 characters from `[A-Za-z0-9]`,
/// drawn from the OS CSPRNG with rejection sampling to stay uniform.
fn generate_secret() -> Result<String> {
    const CHARSET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    // Bytes at or above the largest multiple of 62 would bias the modulo.
    const UNBIASED_LIMIT: u8 = (u8::MAX / 62) * 62;
    let mut secret = String::with_capacity(64);
    let mut buffer = [0u8; 64];
    while secret.len() < 64 {
        getrandom::fill(&mut buffer).map_err(|error| anyhow::anyhow!("csprng failed: {error}"))?;
        for byte in buffer {
            if byte < UNBIASED_LIMIT {
                secret.push(char::from(CHARSET[usize::from(byte % 62)]));
                if secret.len() == 64 {
                    break;
                }
            }
        }
    }
    Ok(secret)
}

/// The plain text prompt: show the description — plus the
/// suggested value when there is one — and read an answer. An empty answer
/// accepts the suggestion; without one it is re-prompted unless `optional`,
/// where it stands for the empty string.
fn text_prompt(
    description: &str,
    suggestion: &str,
    optional: bool,
    console: &mut dyn Console,
) -> Result<String> {
    let prompt = if suggestion.is_empty() {
        format!("{description}: ")
    } else {
        format!("{description} [{suggestion}]: ")
    };
    loop {
        let answer = console.ask(&prompt)?;
        if !answer.is_empty() {
            return Ok(answer);
        }
        if !suggestion.is_empty() {
            return Ok(suggestion.to_owned());
        }
        if optional {
            return Ok(String::new());
        }
    }
}

/// Expand `${VAR}` references against the loaded environment;
/// an unset reference expands to the empty string.
fn expand(text: &str, env: &BTreeMap<String, String>) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let reference = &rest[start + 2..];
        let Some(end) = reference.find('}') else {
            // No closing brace: the remainder is literal text.
            expanded.push_str(&rest[start..]);
            return expanded;
        };
        expanded.push_str(env.get(&reference[..end]).map_or("", String::as_str));
        rest = &reference[end + 1..];
    }
    expanded.push_str(rest);
    expanded
}
