//! Tests for the lifecycle commands and the process
//! execution model, driven through the recording seam against
//! fixture repos — no docker, no `../docker-host`.

mod common;

use std::collections::BTreeMap;

use tug::commands::{self, DepsOptions};
use tug::graph::{Depth, Direction};
use tug::process::{DryRunRunner, Invocation, Outcome, RecordingRunner, Runner, StdoutSink};
use tug::repo::Repo;

/// A runner that answers captured-stdout queries with the given text and
/// records every invocation, like the docker CLI answering `compose ls`.
struct ComposeLsRunner {
    stdout: String,
    recorded: Vec<Invocation>,
}

impl ComposeLsRunner {
    fn listing(stdout: &str) -> Self {
        Self {
            stdout: stdout.to_owned(),
            recorded: Vec::new(),
        }
    }
}

impl Runner for ComposeLsRunner {
    fn run(&mut self, invocation: &Invocation) -> anyhow::Result<Outcome> {
        self.recorded.push(invocation.clone());
        Ok(Outcome {
            exit_code: 0,
            stdout: Some(self.stdout.clone()),
        })
    }
}

/// A runner whose Nth invocation (0-based) exits with the given code.
struct FailingRunner {
    fail_at: usize,
    exit_code: i32,
    recorded: Vec<Invocation>,
}

impl Runner for FailingRunner {
    fn run(&mut self, invocation: &Invocation) -> anyhow::Result<Outcome> {
        self.recorded.push(invocation.clone());
        let exit_code = if self.recorded.len() - 1 == self.fail_at {
            self.exit_code
        } else {
            0
        };
        Ok(Outcome {
            exit_code,
            stdout: None,
        })
    }
}

/// A process environment with nothing tug-relevant set (env name `local`).
fn plain_env() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[test]
fn up_runs_the_worked_example_chain_in_order() {
    // The worked example chain: bonob -> [networks, traefik,
    // navidrome], traefik/navidrome -> [networks], env `local`, overlays for
    // traefik and bonob only. networks is graph-only and skipped.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let code = commands::up(
        &repo,
        &plain_env(),
        "bonob",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 0);
    let recorded: Vec<Vec<String>> = runner
        .recorded()
        .iter()
        .map(|inv| inv.argv.clone())
        .collect();
    let expected: Vec<Vec<String>> = [
        [
            "docker",
            "compose",
            "--file",
            "apps/traefik/docker-compose.yml",
            "--file",
            "apps/traefik/docker-compose.local.yml",
            "up",
            "--yes",
            "--remove-orphans",
            "--detach",
        ]
        .as_slice(),
        [
            "docker",
            "compose",
            "--file",
            "apps/navidrome/docker-compose.yml",
            "up",
            "--yes",
            "--remove-orphans",
            "--detach",
        ]
        .as_slice(),
        [
            "docker",
            "compose",
            "--file",
            "apps/bonob/docker-compose.yml",
            "--file",
            "apps/bonob/docker-compose.local.yml",
            "up",
            "--yes",
            "--remove-orphans",
            "--detach",
        ]
        .as_slice(),
    ]
    .iter()
    .map(|argv| argv.iter().map(ToString::to_string).collect())
    .collect();
    assert_eq!(recorded, expected);
    // All three with cwd = repo root.
    for invocation in runner.recorded() {
        assert_eq!(invocation.cwd, root.to_string_lossy());
    }
}

#[test]
fn up_no_deps_starts_only_the_named_project() {
    // The counterpart of the worked example: --no-deps cuts the
    // implicitDependencies walk, so bonob starts alone — no traefik, no
    // navidrome — while the invocation itself is byte-for-byte the leg the
    // full cascade would have run for bonob.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::UpOptions {
        no_deps: true,
        ..commands::UpOptions::default()
    };
    let code = commands::up(&repo, &plain_env(), "bonob", &options, &mut runner).unwrap();

    assert_eq!(code, 0);
    let recorded: Vec<Vec<String>> = runner
        .recorded()
        .iter()
        .map(|inv| inv.argv.clone())
        .collect();
    assert_eq!(
        recorded,
        vec![
            [
                "docker",
                "compose",
                "--file",
                "apps/bonob/docker-compose.yml",
                "--file",
                "apps/bonob/docker-compose.local.yml",
                "up",
                "--yes",
                "--remove-orphans",
                "--detach",
            ]
            .map(ToString::to_string)
            .to_vec()
        ],
    );
}

#[test]
fn up_no_deps_composes_with_the_other_up_flags() {
    // --no-deps only decides *which* projects run; it must not change the
    // shape of the leg. The CI-facing combination is --no-deps --wait, so
    // pair it with --force too and check the argv is exactly what the
    // cascading form emits for this project.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::UpOptions {
        no_deps: true,
        force: true,
        wait_timeout: Some(45),
        ..commands::UpOptions::default()
    };
    commands::up(&repo, &plain_env(), "navidrome", &options, &mut runner).unwrap();

    assert_eq!(runner.recorded().len(), 1);
    assert_eq!(
        runner.recorded()[0].argv,
        [
            "docker",
            "compose",
            "--file",
            "apps/navidrome/docker-compose.yml",
            "up",
            "--yes",
            "--remove-orphans",
            "--detach",
            "--force-recreate",
            "--wait",
            "--wait-timeout",
            "45",
        ]
        .map(ToString::to_string),
    );
}

#[test]
fn up_no_deps_still_rejects_a_graph_only_project_named_directly() {
    // Skipping the cascade does not make a graph-only project startable:
    // there is still no docker-compose.yml to hand to compose.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::UpOptions {
        no_deps: true,
        ..commands::UpOptions::default()
    };
    let error = commands::up(&repo, &plain_env(), "networks", &options, &mut runner).unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains("networks") && message.contains("compose"),
        "explains the project has no compose file: {message}"
    );
    assert!(runner.recorded().is_empty(), "nothing spawned");
}

#[test]
fn up_maps_no_detach_and_force_onto_every_invocation_in_the_chain() {
    // --no-detach omits --detach chain-wide; --force maps to compose's
    // --force-recreate chain-wide.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::UpOptions {
        force: true,
        detach: false,
        ..commands::UpOptions::default()
    };
    commands::up(&repo, &plain_env(), "bonob", &options, &mut runner).unwrap();

    assert_eq!(runner.recorded().len(), 3);
    for invocation in runner.recorded() {
        let tail = &invocation.argv[invocation.argv.len() - 4..];
        assert_eq!(
            tail,
            ["up", "--yes", "--remove-orphans", "--force-recreate"]
        );
    }
}

#[test]
fn up_wait_appends_wait_and_the_default_timeout_to_every_leg() {
    // --wait blocks until healthy, appending `--wait --wait-timeout <n>` per
    // compose up. tug runs one compose up per project in dependency order, so
    // --wait applies to each leg — every dependency waits healthy before the
    // next starts. The flags land after --detach/--force-recreate.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::UpOptions {
        wait_timeout: Some(120),
        ..commands::UpOptions::default()
    };
    commands::up(&repo, &plain_env(), "bonob", &options, &mut runner).unwrap();

    assert_eq!(runner.recorded().len(), 3);
    for invocation in runner.recorded() {
        let tail = &invocation.argv[invocation.argv.len() - 7..];
        assert_eq!(
            tail,
            [
                "up",
                "--yes",
                "--remove-orphans",
                "--detach",
                "--wait",
                "--wait-timeout",
                "120",
            ],
            "wait flags follow --detach on every leg with the default timeout: {:?}",
            invocation.argv,
        );
    }
}

#[test]
fn up_wait_honors_a_custom_timeout() {
    // --wait-timeout <secs> overrides the default 120. The value rides on
    // every leg.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::UpOptions {
        wait_timeout: Some(45),
        ..commands::UpOptions::default()
    };
    commands::up(&repo, &plain_env(), "navidrome", &options, &mut runner).unwrap();

    let invocation = runner.recorded().last().unwrap();
    let tail = &invocation.argv[invocation.argv.len() - 4..];
    assert_eq!(tail, ["--detach", "--wait", "--wait-timeout", "45"]);
}

#[test]
fn up_prepends_the_ci_flags_and_picks_the_ci_overlay_when_ci_is_set() {
    // CI set (even empty) prepends --ansi never --progress plain to every
    // compose invocation; the env name becomes `ci`, so the local
    // overlays no longer apply (no docker-compose.ci.yml in the fixture).
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();
    let ci_env = BTreeMap::from([("CI".to_string(), String::new())]);

    commands::up(
        &repo,
        &ci_env,
        "bonob",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap();

    let first = &runner.recorded()[0];
    assert_eq!(
        first.argv,
        [
            "docker",
            "compose",
            "--ansi",
            "never",
            "--progress",
            "plain",
            "--file",
            "apps/traefik/docker-compose.yml",
            "up",
            "--yes",
            "--remove-orphans",
            "--detach",
        ]
    );
}

#[test]
fn up_gives_every_child_the_merged_environment_of_its_project() {
    // the child's environment is the merged environment — process env wins over
    // the project's dotenv layers, which win over the repo root's.
    let root = common::fixture("chain");
    std::fs::write(root.join(".env"), "FROM_ROOT='root'\nSHADOWED='root'\n").unwrap();
    std::fs::write(
        root.join("apps/bonob/.env"),
        "FROM_PROJECT='bonob'\nSHADOWED='bonob'\n",
    )
    .unwrap();
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();
    let process_env = BTreeMap::from([("FROM_PROCESS".to_string(), "process".to_string())]);

    commands::up(
        &repo,
        &process_env,
        "bonob",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap();

    // The bonob invocation is last in the chain.
    let bonob = runner.recorded().last().unwrap();
    assert_eq!(bonob.env.get("FROM_PROCESS").unwrap(), "process");
    assert_eq!(bonob.env.get("FROM_ROOT").unwrap(), "root");
    assert_eq!(bonob.env.get("FROM_PROJECT").unwrap(), "bonob");
    assert_eq!(bonob.env.get("SHADOWED").unwrap(), "bonob");
    // The traefik invocation sees the root layer but not bonob's.
    let traefik = &runner.recorded()[0];
    assert_eq!(traefik.env.get("FROM_ROOT").unwrap(), "root");
    assert_eq!(traefik.env.get("SHADOWED").unwrap(), "root");
    assert!(!traefik.env.contains_key("FROM_PROJECT"));
}

#[test]
fn up_rejects_an_unknown_project_listing_the_valid_names() {
    // An unknown project name produces a clear error listing valid names.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let error = commands::up(
        &repo,
        &plain_env(),
        "sonarr",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("sonarr"), "names the culprit: {message}");
    for valid in ["bonob", "navidrome", "networks", "traefik"] {
        assert!(message.contains(valid), "lists {valid}: {message}");
    }
    assert!(runner.recorded().is_empty(), "nothing spawned");
}

#[test]
fn up_rejects_a_graph_only_project_named_directly() {
    // The cascade skips graph-only projects, but naming one directly
    // is an error.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let error = commands::up(
        &repo,
        &plain_env(),
        "networks",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains("networks") && message.contains("compose"),
        "explains the project has no compose file: {message}"
    );
    assert!(runner.recorded().is_empty(), "nothing spawned");
}

#[test]
fn up_stops_the_chain_at_the_first_failure_and_propagates_the_exit_code() {
    // Sequential execution, stopping at the first non-zero child and
    // propagating its exit code as the CLI's.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = FailingRunner {
        fail_at: 1, // navidrome, the second project in the chain
        exit_code: 17,
        recorded: Vec::new(),
    };

    let code = commands::up(
        &repo,
        &plain_env(),
        "bonob",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 17);
    assert_eq!(runner.recorded.len(), 2, "bonob is never attempted");
}

#[test]
fn down_takes_down_only_the_named_project() {
    // No dependency cascade — `down bonob` must not touch traefik.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let code = commands::down(&repo, &plain_env(), "bonob", &mut runner).unwrap();

    assert_eq!(code, 0);
    let recorded: Vec<Vec<String>> = runner
        .recorded()
        .iter()
        .map(|inv| inv.argv.clone())
        .collect();
    assert_eq!(
        recorded,
        vec![vec![
            "docker".to_string(),
            "compose".into(),
            "--file".into(),
            "apps/bonob/docker-compose.yml".into(),
            "--file".into(),
            "apps/bonob/docker-compose.local.yml".into(),
            "down".into(),
            "--remove-orphans".into(),
        ]]
    );
}

#[test]
fn restart_bounces_only_the_named_project_down_then_up() {
    // Strict bounce — a non-cascading down leg followed by a
    // non-cascading up leg on the named project alone; the dependency
    // chain (traefik, navidrome, networks) is never touched.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let code = commands::restart(
        &repo,
        &plain_env(),
        "bonob",
        &commands::RestartOptions::default(),
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 0);
    let recorded: Vec<Vec<String>> = runner
        .recorded()
        .iter()
        .map(|inv| inv.argv.clone())
        .collect();
    let files = [
        "--file",
        "apps/bonob/docker-compose.yml",
        "--file",
        "apps/bonob/docker-compose.local.yml",
    ];
    let expected: Vec<Vec<String>> = [
        [
            &["docker", "compose"],
            files.as_slice(),
            &["down", "--remove-orphans"] as &[_],
        ]
        .concat(),
        [
            &["docker", "compose"],
            files.as_slice(),
            &["up", "--yes", "--remove-orphans", "--detach"] as &[_],
        ]
        .concat(),
    ]
    .iter()
    .map(|argv| argv.iter().map(ToString::to_string).collect())
    .collect();
    assert_eq!(recorded, expected);
}

#[test]
fn restart_no_detach_omits_detach_from_the_up_leg_only() {
    // --no-detach means "bounce it and watch the logs" — the up
    // leg loses --detach; the down leg is unchanged.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let options = commands::RestartOptions { detach: false };
    commands::restart(&repo, &plain_env(), "navidrome", &options, &mut runner).unwrap();

    let recorded: Vec<Vec<String>> = runner
        .recorded()
        .iter()
        .map(|inv| inv.argv.clone())
        .collect();
    assert_eq!(
        recorded,
        vec![
            vec![
                "docker".to_string(),
                "compose".into(),
                "--file".into(),
                "apps/navidrome/docker-compose.yml".into(),
                "down".into(),
                "--remove-orphans".into(),
            ],
            vec![
                "docker".to_string(),
                "compose".into(),
                "--file".into(),
                "apps/navidrome/docker-compose.yml".into(),
                "up".into(),
                "--yes".into(),
                "--remove-orphans".into(),
            ],
        ]
    );
}

#[test]
fn restart_stops_after_a_failed_down_leg_and_propagates_its_exit_code() {
    // The down leg exits non-zero → the up leg
    // never runs and the child's exit code becomes the CLI's.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = FailingRunner {
        fail_at: 0, // the down leg
        exit_code: 17,
        recorded: Vec::new(),
    };

    let code = commands::restart(
        &repo,
        &plain_env(),
        "bonob",
        &commands::RestartOptions::default(),
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 17);
    assert_eq!(runner.recorded.len(), 1, "the up leg is never attempted");
}

#[test]
fn pull_pulls_only_the_named_project() {
    // No cascade, bare `pull` args.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let code = commands::pull(&repo, &plain_env(), "navidrome", &mut runner).unwrap();

    assert_eq!(code, 0);
    let recorded: Vec<Vec<String>> = runner
        .recorded()
        .iter()
        .map(|inv| inv.argv.clone())
        .collect();
    assert_eq!(
        recorded,
        vec![vec![
            "docker".to_string(),
            "compose".into(),
            "--file".into(),
            "apps/navidrome/docker-compose.yml".into(),
            "pull".into(),
        ]]
    );
}

#[test]
fn logs_shows_only_the_named_project_streamed() {
    // No dependency cascade — `logs navidrome` shows only
    // navidrome's logs (like `down`/`pull`), bare `logs` args, and streams
    // through inherited stdio (StdoutSink::Inherit).
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    let code = commands::logs(&repo, &plain_env(), "navidrome", false, &mut runner).unwrap();

    assert_eq!(code, 0);
    let [invocation] = runner.recorded() else {
        panic!("expected exactly one invocation");
    };
    assert_eq!(
        invocation.argv,
        [
            "docker",
            "compose",
            "--file",
            "apps/navidrome/docker-compose.yml",
            "logs",
        ]
    );
    assert_eq!(invocation.stdout, StdoutSink::Inherit, "logs streams");
}

#[test]
fn logs_follow_appends_the_follow_flag() {
    // --follow keeps streaming new output — it appends compose's
    // own --follow to the single invocation.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();

    commands::logs(&repo, &plain_env(), "navidrome", true, &mut runner).unwrap();

    let [invocation] = runner.recorded() else {
        panic!("expected exactly one invocation");
    };
    assert_eq!(
        invocation.argv,
        [
            "docker",
            "compose",
            "--file",
            "apps/navidrome/docker-compose.yml",
            "logs",
            "--follow",
        ]
    );
}

#[test]
fn down_pull_restart_and_logs_reject_graph_only_and_unknown_projects() {
    // Graph-only projects cannot be named; unknown
    // names error. All share the `named_compose_project` guard.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = RecordingRunner::new();
    let restart_options = commands::RestartOptions::default();

    for name in ["networks", "sonarr"] {
        commands::down(&repo, &plain_env(), name, &mut runner).unwrap_err();
        commands::pull(&repo, &plain_env(), name, &mut runner).unwrap_err();
        commands::restart(&repo, &plain_env(), name, &restart_options, &mut runner).unwrap_err();
        commands::logs(&repo, &plain_env(), name, false, &mut runner).unwrap_err();
    }

    assert!(runner.recorded().is_empty(), "nothing spawned");
}

#[test]
fn list_prints_every_project_with_its_direct_dependencies_sorted_by_name() {
    // One line per project, direct implicitDependencies only (in
    // declaration order), projects sorted by name.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();

    let listing = commands::list(&repo);

    assert_eq!(
        listing,
        "bonob (networks, traefik, navidrome)\n\
         navidrome (networks)\n\
         networks\n\
         traefik (networks)\n"
    );
}

#[test]
fn ps_lists_a_running_project_via_one_compose_ls_query() {
    // One machine-readable `docker compose ls` query;
    // a running project renders as name, compose status verbatim, and the
    // loaded config files relative to the repo root.
    let root = common::fixture("listing");
    let repo = Repo::discover(&root).unwrap();
    let stdout = format!(
        r#"[{{"Name":"whoami","Status":"running(1)","ConfigFiles":"{root}/apps/whoami/docker-compose.yml"}}]"#,
        root = root.display()
    );
    let mut runner = ComposeLsRunner::listing(&stdout);

    let (exit_code, listing) = commands::ps(&repo, &BTreeMap::new(), &mut runner).unwrap();

    assert_eq!(exit_code, 0);
    assert_eq!(
        listing,
        "whoami  running(1)  apps/whoami/docker-compose.yml\n"
    );
    let [query] = runner.recorded.as_slice() else {
        panic!("expected exactly one invocation");
    };
    assert_eq!(query.argv, ["docker", "compose", "ls", "--format", "json"]);
    assert_eq!(query.cwd, root.to_string_lossy());
    assert_eq!(query.stdout, StdoutSink::Capture);
}

#[test]
fn ps_matches_compose_names_to_directories_and_excludes_non_repo_projects() {
    // Compose reports the *directory* name (apps/music), the row
    // shows the project.json name (bonob); compose projects that match no
    // discovered directory are not this repository's business. Columns
    // align across rows; rows sort by project name.
    let root = common::fixture("listing");
    let repo = Repo::discover(&root).unwrap();
    let stdout = format!(
        r#"[{{"Name":"whoami","Status":"running(1)","ConfigFiles":"{root}/apps/whoami/docker-compose.yml"}},
            {{"Name":"music","Status":"running(2)","ConfigFiles":"{root}/apps/music/docker-compose.yml"}},
            {{"Name":"unrelated","Status":"running(5)","ConfigFiles":"/elsewhere/docker-compose.yml"}}]"#,
        root = root.display()
    );
    let mut runner = ComposeLsRunner::listing(&stdout);

    let (_, listing) = commands::ps(&repo, &BTreeMap::new(), &mut runner).unwrap();

    assert_eq!(
        listing,
        "bonob   running(2)  apps/music/docker-compose.yml\n\
         whoami  running(1)  apps/whoami/docker-compose.yml\n"
    );
}

#[test]
fn ps_prints_nothing_when_no_repository_project_is_running() {
    // Empty result, empty output, exit 0 — pipe-friendly.
    let root = common::fixture("listing");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = ComposeLsRunner::listing("[]");

    let (exit_code, listing) = commands::ps(&repo, &BTreeMap::new(), &mut runner).unwrap();

    assert_eq!((exit_code, listing.as_str()), (0, ""));
}

#[test]
fn ps_propagates_a_failed_compose_ls_exit_code() {
    // Docker's exit code propagates where one exists.
    let root = common::fixture("listing");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = FailingRunner {
        fail_at: 0,
        exit_code: 7,
        recorded: Vec::new(),
    };

    let (exit_code, listing) = commands::ps(&repo, &BTreeMap::new(), &mut runner).unwrap();

    assert_eq!((exit_code, listing.as_str()), (7, ""));
}

#[test]
fn ps_prepends_the_ci_flags_when_ci_is_set() {
    // CI set → `--ansi never --progress plain` on every compose
    // invocation, the ls query included.
    let root = common::fixture("listing");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = ComposeLsRunner::listing("[]");
    let env = BTreeMap::from([("CI".to_owned(), String::new())]);

    commands::ps(&repo, &env, &mut runner).unwrap();

    let [query] = runner.recorded.as_slice() else {
        panic!("expected exactly one invocation");
    };
    assert_eq!(
        query.argv,
        [
            "docker",
            "compose",
            "--ansi",
            "never",
            "--progress",
            "plain",
            "ls",
            "--format",
            "json"
        ]
    );
}

#[test]
fn dry_run_prints_the_worked_example_lines_verbatim_and_executes_nothing() {
    // --dry-run prints the docker invocations that would run — full argv,
    // in order — without executing anything. The expected lines are the
    // worked example chain, byte for byte.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut out = Vec::new();
    let mut runner = DryRunRunner::new(&mut out);

    let code = commands::up(
        &repo,
        &plain_env(),
        "bonob",
        &commands::UpOptions::default(),
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "docker compose --file apps/traefik/docker-compose.yml --file apps/traefik/docker-compose.local.yml up --yes --remove-orphans --detach\n\
         docker compose --file apps/navidrome/docker-compose.yml up --yes --remove-orphans --detach\n\
         docker compose --file apps/bonob/docker-compose.yml --file apps/bonob/docker-compose.local.yml up --yes --remove-orphans --detach\n"
    );
}

// ---------------------------------------------------------------------------
// `deps <project>` — query the dependency graph in both directions
// ---------------------------------------------------------------------------
//
// The `deps` fixture is a hub + diamond shaped after the docker-host reality:
//   networks (lib) ← traefik ← {navidrome, baikal, whoami}, bonob → {navidrome,
//   traefik}. bonob reaches traefik two ways (directly and via navidrome) and
//   networks only transitively, so direct ≠ transitive and dedup is exercised.

/// A `deps` options bundle spelled out at the call site for readability.
fn deps_options(direction: Direction, depth: Depth, json: bool) -> DepsOptions {
    DepsOptions {
        direction,
        depth,
        json,
    }
}

#[test]
fn deps_forward_direct_lists_the_projects_own_dependencies_in_list_style() {
    // Default direction/depth: the direct implicitDependencies, sorted, in the
    // same `name (a, b)` shape as `list` — mirroring the `list` mental model.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "bonob",
        &deps_options(Direction::Forward, Depth::Direct, false),
    )
    .unwrap();

    assert_eq!(out, "bonob (navidrome, traefik)\n");
}

#[test]
fn deps_forward_transitive_expands_to_the_full_closure() {
    // Transitive forward adds `networks`, which bonob depends on only through
    // traefik/navidrome — not a direct edge. The diamond dedupes traefik once.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "bonob",
        &deps_options(Direction::Forward, Depth::Transitive, false),
    )
    .unwrap();

    assert_eq!(out, "bonob (navidrome, networks, traefik)\n");
}

#[test]
fn deps_reverse_direct_lists_the_immediate_dependents() {
    // Reverse flips to dependents: the projects that directly list `networks`.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "networks",
        &deps_options(Direction::Reverse, Depth::Direct, false),
    )
    .unwrap();

    assert_eq!(out, "networks (navidrome, traefik)\n");
}

#[test]
fn deps_reverse_transitive_of_a_hub_fans_out_to_everything() {
    // The real-world hub shape: a `networks`/`traefik` change re-runs the world.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "networks",
        &deps_options(Direction::Reverse, Depth::Transitive, false),
    )
    .unwrap();

    assert_eq!(
        out,
        "networks (baikal, bonob, navidrome, traefik, whoami)\n"
    );
}

#[test]
fn deps_reverse_transitive_of_a_single_edge_dep_yields_one() {
    // The `navidrome`-like case: exactly one project (bonob) depends on it.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "navidrome",
        &deps_options(Direction::Reverse, Depth::Transitive, false),
    )
    .unwrap();

    assert_eq!(out, "navidrome (bonob)\n");
}

#[test]
fn deps_leaf_with_no_dependents_prints_the_bare_name() {
    // A leaf in the requested direction: empty result, success — the queried
    // name alone (list-style), distinguishable from an error.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "bonob",
        &deps_options(Direction::Reverse, Depth::Direct, false),
    )
    .unwrap();

    assert_eq!(out, "bonob\n");
}

#[test]
fn deps_json_matches_the_documented_contract() {
    // The stable machine-readable object CI parses: keys in a fixed order,
    // `deps` sorted and deduplicated, direction/transitive echoed.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "traefik",
        &deps_options(Direction::Reverse, Depth::Transitive, true),
    )
    .unwrap();

    assert_eq!(
        out,
        "{\"project\":\"traefik\",\"direction\":\"reverse\",\"transitive\":true,\
         \"deps\":[\"baikal\",\"bonob\",\"navidrome\",\"whoami\"]}\n"
    );
}

#[test]
fn deps_json_forward_direct_echoes_direction_and_transitive() {
    // The default direction/depth still fills the echoed fields correctly, and
    // a leaf's `deps` is an empty array (not null/absent).
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "networks",
        &deps_options(Direction::Forward, Depth::Direct, true),
    )
    .unwrap();

    assert_eq!(
        out,
        "{\"project\":\"networks\",\"direction\":\"forward\",\"transitive\":false,\"deps\":[]}\n"
    );
}

#[test]
fn deps_rejects_an_unknown_project_listing_the_valid_names() {
    // A typo fails loudly — non-zero, naming the culprit and the valid
    // set, like the lifecycle commands' guard.
    let root = common::fixture("deps");
    let repo = Repo::discover(&root).unwrap();

    let error = commands::deps(
        &repo,
        "ghost",
        &deps_options(Direction::Forward, Depth::Direct, false),
    )
    .unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("ghost"), "names the culprit: {message}");
    for valid in [
        "baikal",
        "bonob",
        "navidrome",
        "networks",
        "traefik",
        "whoami",
    ] {
        assert!(message.contains(valid), "lists {valid}: {message}");
    }
}

#[test]
fn deps_transitive_traversal_terminates_on_a_cycle() {
    // Cycle safety: the graph is expected acyclic, but the walk must not hang
    // if one exists. alpha → beta → gamma → alpha; alpha's transitive forward
    // deps are the other two, itself excluded.
    let root = common::fixture("cyclic");
    let repo = Repo::discover(&root).unwrap();

    let out = commands::deps(
        &repo,
        "alpha",
        &deps_options(Direction::Forward, Depth::Transitive, false),
    )
    .unwrap();

    assert_eq!(out, "alpha (beta, gamma)\n");
}

// ---------------------------------------------------------------------------
// `all <verb>` — bulk operations over the running set
// ---------------------------------------------------------------------------

/// A runner for `all <verb>` tests: it answers the captured `docker compose
/// ls` selection query with canned JSON, and lets specific *verb* invocations
/// (the non-captured ones) exit non-zero to exercise the failure policy.
struct BulkRunner {
    ls_json: String,
    /// (verb-index, exit code) — the Nth non-ls invocation exits with `code`.
    fail: Vec<(usize, i32)>,
    recorded: Vec<Invocation>,
    verb_seen: usize,
}

impl BulkRunner {
    fn listing(ls_json: &str) -> Self {
        Self {
            ls_json: ls_json.to_owned(),
            fail: Vec::new(),
            recorded: Vec::new(),
            verb_seen: 0,
        }
    }

    fn failing(ls_json: &str, at: usize, code: i32) -> Self {
        let mut runner = Self::listing(ls_json);
        runner.fail.push((at, code));
        runner
    }

    /// The verb invocations (everything but the ls selection query), in order.
    fn verbs(&self) -> Vec<Vec<String>> {
        self.recorded
            .iter()
            .filter(|inv| inv.stdout != StdoutSink::Capture)
            .map(|inv| inv.argv.clone())
            .collect()
    }
}

impl Runner for BulkRunner {
    fn run(&mut self, invocation: &Invocation) -> anyhow::Result<Outcome> {
        self.recorded.push(invocation.clone());
        if invocation.stdout == StdoutSink::Capture {
            return Ok(Outcome {
                exit_code: 0,
                stdout: Some(self.ls_json.clone()),
            });
        }
        let index = self.verb_seen;
        self.verb_seen += 1;
        let exit_code = self
            .fail
            .iter()
            .find(|(n, _)| *n == index)
            .map_or(0, |(_, code)| *code);
        Ok(Outcome {
            exit_code,
            stdout: None,
        })
    }
}

/// Build a `docker compose ls --format json` payload: one entry per
/// `(directory-name, repo-relative compose file)` pair, `ConfigFiles` absolute.
fn compose_ls_json(root: &std::path::Path, running: &[(&str, &str)]) -> String {
    let entries: Vec<String> = running
        .iter()
        .map(|(name, file)| {
            format!(
                r#"{{"Name":"{name}","Status":"running(1)","ConfigFiles":"{}/{file}"}}"#,
                root.display()
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// A `docker compose <tail…>` argv as owned strings.
fn compose(tail: &[&str]) -> Vec<String> {
    let mut command = vec!["docker".to_string(), "compose".to_string()];
    command.extend(tail.iter().map(ToString::to_string));
    command
}

#[test]
fn all_pull_fans_out_one_pull_per_running_project_in_topological_order() {
    // `tug all pull` pulls every RUNNING project's images, in
    // topological order (dependencies first). chain running set
    // {bonob, navidrome, traefik} orders as traefik, navidrome, bonob.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let ls = compose_ls_json(
        &root,
        &[
            ("bonob", "apps/bonob/docker-compose.yml"),
            ("navidrome", "apps/navidrome/docker-compose.yml"),
            ("traefik", "apps/traefik/docker-compose.yml"),
        ],
    );
    let mut runner = BulkRunner::listing(&ls);

    let (code, summary) =
        commands::all(&repo, &plain_env(), commands::BulkVerb::Pull, &mut runner).unwrap();

    assert_eq!(code, 0);
    assert_eq!(summary, "all pull: 3 ok\n");
    assert_eq!(
        runner.verbs(),
        vec![
            compose(&[
                "--file",
                "apps/traefik/docker-compose.yml",
                "--file",
                "apps/traefik/docker-compose.local.yml",
                "pull",
            ]),
            compose(&["--file", "apps/navidrome/docker-compose.yml", "pull"]),
            compose(&[
                "--file",
                "apps/bonob/docker-compose.yml",
                "--file",
                "apps/bonob/docker-compose.local.yml",
                "pull",
            ]),
        ]
    );
    // Exactly one selection query, and it comes first (captured stdout).
    assert_eq!(
        runner.recorded[0].argv,
        ["docker", "compose", "ls", "--format", "json"]
    );
    assert_eq!(runner.recorded[0].stdout, StdoutSink::Capture);
}

#[test]
fn all_down_tears_down_the_running_set_in_reverse_topological_order() {
    // `tug all down` takes down every running project, dependents
    // before dependencies (the mirror of up) — bonob, navidrome, traefik.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let ls = compose_ls_json(
        &root,
        &[
            ("bonob", "apps/bonob/docker-compose.yml"),
            ("navidrome", "apps/navidrome/docker-compose.yml"),
            ("traefik", "apps/traefik/docker-compose.yml"),
        ],
    );
    let mut runner = BulkRunner::listing(&ls);

    let (code, summary) =
        commands::all(&repo, &plain_env(), commands::BulkVerb::Down, &mut runner).unwrap();

    assert_eq!((code, summary.as_str()), (0, "all down: 3 ok\n"));
    assert_eq!(
        runner.verbs(),
        vec![
            compose(&[
                "--file",
                "apps/bonob/docker-compose.yml",
                "--file",
                "apps/bonob/docker-compose.local.yml",
                "down",
                "--remove-orphans",
            ]),
            compose(&[
                "--file",
                "apps/navidrome/docker-compose.yml",
                "down",
                "--remove-orphans",
            ]),
            compose(&[
                "--file",
                "apps/traefik/docker-compose.yml",
                "--file",
                "apps/traefik/docker-compose.local.yml",
                "down",
                "--remove-orphans",
            ]),
        ]
    );
}

#[test]
fn all_restart_bounces_the_running_set_in_topological_order_detached() {
    // `tug all restart` bounces every running project — a
    // non-cascading down leg then up leg each, dependencies first
    // (traefik, navidrome, bonob), always detached (no per-project attach
    // makes sense across a batch).
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let ls = compose_ls_json(
        &root,
        &[
            ("bonob", "apps/bonob/docker-compose.yml"),
            ("navidrome", "apps/navidrome/docker-compose.yml"),
            ("traefik", "apps/traefik/docker-compose.yml"),
        ],
    );
    let mut runner = BulkRunner::listing(&ls);

    let (code, summary) = commands::all(
        &repo,
        &plain_env(),
        commands::BulkVerb::Restart,
        &mut runner,
    )
    .unwrap();

    assert_eq!((code, summary.as_str()), (0, "all restart: 3 ok\n"));
    let traefik = [
        "apps/traefik/docker-compose.yml",
        "apps/traefik/docker-compose.local.yml",
    ];
    let navidrome = ["apps/navidrome/docker-compose.yml"];
    let bonob = [
        "apps/bonob/docker-compose.yml",
        "apps/bonob/docker-compose.local.yml",
    ];
    let down = |files: &[&str]| {
        let mut a: Vec<&str> = Vec::new();
        for f in files {
            a.push("--file");
            a.push(f);
        }
        a.extend(["down", "--remove-orphans"]);
        compose(&a)
    };
    let up = |files: &[&str]| {
        let mut a: Vec<&str> = Vec::new();
        for f in files {
            a.push("--file");
            a.push(f);
        }
        a.extend(["up", "--yes", "--remove-orphans", "--detach"]);
        compose(&a)
    };
    assert_eq!(
        runner.verbs(),
        vec![
            down(&traefik),
            up(&traefik),
            down(&navidrome),
            up(&navidrome),
            down(&bonob),
            up(&bonob),
        ]
    );
}

#[test]
fn all_continues_past_a_failing_project_and_reports_the_first_failure() {
    // Best-effort batch — one project's non-zero exit does not
    // strand the rest. Every project is still attempted; the summary names
    // the failures; the exit code is the first non-zero one seen.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let ls = compose_ls_json(
        &root,
        &[
            ("bonob", "apps/bonob/docker-compose.yml"),
            ("navidrome", "apps/navidrome/docker-compose.yml"),
            ("traefik", "apps/traefik/docker-compose.yml"),
        ],
    );
    // pull order is traefik(0), navidrome(1), bonob(2); fail navidrome.
    let mut runner = BulkRunner::failing(&ls, 1, 3);

    let (code, summary) =
        commands::all(&repo, &plain_env(), commands::BulkVerb::Pull, &mut runner).unwrap();

    assert_eq!(
        (code, summary.as_str()),
        (3, "all pull: 2 ok, 1 failed (navidrome)\n")
    );
    // bonob was still pulled after navidrome failed — three verb invocations.
    assert_eq!(runner.verbs().len(), 3);
}

#[test]
fn all_reports_no_running_projects_without_issuing_any_verb() {
    // Nothing running → a friendly line, exit 0, and not a single
    // verb invocation beyond the one selection query.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = BulkRunner::listing("[]");

    let (code, summary) =
        commands::all(&repo, &plain_env(), commands::BulkVerb::Down, &mut runner).unwrap();

    assert_eq!((code, summary.as_str()), (0, "no running projects\n"));
    assert_eq!(runner.verbs(), Vec::<Vec<String>>::new());
    assert_eq!(runner.recorded.len(), 1, "only the selection query ran");
}

#[test]
fn all_propagates_a_failed_selection_query_and_issues_no_verbs() {
    // If the running-set query itself fails, there is no set to act
    // on — propagate docker's exit code and touch nothing.
    let root = common::fixture("chain");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = FailingRunner {
        fail_at: 0,
        exit_code: 7,
        recorded: Vec::new(),
    };

    let (code, summary) = commands::all(
        &repo,
        &plain_env(),
        commands::BulkVerb::Restart,
        &mut runner,
    )
    .unwrap();

    assert_eq!((code, summary.as_str()), (7, ""));
    assert_eq!(runner.recorded.len(), 1, "only the failed query ran");
}
