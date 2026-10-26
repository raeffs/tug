//! Tests for `backup-volumes` and `restore-volumes`,
//! driven through the recording seam against fixture repos — no docker,
//! no `../docker-host`.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use tug::backup;
use tug::process::{Invocation, Outcome, Runner, StdinSource, StdoutSink};
use tug::repo::Repo;

/// A runner that records every invocation and, like the real docker child
/// writing an archive, deposits marker bytes into a file-wired stdout.
struct ArchivingRunner {
    /// Exit non-zero on the Nth invocation (0-based), like a failing child.
    fail_at: Option<usize>,
    recorded: Vec<Invocation>,
}

impl ArchivingRunner {
    fn new() -> Self {
        Self {
            fail_at: None,
            recorded: Vec::new(),
        }
    }

    fn failing_at(fail_at: usize) -> Self {
        Self {
            fail_at: Some(fail_at),
            recorded: Vec::new(),
        }
    }
}

impl Runner for ArchivingRunner {
    fn run(&mut self, invocation: &Invocation) -> anyhow::Result<Outcome> {
        self.recorded.push(invocation.clone());
        let failing = self.fail_at == Some(self.recorded.len() - 1);
        if let StdoutSink::File(path) = &invocation.stdout
            && !failing
        {
            std::fs::write(path, format!("archive of {}", invocation.argv.join(" ")))?;
        }
        let exit_code = if failing { 23 } else { 0 };
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

fn argv_of(recorded: &[Invocation]) -> Vec<Vec<String>> {
    recorded.iter().map(|i| i.argv.clone()).collect()
}

fn backup_argv(volume: &str) -> Vec<String> {
    [
        "docker",
        "run",
        "--rm",
        "--log-driver",
        "none",
        "-v",
        &format!("{volume}:/volume"),
        "loomchild/volume-backup",
        "backup",
    ]
    .map(String::from)
    .to_vec()
}

#[test]
fn backup_backs_up_own_volumes_only_into_the_default_target() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = ArchivingRunner::new();

    let code = backup::backup_volumes(&repo, &plain_env(), "app", None, &mut runner).unwrap();

    assert_eq!(code, 0);
    // The project's *own* volumes in declaration order — no cascade, so
    // base's `base-data` never appears.
    assert_eq!(
        argv_of(&runner.recorded),
        vec![backup_argv("app-data"), backup_argv("app-media")]
    );
    // Each child's stdout is wired natively to a file inside the target dir.
    let backups = root.join(".backups");
    for invocation in runner.recorded {
        assert_eq!(invocation.cwd, root.to_string_lossy());
        let StdoutSink::File(path) = &invocation.stdout else {
            panic!("stdout not wired to a file: {:?}", invocation.stdout);
        };
        assert_eq!(path.parent().unwrap(), backups);
    }
    // The archives land under their final names, nothing else left behind.
    let mut entries: Vec<String> = std::fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, ["app-data.tar.bz2", "app-media.tar.bz2"]);
    let content = std::fs::read_to_string(backups.join("app-data.tar.bz2")).unwrap();
    assert!(
        content.contains("app-data:/volume"),
        "wrong archive: {content}"
    );
}

#[test]
fn backup_resolves_a_relative_target_against_the_repo_root() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = ArchivingRunner::new();

    let code = backup::backup_volumes(
        &repo,
        &plain_env(),
        "app",
        Some(Path::new("saves/today")),
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 0);
    assert!(root.join("saves/today/app-data.tar.bz2").is_file());
}

#[test]
fn backup_uses_an_absolute_target_as_is() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let target = root.join("elsewhere");
    let mut runner = ArchivingRunner::new();

    let code =
        backup::backup_volumes(&repo, &plain_env(), "app", Some(&target), &mut runner).unwrap();

    assert_eq!(code, 0);
    assert!(target.join("app-media.tar.bz2").is_file());
    assert!(!root.join(".backups").exists());
}

#[test]
fn a_failed_backup_stops_and_never_clobbers_a_previous_archive() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let backups = root.join(".backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(backups.join("app-data.tar.bz2"), "previous archive").unwrap();
    let mut runner = ArchivingRunner::failing_at(0);

    let code = backup::backup_volumes(&repo, &plain_env(), "app", None, &mut runner).unwrap();

    // The child's exit code propagates and the run stops at the failure —
    // app-media was never attempted.
    assert_eq!(code, 23);
    assert_eq!(argv_of(&runner.recorded), vec![backup_argv("app-data")]);
    // The previous archive survives; no partial file is left behind.
    assert_eq!(
        std::fs::read_to_string(backups.join("app-data.tar.bz2")).unwrap(),
        "previous archive"
    );
    let entries: Vec<String> = std::fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, ["app-data.tar.bz2"]);
}

#[test]
fn restore_fails_before_spawning_when_an_archive_is_missing() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let backups = root.join(".backups");
    std::fs::create_dir_all(&backups).unwrap();
    // Only app-data's archive exists; app-media's is missing.
    std::fs::write(backups.join("app-data.tar.bz2"), "archive").unwrap();
    let mut runner = ArchivingRunner::new();

    let error =
        backup::restore_volumes(&repo, &plain_env(), "app", None, false, &mut runner).unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains("app-media.tar.bz2"),
        "message misses the missing archive: {message}"
    );
    // Nothing was spawned — not even app-data's restore.
    assert_eq!(runner.recorded, []);
}

fn restore_argv(volume: &str, force: bool) -> Vec<String> {
    let mut argv: Vec<String> = [
        "docker",
        "run",
        "--rm",
        "-i",
        "-v",
        &format!("{volume}:/volume"),
        "loomchild/volume-backup",
        "restore",
    ]
    .map(String::from)
    .to_vec();
    if force {
        argv.push("-f".to_owned());
    }
    argv
}

/// Seed the fixture's backup dir with an archive per own volume of `app`.
fn seed_archives(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for name in ["app-data", "app-media"] {
        std::fs::write(dir.join(format!("{name}.tar.bz2")), "archive").unwrap();
    }
}

#[test]
fn restore_feeds_each_own_volume_its_archive_over_stdin() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let backups = root.join(".backups");
    seed_archives(&backups);
    let mut runner = ArchivingRunner::new();

    let code =
        backup::restore_volumes(&repo, &plain_env(), "app", None, false, &mut runner).unwrap();

    assert_eq!(code, 0);
    // Own volumes only, declaration order, no cascade.
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            restore_argv("app-data", false),
            restore_argv("app-media", false)
        ]
    );
    // Each child's stdin is fed natively from its archive file.
    for (invocation, volume) in runner.recorded.iter().zip(["app-data", "app-media"]) {
        assert_eq!(invocation.cwd, root.to_string_lossy());
        assert_eq!(
            invocation.stdin,
            StdinSource::File(backups.join(format!("{volume}.tar.bz2")))
        );
    }
}

#[test]
fn restore_force_passes_dash_f_and_a_relative_source_resolves_against_the_root() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    seed_archives(&root.join("saves"));
    let mut runner = ArchivingRunner::new();

    let code = backup::restore_volumes(
        &repo,
        &plain_env(),
        "app",
        Some(Path::new("saves")),
        true,
        &mut runner,
    )
    .unwrap();

    assert_eq!(code, 0);
    assert_eq!(
        argv_of(&runner.recorded),
        vec![
            restore_argv("app-data", true),
            restore_argv("app-media", true)
        ]
    );
    assert_eq!(
        runner.recorded[0].stdin,
        StdinSource::File(root.join("saves/app-data.tar.bz2"))
    );
}

#[test]
fn a_failed_restore_stops_and_propagates_the_exit_code() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    seed_archives(&root.join(".backups"));
    let mut runner = ArchivingRunner::failing_at(0);

    let code =
        backup::restore_volumes(&repo, &plain_env(), "app", None, false, &mut runner).unwrap();

    assert_eq!(code, 23);
    assert_eq!(
        argv_of(&runner.recorded),
        vec![restore_argv("app-data", false)]
    );
}

#[test]
fn backup_rejects_an_unknown_project_listing_the_valid_names() {
    let root = common::fixture("backup");
    let repo = Repo::discover(&root).unwrap();
    let mut runner = ArchivingRunner::new();

    let error = backup::backup_volumes(&repo, &plain_env(), "nope", None, &mut runner).unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains("nope"),
        "message misses the name: {message}"
    );
    assert!(
        message.contains("app"),
        "message misses valid names: {message}"
    );
    assert!(
        message.contains("base"),
        "message misses valid names: {message}"
    );
    assert_eq!(runner.recorded, []);
}
