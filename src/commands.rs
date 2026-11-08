//! The lifecycle commands on top of the process execution
//! model. Every docker invocation is built as data and handed to
//! the injected [`Runner`], so `--dry-run` and tests are just different
//! adapters at the same seam.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::env;
use crate::graph::{Depth, Direction};
use crate::process::{Invocation, Runner};
use crate::repo::{Project, Repo};

/// Options of `up`.
#[derive(Debug, Clone, Copy)]
pub struct UpOptions {
    /// Maps to compose's `--force-recreate`, applied to the whole chain.
    pub force: bool,
    /// `--detach` is on by default; `--no-detach` turns it off chain-wide.
    pub detach: bool,
    /// `Some(secs)` blocks each leg until every container reports healthy —
    /// compose's `--wait --wait-timeout <secs>`, applied per leg so every
    /// dependency is healthy before the next starts. `None` skips the wait. It
    /// is never `Some` alongside `!detach`: compose rejects `--wait` in
    /// attached mode.
    pub wait_timeout: Option<u32>,
    /// Start the named project alone, skipping the dependency cascade. This
    /// cuts tug's `implicitDependencies` walk only — the single invocation is
    /// unchanged, so compose still starts the project's own `depends_on`
    /// services. Compose's like-named `--no-deps` is deliberately not passed:
    /// the compose project is tug's atomic unit.
    pub no_deps: bool,
}

impl Default for UpOptions {
    fn default() -> Self {
        Self {
            force: false,
            detach: true,
            wait_timeout: None,
            no_deps: false,
        }
    }
}

/// Options of `restart`. No `force`: after the down leg the
/// containers are gone, so compose's `--force-recreate` could never matter.
#[derive(Debug, Clone, Copy)]
pub struct RestartOptions {
    /// `--detach` is on by default on the up leg; `--no-detach` turns it off.
    pub detach: bool,
}

impl Default for RestartOptions {
    fn default() -> Self {
        Self { detach: true }
    }
}

/// `up <project>`: start the project and all its transitive
/// dependencies, dependencies first, sequentially. Graph-only projects in
/// the chain are skipped.
///
/// Returns the CLI exit code: `0` on success, the first failing child's
/// exit code otherwise — the chain stops at the first failure.
///
/// # Errors
///
/// Fails on an unknown or graph-only project name, or when a child cannot
/// be spawned at all.
pub fn up(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    options: &UpOptions,
    runner: &mut dyn Runner,
) -> Result<i32> {
    let named = named_compose_project(repo, name)?;
    // Formatted once here so the string outlives `args`, which borrows it below.
    let wait_timeout = options.wait_timeout.map(|secs| secs.to_string());
    let mut args = vec!["up", "--yes", "--remove-orphans"];
    if options.detach {
        args.push("--detach");
    }
    if options.force {
        args.push("--force-recreate");
    }
    if let Some(timeout) = &wait_timeout {
        args.extend(["--wait", "--wait-timeout", timeout.as_str()]);
    }
    // `--no-deps` collapses the cascade to the named project, which
    // `named_compose_project` has already proven is a compose project.
    let chain = if options.no_deps {
        vec![named]
    } else {
        repo.dependency_order(name)?
    };
    for project in chain {
        if !project.has_compose_file {
            continue;
        }
        let invocation = compose_invocation(repo, project, process_env, &args)?;
        let outcome = runner.run(&invocation)?;
        if outcome.exit_code != 0 {
            return Ok(outcome.exit_code);
        }
    }
    Ok(0)
}

/// `down <project>`: take down only the named project — no
/// dependency cascade. Returns the CLI exit code like [`up`].
///
/// # Errors
///
/// Fails on an unknown or graph-only project name, or when the child cannot
/// be spawned at all.
pub fn down(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    runner: &mut dyn Runner,
) -> Result<i32> {
    single_project_command(
        repo,
        process_env,
        name,
        &["down", "--remove-orphans"],
        runner,
    )
}

/// `restart <project>`: strict bounce — a non-cascading down
/// leg, then a non-cascading up leg, both on the named project alone. The
/// state of every other project is exactly as before the restart.
/// Returns the CLI exit code like [`up`].
///
/// # Errors
///
/// Fails on an unknown or graph-only project name, or when a child cannot
/// be spawned at all.
pub fn restart(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    options: &RestartOptions,
    runner: &mut dyn Runner,
) -> Result<i32> {
    let project = named_compose_project(repo, name)?;
    let down = compose_invocation(repo, project, process_env, &["down", "--remove-orphans"])?;
    let outcome = runner.run(&down)?;
    if outcome.exit_code != 0 {
        return Ok(outcome.exit_code);
    }
    let mut up_args = vec!["up", "--yes", "--remove-orphans"];
    if options.detach {
        up_args.push("--detach");
    }
    let up = compose_invocation(repo, project, process_env, &up_args)?;
    Ok(runner.run(&up)?.exit_code)
}

/// `logs <project> [--follow]`: stream the named project's
/// compose logs — no dependency cascade, the same single-project shape as
/// `down`/`pull`. `--follow` appends compose's `--follow`; output streams
/// through inherited stdio, and under `--follow` the child runs until
/// interrupted, at which point the process-wide handler forwards the
/// SIGINT/SIGTERM to it and its exit code propagates verbatim.
/// Returns the CLI exit code like [`down`].
///
/// # Errors
///
/// Fails on an unknown or graph-only project name, or when the child cannot
/// be spawned at all.
pub fn logs(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    follow: bool,
    runner: &mut dyn Runner,
) -> Result<i32> {
    let mut args = vec!["logs"];
    if follow {
        args.push("--follow");
    }
    single_project_command(repo, process_env, name, &args, runner)
}

/// `pull <project>`: pull only the named project — no cascade.
/// Returns the CLI exit code like [`up`].
///
/// # Errors
///
/// Fails on an unknown or graph-only project name, or when the child cannot
/// be spawned at all.
pub fn pull(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    runner: &mut dyn Runner,
) -> Result<i32> {
    single_project_command(repo, process_env, name, &["pull"], runner)
}

/// `list`: every discovered project with its direct dependencies, sorted
/// by name — one line each, dependencies in declaration order. What counts
/// as discovered is settled before this ever runs: a project inside an
/// ignored directory does not exist.
#[must_use]
pub fn list(repo: &Repo) -> String {
    let mut listing = String::new();
    for project in repo.projects() {
        listing.push_str(&edge_line(&project.name, &project.implicit_dependencies));
    }
    listing
}

/// Options of `deps`.
#[derive(Debug, Clone, Copy)]
pub struct DepsOptions {
    /// Forward = dependencies, Reverse = dependents.
    pub direction: Direction,
    /// Direct edges only, or the full transitive closure.
    pub depth: Depth,
    /// Emit the stable JSON object instead of the human `list`-style line.
    pub json: bool,
}

/// The `--json` object of `deps`: a stable, direction-agnostic shape. `deps`
/// carries both dependencies and dependents — its meaning is fixed by the
/// echoed `direction`/`transitive` fields, so CI never has to scrape text.
/// Field order here is the wire order; keep it stable across releases.
#[derive(Debug, Serialize)]
struct DepsReport<'a> {
    project: &'a str,
    direction: &'static str,
    transitive: bool,
    deps: Vec<&'a str>,
}

/// `deps <project>`: report a project's dependency edges in
/// the chosen direction and depth. The result set — the queried project
/// excluded, deduplicated, sorted — is rendered either as a `list`-style line
/// (human default) or as the stable [`DepsReport`] JSON object (CI's `--json`).
///
/// # Errors
///
/// Fails when `name` is not a known project (a typo fails loudly rather than
/// returning an empty set), or when JSON serialization fails.
pub fn deps(repo: &Repo, name: &str, options: &DepsOptions) -> Result<String> {
    // `related` validates the name; on success `name` is an exact project key,
    // so it is already the canonical spelling — no second lookup needed.
    let names = repo.related(name, options.direction, options.depth)?;
    if options.json {
        let report = DepsReport {
            project: name,
            direction: options.direction.label(),
            transitive: matches!(options.depth, Depth::Transitive),
            deps: names,
        };
        let mut line = serde_json::to_string(&report)
            .context("failed to serialize the deps report as JSON")?;
        line.push('\n');
        Ok(line)
    } else {
        Ok(edge_line(name, &names))
    }
}

/// One `name (a, b, …)` line — the shared shape of `list` and text `deps`: the
/// name alone when there are no edges, else the name with its edges in parens.
fn edge_line(name: &str, edges: &[impl AsRef<str>]) -> String {
    let mut line = name.to_owned();
    if !edges.is_empty() {
        line.push_str(" (");
        for (index, edge) in edges.iter().enumerate() {
            if index > 0 {
                line.push_str(", ");
            }
            line.push_str(edge.as_ref());
        }
        line.push(')');
    }
    line.push('\n');
    line
}

/// One row of the `docker compose ls --format json` listing;
/// unknown fields are ignored, like everywhere else docker output is read.
#[derive(Debug, Clone, Deserialize)]
struct ComposeLsEntry {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "ConfigFiles")]
    config_files: String,
}

/// `ps`: the repository's running projects, from one
/// machine-readable `docker compose ls` query. Compose derives a
/// project's name from its directory, so entries are matched to
/// discovered projects by directory name and displayed under the
/// `project.json` name. Non-repo compose projects are excluded.
///
/// # Errors
///
/// Returns an error when the listing cannot be parsed or the child cannot
/// be spawned; a non-zero query exit is propagated in the pair.
pub fn ps(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    runner: &mut dyn Runner,
) -> Result<(i32, String)> {
    let (exit_code, matched) = running_projects(repo, process_env, runner)?;

    let mut rows: Vec<(String, String, String)> = Vec::new();
    for (project, entry) in &matched {
        let files = relative_config_files(repo, &entry.config_files);
        rows.push((project.name.clone(), entry.status.clone(), files));
    }

    let name_width = rows.iter().map(|(n, ..)| n.len()).max().unwrap_or(0);
    let status_width = rows.iter().map(|(_, s, _)| s.len()).max().unwrap_or(0);
    let mut listing = String::new();
    for (name, status, files) in rows {
        use std::fmt::Write;
        let _ = writeln!(
            listing,
            "{name:<name_width$}  {status:<status_width$}  {files}"
        );
    }
    // `matched` is empty on a failed query, so `listing` is already empty;
    // the query's own exit code is what propagates.
    Ok((exit_code, listing))
}

/// The verbs `tug all` fans out over the running set. No `up`:
/// over the running set it is a near no-op, and over every project it would
/// start deliberately-down services — the batching `up` needs is already a
/// project's own dependency cascade.
#[derive(Debug, Clone, Copy)]
pub enum BulkVerb {
    Down,
    Restart,
    Pull,
}

impl BulkVerb {
    /// The word shown in the summary line and the CLI surface.
    fn label(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Restart => "restart",
            Self::Pull => "pull",
        }
    }
}

/// `all <verb>`: run one lifecycle verb over every **running**
/// project. The running set is exactly `ps`'s selection (one `docker compose
/// ls` query, matched by directory name), so `all` and `ps` never disagree.
/// Order is direction-aware: `down` tears down dependents first
/// (reverse-topological), `restart`/`pull` go dependencies first
/// (topological) — the house order, dependencies first.
///
/// The batch is best-effort: a project whose op exits non-zero is recorded
/// and the batch continues (each project still runs sequentially, stopping at
/// its own first failure). The
/// returned exit code is the first non-zero one seen, else `0`; the returned
/// summary is one human line. A genuine spawn failure still aborts via `?`.
///
/// # Errors
///
/// Fails when the graph has a cycle, the selection listing cannot be parsed,
/// or a child cannot be spawned at all.
pub fn all(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    verb: BulkVerb,
    runner: &mut dyn Runner,
) -> Result<(i32, String)> {
    let (exit_code, matched) = running_projects(repo, process_env, runner)?;
    if exit_code != 0 {
        return Ok((exit_code, String::new()));
    }
    if matched.is_empty() {
        return Ok((0, "no running projects\n".to_string()));
    }

    let running: HashSet<&str> = matched.iter().map(|(p, _)| p.name.as_str()).collect();
    let mut ordered: Vec<&Project> = repo
        .topological_order()?
        .into_iter()
        .filter(|project| running.contains(project.name.as_str()))
        .collect();
    if matches!(verb, BulkVerb::Down) {
        ordered.reverse();
    }

    let total = ordered.len();
    let mut first_failure = 0;
    let mut failed: Vec<&str> = Vec::new();
    for project in ordered {
        let code = run_verb(repo, process_env, verb, &project.name, runner)?;
        if code != 0 {
            if first_failure == 0 {
                first_failure = code;
            }
            failed.push(&project.name);
        }
    }

    let ok = total - failed.len();
    let summary = if failed.is_empty() {
        format!("all {}: {ok} ok\n", verb.label())
    } else {
        format!(
            "all {}: {ok} ok, {} failed ({})\n",
            verb.label(),
            failed.len(),
            failed.join(", "),
        )
    };
    Ok((first_failure, summary))
}

/// Run one `all` verb against a single project by delegating to the existing
/// per-project command, so the invocation shape is identical to naming that
/// project directly.
fn run_verb(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    verb: BulkVerb,
    name: &str,
    runner: &mut dyn Runner,
) -> Result<i32> {
    match verb {
        BulkVerb::Down => down(repo, process_env, name, runner),
        BulkVerb::Pull => pull(repo, process_env, name, runner),
        BulkVerb::Restart => restart(repo, process_env, name, &RestartOptions::default(), runner),
    }
}

/// The repository's **running** projects, paired with their `docker compose
/// ls` entry, in name order. One machine-readable query,
/// matched to discovered compose projects by directory name — the
/// shared basis of `ps` and `all <verb>`. On a failed query the exit code is
/// returned with an empty match list.
///
/// # Errors
///
/// Returns an error when the listing cannot be parsed or the child cannot be
/// spawned.
fn running_projects<'repo>(
    repo: &'repo Repo,
    process_env: &BTreeMap<String, String>,
    runner: &mut dyn Runner,
) -> Result<(i32, Vec<(&'repo Project, ComposeLsEntry)>)> {
    let root = repo.root().to_string_lossy().into_owned();
    let mut argv: Vec<String> = vec!["docker".into(), "compose".into()];
    if process_env.contains_key("CI") {
        argv.extend(["--ansi", "never", "--progress", "plain"].map(String::from));
    }
    argv.extend(["ls", "--format", "json"].map(String::from));
    let query = Invocation::new(argv, &root)
        .with_env(process_env.clone())
        .with_captured_stdout();
    let outcome = runner.run(&query)?;
    if outcome.exit_code != 0 {
        return Ok((outcome.exit_code, Vec::new()));
    }
    let entries: Vec<ComposeLsEntry> =
        serde_json::from_str(outcome.stdout.as_deref().unwrap_or("[]"))
            .context("failed to parse the docker compose ls listing")?;

    let mut matched = Vec::new();
    for project in repo.projects().filter(|p| p.has_compose_file) {
        let dir_name = project.dir.file_name().unwrap_or_default();
        if let Some(entry) = entries.iter().find(|e| dir_name == e.name.as_str()) {
            matched.push((project, entry.clone()));
        }
    }
    Ok((0, matched))
}

/// Render a compose ls `ConfigFiles` value (comma-separated absolute
/// paths) relative to the repo root for readability.
fn relative_config_files(repo: &Repo, config_files: &str) -> String {
    config_files
        .split(',')
        .map(|file| {
            let path = std::path::Path::new(file);
            path.strip_prefix(repo.root())
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The shared cascade-free shape of `down` and `pull`.
fn single_project_command(
    repo: &Repo,
    process_env: &BTreeMap<String, String>,
    name: &str,
    args: &[&str],
    runner: &mut dyn Runner,
) -> Result<i32> {
    let project = named_compose_project(repo, name)?;
    let invocation = compose_invocation(repo, project, process_env, args)?;
    Ok(runner.run(&invocation)?.exit_code)
}

/// Look up the project a lifecycle command names: an
/// unknown name errors listing the valid names, and a graph-only project —
/// no `docker-compose.yml` — cannot be named directly.
fn named_compose_project<'repo>(repo: &'repo Repo, name: &str) -> Result<&'repo Project> {
    let project = repo.project_or_bail(name)?;
    if !project.has_compose_file {
        bail!("project {name:?} is graph-only (it has no docker-compose.yml)");
    }
    Ok(project)
}

/// One `docker compose` invocation for `project`: repo-root cwd,
/// repo-relative `--file` paths with the environment overlay (named by the
/// process environment or the repo-root dotenv files, see
/// `env::environment_name`), the merged environment, and the CI flags when
/// `CI` is set in the process environment alone.
fn compose_invocation(
    repo: &Repo,
    project: &Project,
    process_env: &BTreeMap<String, String>,
    compose_args: &[&str],
) -> Result<Invocation> {
    let env_name = env::environment_name(process_env, repo.root())?;
    let merged = env::merged_env(process_env, repo.root(), &project.dir)?;
    let mut argv: Vec<String> = vec!["docker".into(), "compose".into()];
    if process_env.contains_key("CI") {
        argv.extend(["--ansi", "never", "--progress", "plain"].map(String::from));
    }
    for file in env::compose_files(repo.root(), &project.dir, &env_name) {
        argv.push("--file".into());
        argv.push(file.to_string_lossy().into_owned());
    }
    argv.extend(compose_args.iter().map(ToString::to_string));
    Ok(Invocation::new(argv, repo.root().to_string_lossy()).with_env(merged))
}
