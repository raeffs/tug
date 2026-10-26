//! Tests for the repository model: repo root detection, project
//! discovery, `project.json` parsing, and the project/graph-only distinction.

mod common;

use std::path::Path;

use tug::repo::{NetworkSpec, Repo, VariableKind, VariableSpec, VolumeSpec, find_repo_root};

#[test]
fn repo_root_is_found_by_walking_up_to_the_git_marker() {
    let root = temp_dir("root-walk-up");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let nested = root.join("apps").join("whoami");
    std::fs::create_dir_all(&nested).unwrap();

    assert_eq!(find_repo_root(&nested).unwrap(), root);
}

#[test]
fn repo_root_detection_errors_outside_a_git_repository() {
    // temp_dir ancestors (/tmp, /) carry no .git, so the walk finds nothing.
    let dir = temp_dir("root-no-git");

    let message = find_repo_root(&dir).unwrap_err().to_string();

    assert!(
        message.contains("not inside a git repository"),
        "unexpected error: {message}"
    );
}

#[test]
fn discovery_finds_every_project_and_skips_excluded_directories() {
    // The basic fixture also contains a compose-only directory (libs/base), a
    // node_modules decoy its .tugignore names, and a dot-directory decoy
    // skipped unconditionally — none may surface.
    let root = common::fixture("basic");

    let repo = Repo::discover(&root.join("apps").join("whoami")).unwrap();

    assert_eq!(repo.root(), root);
    let names: Vec<&str> = repo.projects().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["networks", "traefik", "whoami"]);
}

#[test]
fn projects_inside_an_ignored_directory_are_not_discovered() {
    // The manifest under archive/ is well-formed and would otherwise be a
    // perfectly ordinary project; the .tugignore is the only reason it is
    // absent.
    let root = common::fixture("ignored");

    let repo = Repo::discover(&root).unwrap();

    let names: Vec<&str> = repo.projects().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["whoami"]);
}

#[test]
fn a_malformed_manifest_inside_an_ignored_directory_cannot_break_discovery() {
    // The ignored fixture's archive/broken/project.json is not valid JSON.
    // Never reading it is what makes this hold, so this locks the difference
    // between skipping a tree and filtering it out after the fact.
    let root = common::fixture("ignored");

    let repo = Repo::discover(&root).unwrap();

    assert!(repo.project("whoami").is_some());
}

#[test]
fn a_dependency_on_a_project_inside_an_ignored_directory_is_an_error() {
    // Ignored means nonexistent, and an edge to a nonexistent project is
    // fatal — deliberately, so a retired dependency surfaces at the manifest
    // rather than as a container that never started.
    let root = common::fixture("ignored-dependency");

    let message = format!("{:#}", Repo::discover(&root).unwrap_err());

    assert!(
        message.contains("app") && message.contains("retired"),
        "unexpected error: {message}"
    );
}

#[test]
fn dot_directories_are_the_only_skip_tug_makes_on_its_own() {
    // tug carries no knowledge of anyone's ecosystem: node_modules is an
    // ordinary directory unless the repository says otherwise in a
    // .tugignore. Dot-directories stay unconditional because that is tug
    // shielding itself from .git, .secrets, and .backups.
    let root = temp_dir("no-builtin-skips");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    for (dir, name) in [
        ("apps/whoami", "whoami"),
        ("node_modules/pkg", "vendored"),
        (".cache/pkg", "cached"),
    ] {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            format!("{{\"name\": \"{name}\"}}"),
        )
        .unwrap();
    }

    let repo = Repo::discover(&root).unwrap();

    let names: Vec<&str> = repo.projects().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["vendored", "whoami"]);
}

#[test]
fn an_unreadable_tugignore_is_an_error() {
    // Falling back to "ignore nothing" would silently widen every command to
    // trees the repository asked tug to stay out of, so an unreadable file
    // has to fail loudly. A directory stands in for one: it exists, and
    // reading it as a file cannot succeed.
    let root = temp_dir("unreadable-tugignore");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".tugignore")).unwrap();

    let message = format!("{:#}", Repo::discover(&root).unwrap_err());

    assert!(
        message.contains(".tugignore"),
        "unexpected error: {message}"
    );
}

// Long because it asserts the full project.json parsing surface, deliberately.
#[allow(clippy::too_many_lines)]
#[test]
fn project_json_parsing_reads_dependencies_and_the_three_target_specs() {
    // The traefik fixture carries leftover fields ($schema, executor) that the
    // parser ignores — parsing succeeding at all is part of the assertion.
    let root = common::fixture("basic");
    let repo = Repo::discover(&root).unwrap();

    let whoami = repo.project("whoami").unwrap();
    assert_eq!(whoami.dir, Path::new("apps/whoami"));
    assert_eq!(whoami.implicit_dependencies, Vec::<String>::new());
    assert_eq!(whoami.variables, []);
    assert_eq!(whoami.volumes, []);
    assert_eq!(whoami.networks, []);

    let traefik = repo.project("traefik").unwrap();
    assert_eq!(traefik.dir, Path::new("apps/traefik"));
    assert_eq!(traefik.implicit_dependencies, ["networks"]);
    assert_eq!(
        traefik.volumes,
        [
            VolumeSpec {
                name: "traefik-certificates".into(),
                user_id: Some(65001),
                image: None,
                mount_path: None,
            },
            // The plain-string form: a name and nothing else.
            VolumeSpec {
                name: "traefik-logs".into(),
                user_id: None,
                image: None,
                mount_path: None,
            },
        ]
    );
    // Descriptions are required (parse failure proves absence) but
    // their prose is informational — blank them out before comparing.
    let variables: Vec<VariableSpec> = traefik
        .variables
        .iter()
        .cloned()
        .map(|mut spec| {
            assert!(
                !spec.description.is_empty(),
                "{} lost its description",
                spec.name
            );
            spec.description = String::new();
            spec
        })
        .collect();
    assert_eq!(
        variables,
        [
            variable("DEFAULT_DOMAIN_SUFFIX", None, None, false, false),
            variable(
                "TRAEFIK_DOMAIN",
                Some("traefik.${DEFAULT_DOMAIN_SUFFIX}"),
                None,
                false,
                false,
            ),
            variable(
                "TRAEFIK_DOCKER_GID",
                None,
                Some(VariableKind::DockerGroup),
                false,
                false
            ),
            variable(
                "TRAEFIK_DASHBOARD_PASSWORD",
                None,
                Some(VariableKind::Password),
                false,
                true,
            ),
            variable(
                "TRAEFIK_API_KEY",
                None,
                Some(VariableKind::Generated),
                false,
                true
            ),
            variable(
                "TRAEFIK_ENABLE_METRICS",
                None,
                Some(VariableKind::Boolean),
                false,
                false
            ),
            variable(
                "TRAEFIK_HOST_NAME",
                None,
                Some(VariableKind::Hostname),
                false,
                false
            ),
            variable("TRAEFIK_EXTRA_ARGS", None, None, true, false),
        ]
    );

    let networks = repo.project("networks").unwrap();
    assert_eq!(
        networks.networks,
        [
            NetworkSpec {
                name: "default-frontend".into(),
                subnet: "172.20.0.0/24".into(),
                ip_range: "172.20.0.128/25".into(),
            },
            NetworkSpec {
                name: "default-backend".into(),
                subnet: "172.30.0.0/24".into(),
                ip_range: "172.30.0.128/25".into(),
            },
        ]
    );
}

#[test]
fn presence_of_a_compose_file_separates_projects_from_graph_only_nodes() {
    let root = common::fixture("basic");
    let repo = Repo::discover(&root).unwrap();

    assert!(repo.project("whoami").unwrap().has_compose_file);
    assert!(repo.project("traefik").unwrap().has_compose_file);
    assert!(!repo.project("networks").unwrap().has_compose_file);
}

#[test]
fn two_projects_sharing_a_name_are_an_error() {
    let root = common::fixture("duplicate-name");

    let message = format!("{:#}", Repo::discover(&root).unwrap_err());

    assert!(
        message.contains("duplicate project name") && message.contains("app"),
        "unexpected error: {message}"
    );
}

#[test]
fn a_dependency_on_an_unknown_project_is_an_error() {
    let root = common::fixture("unknown-dependency");

    let message = format!("{:#}", Repo::discover(&root).unwrap_err());

    assert!(
        message.contains("app") && message.contains("ghost"),
        "unexpected error: {message}"
    );
}

/// A [`VariableSpec`] with the fixture's description left unasserted — the
/// prompt copy is informational; name, default, type, and flags are contract.
fn variable(
    name: &str,
    default_value: Option<&str>,
    kind: Option<VariableKind>,
    optional: bool,
    is_secret: bool,
) -> VariableSpec {
    VariableSpec {
        name: name.into(),
        description: String::new(),
        default_value: default_value.map(Into::into),
        kind,
        optional,
        is_secret,
    }
}

/// A fresh scratch directory unique to this test process.
fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tug-repo-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
