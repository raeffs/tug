//! Tests for `create-volumes`, `create-networks`, and `configure`,
//! driven through the recording seam against
//! fixture repos — no docker, no `../docker-host`.

mod common;

use std::collections::BTreeMap;

use tug::process::{Invocation, Outcome, Runner, StdoutSink};
use tug::provision;
use tug::repo::Repo;
use tug::vars::{Console, HostFacts};

/// A console that must never be asked anything — `configure --use-defaults`
/// is the CI path — but records warnings.
struct SilentConsole {
    warnings: Vec<String>,
}

impl Console for SilentConsole {
    fn ask(&mut self, prompt: &str) -> anyhow::Result<String> {
        panic!("unexpected prompt: {prompt}");
    }

    fn warn(&mut self, message: &str) {
        self.warnings.push(message.to_owned());
    }
}

fn no_host_facts() -> HostFacts {
    HostFacts {
        docker_gid: None,
        hostname: None,
    }
}

/// A runner that deals the given stdout to captured-output queries and
/// records every invocation, like the docker CLI answering `ls` queries.
struct QueryRunner {
    query_stdout: String,
    /// Exit non-zero on the Nth invocation (0-based), like a failing child.
    fail_at: Option<usize>,
    recorded: Vec<Invocation>,
}

impl QueryRunner {
    fn listing(query_stdout: &str) -> Self {
        Self {
            query_stdout: query_stdout.to_owned(),
            fail_at: None,
            recorded: Vec::new(),
        }
    }

    fn failing_at(fail_at: usize) -> Self {
        Self {
            query_stdout: String::new(),
            fail_at: Some(fail_at),
            recorded: Vec::new(),
        }
    }
}

impl Runner for QueryRunner {
    fn run(&mut self, invocation: &Invocation) -> anyhow::Result<Outcome> {
        self.recorded.push(invocation.clone());
        let exit_code = if self.fail_at == Some(self.recorded.len() - 1) {
            17
        } else {
            0
        };
        let stdout = (invocation.stdout == StdoutSink::Capture).then(|| self.query_stdout.clone());
        Ok(Outcome { exit_code, stdout })
    }
}

/// A process environment with nothing tug-relevant set (env name `local`).
fn plain_env() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn argv_of(recorded: &[Invocation]) -> Vec<Vec<String>> {
    recorded.iter().map(|i| i.argv.clone()).collect()
}

#[test]
fn configure_runs_variables_then_volumes_then_networks() {
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing("");
    let mut console = SilentConsole {
        warnings: Vec::new(),
    };

    let code = provision::configure(
        &repo,
        &plain_env(),
        "bonob",
        true,
        &no_host_facts(),
        &mut console,
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 0);
    // create-variables ran first (--use-defaults passed through: no prompts,
    // values persisted to .env.local)…
    let env_local = std::fs::read_to_string(root.join(".env.local")).unwrap();
    for name in [
        "DEFAULT_DOMAIN_SUFFIX",
        "TRAEFIK_DOMAIN",
        "NAVIDROME_DOMAIN",
        "BONOB_DOMAIN",
    ] {
        assert!(env_local.contains(name), ".env.local misses {name}");
    }
    // …then volumes, then networks, each with its single up-front query.
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "volume", "ls", "--format", "json"],
            vec!["docker", "volume", "create", "traefik-certificates"],
            vec![
                "docker",
                "run",
                "--rm",
                "-v",
                "traefik-certificates:/volume",
                "--entrypoint",
                "/bin/sh",
                "busybox",
                "-c",
                "chown -R 65001:65001 /volume",
            ],
            vec!["docker", "volume", "create", "navidrome-music"],
            vec!["docker", "network", "ls", "--format", "json"],
            vec![
                "docker",
                "network",
                "create",
                "--subnet",
                "172.20.0.0/24",
                "--ip-range",
                "172.20.0.128/25",
                "default-frontend",
            ],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}

#[test]
fn configure_stops_at_the_first_failing_step() {
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    // Invocation 1 is `docker volume create traefik-certificates`.
    let mut runner = QueryRunner::failing_at(1);
    let mut console = SilentConsole {
        warnings: Vec::new(),
    };

    let code = provision::configure(
        &repo,
        &plain_env(),
        "bonob",
        true,
        &no_host_facts(),
        &mut console,
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 17);
    // create-volumes stopped mid-cascade; create-networks never started.
    assert_eq!(runner.recorded.len(), 2);
}

#[test]
fn create_volumes_queries_once_then_creates_the_cascade_in_order() {
    // The chain fixture: bonob -> [networks, traefik, navidrome]; traefik
    // declares traefik-certificates (userId 65001), navidrome declares
    // navidrome-music (plain string form).
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing("");

    let code = provision::create_volumes(&repo, &plain_env(), "bonob", false, &mut runner).unwrap();

    assert_eq!(code, 0);
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "volume", "ls", "--format", "json"],
            vec!["docker", "volume", "create", "traefik-certificates"],
            vec![
                "docker",
                "run",
                "--rm",
                "-v",
                "traefik-certificates:/volume",
                "--entrypoint",
                "/bin/sh",
                "busybox",
                "-c",
                "chown -R 65001:65001 /volume",
            ],
            vec!["docker", "volume", "create", "navidrome-music"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
    // The ls query is machine-readable: its stdout is captured, never
    // scraped from the terminal.
    assert_eq!(runner.recorded[0].stdout, StdoutSink::Capture);
    // Everything runs from the repo root.
    for invocation in &runner.recorded {
        assert_eq!(invocation.cwd, root.to_string_lossy());
    }
}

#[test]
fn create_volumes_skips_volumes_that_already_exist() {
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    // One row per line, with the unrelated fields docker actually emits.
    let mut runner = QueryRunner::listing(concat!(
        r#"{"Availability":"N/A","Driver":"local","Name":"traefik-certificates","Scope":"local"}"#,
        "\n",
        r#"{"Availability":"N/A","Driver":"local","Name":"unrelated","Scope":"local"}"#,
        "\n",
    ));

    let code = provision::create_volumes(&repo, &plain_env(), "bonob", false, &mut runner).unwrap();

    assert_eq!(code, 0);
    // traefik-certificates exists: no create, no chown. Only the missing
    // navidrome-music is created.
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "volume", "ls", "--format", "json"],
            vec!["docker", "volume", "create", "navidrome-music"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}

#[test]
fn create_volumes_stops_at_the_first_failing_child_and_propagates_its_exit_code() {
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    // Invocation 1 is `docker volume create traefik-certificates`.
    let mut runner = QueryRunner::failing_at(1);

    let code = provision::create_volumes(&repo, &plain_env(), "bonob", false, &mut runner).unwrap();

    assert_eq!(code, 17);
    // The chain stops: no chown for the failed volume, no navidrome-music.
    assert_eq!(runner.recorded.len(), 2);
}

#[test]
fn create_volumes_rejects_an_unknown_project_listing_the_valid_names() {
    let root = common::fixture("provision");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing("");

    let error = provision::create_volumes(&repo, &plain_env(), "nope", false, &mut runner)
        .unwrap_err()
        .to_string();

    assert!(error.contains("unknown project"), "got: {error}");
    assert!(error.contains("app, base"), "got: {error}");
    // Nothing was spawned, not even the ls query.
    assert_eq!(runner.recorded, []);
}

#[test]
fn create_volumes_dedupes_first_wins_and_honors_custom_chown_image_and_mount_path() {
    // The provision fixture: app -> [base]; base declares shared-data with
    // userId 1000, image alpine, mountPath /data; app re-declares shared-data
    // (userId 9999) plus the plain-string app-data.
    let root = common::fixture("provision");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing("");

    let code = provision::create_volumes(&repo, &plain_env(), "app", false, &mut runner).unwrap();

    assert_eq!(code, 0);
    // shared-data appears once, with base's spec (first occurrence wins).
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "volume", "ls", "--format", "json"],
            vec!["docker", "volume", "create", "shared-data"],
            vec![
                "docker",
                "run",
                "--rm",
                "-v",
                "shared-data:/data",
                "--entrypoint",
                "/bin/sh",
                "alpine",
                "-c",
                "chown -R 1000:1000 /data",
            ],
            vec!["docker", "volume", "create", "app-data"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}

#[test]
fn create_networks_queries_once_then_creates_missing_networks() {
    // The chain fixture: bonob's cascade includes the graph-only networks
    // project, which declares default-frontend.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing("");

    let code = provision::create_networks(&repo, &plain_env(), "bonob", &mut runner).unwrap();

    assert_eq!(code, 0);
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "network", "ls", "--format", "json"],
            vec![
                "docker",
                "network",
                "create",
                "--subnet",
                "172.20.0.0/24",
                "--ip-range",
                "172.20.0.128/25",
                "default-frontend",
            ],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
    assert_eq!(runner.recorded[0].stdout, StdoutSink::Capture);
}

#[test]
fn create_networks_dedupes_first_wins_and_skips_existing_networks() {
    // The provision fixture: base declares frontend first (10.0.0.0/24);
    // app re-declares it (10.9.9.0/24) and adds backend.
    let root = common::fixture("provision");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing(concat!(
        r#"{"Driver":"bridge","Name":"frontend","Scope":"local"}"#,
        "\n",
        r#"{"Driver":"bridge","Name":"bridge","Scope":"local"}"#,
        "\n",
    ));

    let code = provision::create_networks(&repo, &plain_env(), "app", &mut runner).unwrap();

    assert_eq!(code, 0);
    // frontend exists (and dedupe kept only one spec for it); backend is
    // the single missing network.
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "network", "ls", "--format", "json"],
            vec![
                "docker",
                "network",
                "create",
                "--subnet",
                "10.1.0.0/24",
                "--ip-range",
                "10.1.0.0/25",
                "backend",
            ],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}

#[test]
fn create_volumes_force_removes_and_recreates_existing_volumes() {
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = QueryRunner::listing(concat!(
        r#"{"Driver":"local","Name":"traefik-certificates","Scope":"local"}"#,
        "\n",
    ));

    let code = provision::create_volumes(&repo, &plain_env(), "bonob", true, &mut runner).unwrap();

    assert_eq!(code, 0);
    // The existing volume is removed, then recreated (chown included);
    // missing volumes are created as usual — no rm.
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            vec!["docker", "volume", "ls", "--format", "json"],
            vec!["docker", "volume", "rm", "traefik-certificates"],
            vec!["docker", "volume", "create", "traefik-certificates"],
            vec![
                "docker",
                "run",
                "--rm",
                "-v",
                "traefik-certificates:/volume",
                "--entrypoint",
                "/bin/sh",
                "busybox",
                "-c",
                "chown -R 65001:65001 /volume",
            ],
            vec!["docker", "volume", "create", "navidrome-music"],
        ]
        .into_iter()
        .map(|argv| argv.into_iter().map(String::from).collect::<Vec<_>>())
        .collect::<Vec<_>>()
    );
}
