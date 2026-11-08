//! Environment model: env name, compose layering, dotenv merge,
//! persistence primitives.

mod common;

use std::collections::BTreeMap;

use std::path::{Path, PathBuf};

use tug::env::{append_env_local, compose_files, environment_name, merged_env, write_secret};

fn process_env(vars: &[(&str, &str)]) -> BTreeMap<String, String> {
    vars.iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn ci_set_resolves_to_ci() {
    let root = common::fixture("basic");
    let env = process_env(&[("CI", "true")]);
    assert_eq!(environment_name(&env, &root).unwrap(), "ci");
}

#[test]
fn environment_name_variable_resolves_to_its_value() {
    let root = common::fixture("basic");
    let env = process_env(&[("ENVIRONMENT_NAME", "staging")]);
    assert_eq!(environment_name(&env, &root).unwrap(), "staging");
}

#[test]
fn ci_set_to_empty_still_resolves_to_ci() {
    let root = common::fixture("basic");
    let env = process_env(&[("CI", "")]);
    assert_eq!(environment_name(&env, &root).unwrap(), "ci");
}

#[test]
fn ci_wins_over_environment_name_variable() {
    let root = common::fixture("basic");
    let env = process_env(&[("CI", "1"), ("ENVIRONMENT_NAME", "staging")]);
    assert_eq!(environment_name(&env, &root).unwrap(), "ci");
}

#[test]
fn neither_variable_set_defaults_to_local() {
    let root = common::fixture("basic");
    let env = process_env(&[]);
    assert_eq!(environment_name(&env, &root).unwrap(), "local");
}

#[test]
fn environment_name_comes_from_the_root_env_local() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "ENVIRONMENT_NAME=large\n").unwrap();
    assert_eq!(environment_name(&process_env(&[]), &root).unwrap(), "large");
}

#[test]
fn environment_name_comes_from_the_root_env_when_env_local_lacks_it() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "OTHER=1\n").unwrap();
    std::fs::write(root.join(".env"), "ENVIRONMENT_NAME=large\n").unwrap();
    assert_eq!(environment_name(&process_env(&[]), &root).unwrap(), "large");
}

#[test]
fn root_env_local_wins_over_root_env_for_the_name() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "ENVIRONMENT_NAME=large\n").unwrap();
    std::fs::write(root.join(".env"), "ENVIRONMENT_NAME=small\n").unwrap();
    assert_eq!(environment_name(&process_env(&[]), &root).unwrap(), "large");
}

#[test]
fn exported_environment_name_wins_over_the_root_dotenv_files() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "ENVIRONMENT_NAME=large\n").unwrap();
    let env = process_env(&[("ENVIRONMENT_NAME", "staging")]);
    assert_eq!(environment_name(&env, &root).unwrap(), "staging");
}

#[test]
fn environment_name_in_a_project_dotenv_is_ignored() {
    let root = common::fixture("basic");
    std::fs::write(
        root.join("apps/whoami/.env.local"),
        "ENVIRONMENT_NAME=small\n",
    )
    .unwrap();
    std::fs::write(root.join(".env"), "ENVIRONMENT_NAME=large\n").unwrap();
    assert_eq!(environment_name(&process_env(&[]), &root).unwrap(), "large");
}

#[test]
fn ci_in_a_root_dotenv_does_not_resolve_to_ci() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "CI=1\n").unwrap();
    assert_eq!(environment_name(&process_env(&[]), &root).unwrap(), "local");
}

#[test]
fn exported_ci_wins_over_environment_name_in_the_root_dotenv() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "ENVIRONMENT_NAME=large\n").unwrap();
    assert_eq!(
        environment_name(&process_env(&[("CI", "")]), &root).unwrap(),
        "ci"
    );
}

#[test]
fn empty_environment_name_in_the_root_dotenv_is_the_empty_string() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "ENVIRONMENT_NAME=\n").unwrap();
    assert_eq!(environment_name(&process_env(&[]), &root).unwrap(), "");
}

#[cfg(unix)]
#[test]
fn unreadable_root_env_local_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let root = common::fixture("basic");
    let path = root.join(".env.local");
    std::fs::write(&path, "ENVIRONMENT_NAME=large\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_to_string(&path).is_ok() {
        return; // running as root: the mode does not block reads, nothing to test
    }
    let err = environment_name(&process_env(&[]), &root).unwrap_err();
    assert!(err.to_string().contains("failed to read"), "{err:#}");
}

#[test]
fn compose_files_is_base_only_without_an_overlay() {
    let root = common::fixture("basic");
    let files = compose_files(&root, Path::new("apps/whoami"), "local");
    assert_eq!(files, [PathBuf::from("apps/whoami/docker-compose.yml")]);
}

#[test]
fn merged_env_loads_variables_from_the_root_dotenv() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env"), "GREETING=hello\n").unwrap();
    let merged = merged_env(&process_env(&[]), &root, Path::new("apps/whoami")).unwrap();
    assert_eq!(merged.get("GREETING").map(String::as_str), Some("hello"));
}

#[test]
fn merged_env_strips_single_quotes_around_values() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env"), "QUOTED='hello world'\nPLAIN=as-is\n").unwrap();
    let merged = merged_env(&process_env(&[]), &root, Path::new("apps/whoami")).unwrap();
    assert_eq!(
        merged.get("QUOTED").map(String::as_str),
        Some("hello world")
    );
    assert_eq!(merged.get("PLAIN").map(String::as_str), Some("as-is"));
}

#[test]
fn merged_env_ignores_comment_and_blank_lines() {
    let root = common::fixture("basic");
    std::fs::write(
        root.join(".env"),
        "# DOCUMENTED='template value'\n\nREAL=yes\n",
    )
    .unwrap();
    let merged = merged_env(&process_env(&[]), &root, Path::new("apps/whoami")).unwrap();
    assert_eq!(merged.get("REAL").map(String::as_str), Some("yes"));
    assert_eq!(merged.len(), 1, "comment line must not define a variable");
}

#[test]
fn merged_env_layers_never_override_a_higher_precedence_source() {
    let root = common::fixture("basic");
    let project = root.join("apps/whoami");
    std::fs::write(
        project.join(".env.local"),
        "FROM_PROCESS=project-local\nFROM_PROJECT_LOCAL=project-local\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".env"),
        "FROM_PROCESS=project-env\nFROM_PROJECT_LOCAL=project-env\nFROM_PROJECT_ENV=project-env\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".env.local"),
        "FROM_PROCESS=root-local\nFROM_PROJECT_LOCAL=root-local\nFROM_PROJECT_ENV=root-local\nFROM_ROOT_LOCAL=root-local\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".env"),
        "FROM_PROCESS=root-env\nFROM_PROJECT_LOCAL=root-env\nFROM_PROJECT_ENV=root-env\nFROM_ROOT_LOCAL=root-env\nFROM_ROOT_ENV=root-env\n",
    )
    .unwrap();

    let merged = merged_env(
        &process_env(&[("FROM_PROCESS", "process")]),
        &root,
        Path::new("apps/whoami"),
    )
    .unwrap();

    let value = |name: &str| merged.get(name).map(String::as_str);
    assert_eq!(value("FROM_PROCESS"), Some("process"));
    assert_eq!(value("FROM_PROJECT_LOCAL"), Some("project-local"));
    assert_eq!(value("FROM_PROJECT_ENV"), Some("project-env"));
    assert_eq!(value("FROM_ROOT_LOCAL"), Some("root-local"));
    assert_eq!(value("FROM_ROOT_ENV"), Some("root-env"));
}

#[test]
fn append_env_local_appends_without_touching_existing_lines() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "EXISTING='kept as-is'\n").unwrap();
    append_env_local(&root, "ADDED", "new value").unwrap();
    let content = std::fs::read_to_string(root.join(".env.local")).unwrap();
    assert_eq!(content, "EXISTING='kept as-is'\nADDED='new value'\n");
}

#[test]
fn append_env_local_creates_the_file_when_missing() {
    let root = common::fixture("basic");
    append_env_local(&root, "FIRST", "value").unwrap();
    let content = std::fs::read_to_string(root.join(".env.local")).unwrap();
    assert_eq!(content, "FIRST='value'\n");
}

#[test]
fn append_env_local_rejects_values_containing_a_single_quote() {
    let root = common::fixture("basic");
    std::fs::write(root.join(".env.local"), "EXISTING='kept'\n").unwrap();
    let error = append_env_local(&root, "BAD", "it's quoted").unwrap_err();
    assert!(error.to_string().contains("single quote"), "got: {error}");
    let content = std::fs::read_to_string(root.join(".env.local")).unwrap();
    assert_eq!(content, "EXISTING='kept'\n", "file must stay untouched");
}

#[test]
fn write_secret_stores_the_raw_value_with_restrictive_modes() {
    use std::os::unix::fs::PermissionsExt;
    let root = common::fixture("basic");
    write_secret(&root, "API_KEY", "s3cret").unwrap();

    let path = root.join(".secrets/API_KEY");
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "s3cret", "raw value, no trailing newline");

    let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600);
    let dir_mode = std::fs::metadata(root.join(".secrets"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
}

#[test]
fn compose_files_appends_the_overlay_matching_the_environment() {
    let root = common::fixture("basic");
    std::fs::write(root.join("apps/whoami/docker-compose.ci.yml"), "").unwrap();

    let files = compose_files(&root, Path::new("apps/whoami"), "ci");
    assert_eq!(
        files,
        [
            PathBuf::from("apps/whoami/docker-compose.yml"),
            PathBuf::from("apps/whoami/docker-compose.ci.yml"),
        ]
    );

    let files = compose_files(&root, Path::new("apps/whoami"), "local");
    assert_eq!(files, [PathBuf::from("apps/whoami/docker-compose.yml")]);
}
