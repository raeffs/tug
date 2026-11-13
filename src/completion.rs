//! Shell completion: the grammar-walking completer behind the
//! hidden `tug __complete` subcommand, and the `tug completions <shell>`
//! wrapper-script generator.
//!
//! The engine is deliberately clap-free: the binary introspects its own
//! `clap` command tree once (drift-free) into a [`CommandGrammar`] tree and
//! hands it here, so the position/prefix logic can be unit-tested against
//! fixture repos without building a parser. Repo-discovered project names
//! come from [`Repo`].

use crate::repo::Repo;

/// One node of the CLI grammar the completer walks: a subcommand, its long
/// flags, whether it consumes an existing project name, and any nested
/// subcommands (only `all` has these).
#[derive(Debug, Clone)]
pub struct CommandGrammar {
    /// The subcommand name, e.g. `up` or (nested) `down`.
    pub name: String,
    /// True when a positional existing-project name is expected (the derive
    /// arg is named `project`); drives project-name completion.
    pub takes_project: bool,
    /// The long flags, each already `--`-prefixed, e.g. `--force`.
    pub flags: Vec<String>,
    /// Nested subcommands; only the `all` umbrella populates this.
    pub subcommands: Vec<CommandGrammar>,
}

/// The candidates for the current word given the words typed so far.
///
/// `words` are the tokens after `tug`, the last being the (possibly empty)
/// word under the cursor; earlier tokens are complete. `repo` is `None` when
/// completion fires outside a repository — subcommands and flags still
/// complete, only project names fall away. Returns the matching candidates,
/// sorted and prefix-filtered by that last word.
#[must_use]
pub fn candidates(grammar: &[CommandGrammar], repo: Option<&Repo>, words: &[&str]) -> Vec<String> {
    let Some((current, prior)) = words.split_last() else {
        return Vec::new();
    };

    let mut pool: Vec<String> = Vec::new();
    match prior.split_first() {
        // Completing the subcommand itself.
        None => pool.extend(grammar.iter().map(|command| command.name.clone())),
        // Completing an argument of a chosen subcommand.
        Some((head, rest)) => {
            if let Some(command) = grammar.iter().find(|command| &command.name == head) {
                pool.extend(argument_candidates(command, repo, rest, current));
            }
        }
    }

    finish(pool, current)
}

/// The candidates for an argument of `command`, given the already-complete
/// tokens `rest` that follow it and the word `current` under the cursor.
///
/// An umbrella (`all`) delegates to its nested verb; a leaf offers its flags
/// when `current` is dash-prefixed, otherwise project names while its single
/// positional slot is still open.
fn argument_candidates(
    command: &CommandGrammar,
    repo: Option<&Repo>,
    rest: &[&str],
    current: &str,
) -> Vec<String> {
    if !command.subcommands.is_empty() {
        return match rest.split_first() {
            None => command.subcommands.iter().map(|c| c.name.clone()).collect(),
            Some((verb, tail)) => command
                .subcommands
                .iter()
                .find(|c| &c.name == verb)
                .map_or_default(|nested| argument_candidates(nested, repo, tail, current)),
        };
    }

    if current.starts_with('-') {
        return command.flags.clone();
    }
    if command.takes_project && !rest.iter().any(|token| !token.starts_with('-')) {
        return repo
            .into_iter()
            .flat_map(Repo::projects)
            .map(|project| project.name.clone())
            .collect();
    }
    Vec::new()
}

/// Prefix-filter by the current word and sort, the last step every branch
/// shares. Shells re-filter, but filtering here keeps zsh/fish honest.
fn finish(mut pool: Vec<String>, current: &str) -> Vec<String> {
    pool.retain(|candidate| candidate.starts_with(current));
    pool.sort();
    pool.dedup();
    pool
}

/// The three shells `tug completions` ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

/// The completion wrapper script for `shell`: a small, static forwarder that
/// hands the words on the line to `tug __complete` at TAB-time.
#[must_use]
pub fn script(shell: Shell) -> String {
    // Each script is a dumb forwarder: it hands the words on the line, up to
    // and including the (possibly empty) word under the cursor, to
    // `tug __complete` and feeds the newline-separated reply back to the
    // shell. All grammar logic stays in Rust; the scripts never change as
    // the CLI grows.
    match shell {
        Shell::Bash => BASH.to_owned(),
        Shell::Zsh => ZSH.to_owned(),
        Shell::Fish => FISH.to_owned(),
    }
}

const BASH: &str = r#"_tug() {
    local IFS=$'\n'
    # ${COMP_WORDS[@]:1:COMP_CWORD} is the words after "tug" up to and
    # including the current one (empty when the cursor sits after a space).
    COMPREPLY=($(tug __complete "${COMP_WORDS[@]:1:COMP_CWORD}"))
}
complete -F _tug tug
"#;

const ZSH: &str = r#"#compdef tug
_tug() {
    local -a candidates
    # words[2,CURRENT] is the words after "tug" up to and including the
    # current one; (@) keeps the trailing empty word as its own token.
    candidates=(${(f)"$(tug __complete "${(@)words[2,CURRENT]}")"})
    compadd -a -- candidates
}
_tug "$@"
"#;

const FISH: &str = r#"function __tug_complete
    # -opc: the tokens before the cursor, minus the partial current token,
    # which -ct returns separately; [2..-1] drops "tug".
    set -l tokens (commandline -opc)
    set -l current (commandline -ct)
    tug __complete $tokens[2..-1] $current
end
complete -c tug -f -a '(__tug_complete)'
"#;
