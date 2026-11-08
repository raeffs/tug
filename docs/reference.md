# tug — Reference

The complete behavioural contract of the `tug` CLI: the repository model it
reads, how it resolves environments, how it spawns `docker`, and what every
command does. The [README](../README.md) is the introduction; this is the
detail. Terminology is defined in [CONTEXT.md](../CONTEXT.md) and used verbatim
here.

tug is orchestration on top of plain `docker compose`. It assembles and spawns
`docker` commands; it never becomes a runtime dependency of the services. Two
constraints frame everything it does:

1. **The compose files stay runnable with plain `docker compose`.** tug only
   arranges and runs `docker` for you.
2. **tug is a single self-contained executable.** Its only runtime dependency
   is the `docker` binary on `PATH`.

---

## 1. Repository model

### 1.1 Layout

tug expects a repository shaped roughly like this:

```
<repoRoot>/
  .env                    # optional, committed defaults
  .env.local              # gitignored; written by `configure` (append-only)
  .secrets/               # gitignored contents; one file per secret
  .backups/               # gitignored contents; default backup location
  .tugignore              # optional; directories discovery stays out of
  apps/<name>/            # one directory per running service
  archive/<name>/         # retired services, typically named in .tugignore
  libs/<name>/            # shared compose snippets and graph-only projects
```

The directory names `apps/`, `libs/`, `archive/` are **convention, not
contract**: discovery scans the whole repository and does not hardcode them.
`archive/` in particular means nothing to tug — a repository that wants its
retired services left alone says so in `.tugignore` (1.3), which works for any
directory and not just that one.

### 1.2 Projects and discovery

A **project** is any directory containing a `project.json`. A
`docker-compose.yml` next to it is optional:

- Projects **with** a `docker-compose.yml` are compose projects — they support
  `up` / `down` / `restart` / `pull` / `logs`.
- Projects **without** one are **graph-only**: they may declare
  variables/volumes/networks and appear as dependencies, they participate in
  `configure` cascades, the `up` cascade silently skips them, and naming them
  directly for a lifecycle command is an error.
- Directories with only a compose file and no `project.json` (for example
  `extends:` template snippets under `libs/`) are **not** projects.

Discovery recursively scans from the repo root for `project.json` files,
skipping any directory whose name starts with `.` — that covers `.git`,
`.secrets`, `.backups`, and tug's own footprint generally — plus anything the
repository's `.tugignore` names (1.3). Those two are the only exclusions; tug
carries no built-in knowledge of `node_modules/` or any other ecosystem's
conventions. Two projects with the same `name` are an error.

**Repo root detection:** tug works from any subdirectory — it walks up from the
current working directory to the first directory containing `.git`. If there is
none, it exits with an error.

### 1.3 `.tugignore`

An optional file at the repo root naming directories discovery never descends
into. It is read once per run, before any scanning.

```
# retired services, kept for reference only
archive

# vendored trees, wherever they turn up
node_modules

# one specific directory, not the name
apps/whoami/fixtures
```

- **A line without a slash is a name**, matched wherever that directory
  appears. One `node_modules` line covers every one in the tree.
- **A line with a slash is a path**, relative to the repo root, matched in
  exactly one place. A leading `/` anchors a root-level directory whose name
  also occurs deeper — `/archive` is the one at the top and nothing else.
- **A trailing slash is allowed and means nothing.** Only directories are ever
  matched, so `archive/` and `archive` are the same pattern.
- Blank lines are skipped, and a line whose first non-whitespace character is
  `#` is a comment. There are **no inline comments**, as in `.gitignore`: a
  `#` anywhere else is an ordinary character, so `archive # retired` names a
  directory called `archive # retired` and does not ignore `archive`.
  Surrounding whitespace is not part of a pattern.
- There are no wildcards, no `**`, and no `!` negation. There is no nested
  `.tugignore` — the one at the repo root is the whole list.

**Ignored means nonexistent, not hidden.** tug never reads the `project.json`
files inside an ignored directory, so the projects there do not exist for any
command: they appear in no listing, no dependency query, and no completion, and
naming one is an `unknown project` error rather than a refusal. Two
consequences follow that are easy to be surprised by:

- A project inside an ignored directory whose containers are **still running**
  is invisible to `ps` and is not acted on by `all <verb>`. Stop a service
  before retiring it.
- An active project that declares an `implicitDependencies` entry pointing into
  an ignored directory is a **dangling edge**, and dangling edges are fatal for
  the whole repository — every command fails, including `list`, until the
  manifest or the ignore file is corrected.

A missing `.tugignore` means nothing is ignored. A `.tugignore` that exists but
cannot be read is an error: silently falling back to "ignore nothing" would
widen every command to trees the repository asked tug to stay out of.

### 1.4 `project.json` schema

Only the fields below are read; all unknown fields are ignored, so the files
can carry whatever other tooling metadata they like. The files are plain JSON.

```jsonc
{
  // required; unique repo-wide; the CLI argument
  "name": "bonob",
  // optional; project names
  "implicitDependencies": ["networks", "traefik", "navidrome"],
  // optional; each target optional
  "targets": {
    "create-volumes":   { "options": { "volumes":   [ /* volume specs */ ] } },
    "create-variables": { "options": { "variables": [ /* variable specs */ ] } },
    "create-networks":  { "options": { "networks":  [ /* network specs */ ] } }
  }
}
```

A dependency name that resolves to no known project is an error.

### 1.5 Dependency graph

- Dependencies are **transitive**: the effective dependency set of a project is
  the transitive closure of its `implicitDependencies`.
- The graph is **cycle-checked**; a cycle is an error naming the projects
  involved.
- Ordering is **topological, dependencies first**, and deterministic across
  runs. Ties are broken by depth-first traversal in `implicitDependencies`
  declaration order, with each project visited once (first visit wins).

Example: `bonob` declares `["networks", "traefik", "navidrome"]`; `traefik` and
`navidrome` each declare `["networks"]`. The effective order for `up bonob` is
`networks`, `traefik`, `navidrome`, `bonob` — with `networks` skipped by `up`
because it is graph-only.

---

## 2. Environment model

### 2.1 Environment name

Resolved once per invocation:

1. If the `CI` variable is set in tug's own process environment (to any
   value, including empty): `ci`. `CI` in a dotenv file does not count.
2. Else if `ENVIRONMENT_NAME` is set, taking the first of: tug's process
   environment, `<repoRoot>/.env.local`, `<repoRoot>/.env`: its value.
3. Else: `local`.

Only the repo-root dotenv files take part. `ENVIRONMENT_NAME` in
`<projectDir>/.env.local` or `<projectDir>/.env` reaches the spawned
`docker` process (2.3) but never selects the overlay (2.2).

### 2.2 Compose file layering

For a compose project, every compose invocation uses:

1. `<projectDir>/docker-compose.yml`, and
2. `<projectDir>/docker-compose.<envName>.yml` **if that file exists**,
   appended as a second `--file` (Docker Compose overlay semantics).

### 2.3 Dotenv loading

Compose performs `${VAR}` substitution from the environment of the process that
invokes it, so tug constructs the environment of every spawned `docker` process
by merging, **highest precedence first**:

1. the real process environment of tug itself,
2. `<projectDir>/.env.local` (if present),
3. `<projectDir>/.env` (if present),
4. `<repoRoot>/.env.local`,
5. `<repoRoot>/.env`.

A variable already set by a higher-precedence source is never overridden. File
format: `NAME=value` lines; values may be wrapped in single quotes, which are
stripped; `#` starts a comment line; blank lines are ignored.

### 2.4 Persistence files

- **`<repoRoot>/.env.local`** — non-secret configured values. **Append-only**:
  `configure` appends `NAME='value'` for each newly set variable and never
  rewrites or deletes existing lines. A value containing a single quote is
  rejected rather than written unescaped.
- **`<repoRoot>/.secrets/<NAME>`** — one file per secret, raw value, no
  trailing newline, file mode `0600`, directory mode `0700`. Secrets are never
  written to `.env.local`.

---

## 3. Execution model

All external effects go through spawning the `docker` binary.

- **Compose invocations:** `docker compose --file <base> [--file <overlay>]
  <args…>` with **cwd = repo root** and `--file` paths **relative to the repo
  root**. This matters: compose derives the container project name and
  relative-path resolution from the directory of the first `--file`, so
  containers get project name = app directory name.
- When `CI` is set, `--ansi never --progress plain` is prepended to every
  compose invocation.
- The child's environment is the merged environment of 2.3 — its *entire*
  environment, replacing rather than extending tug's own, so spawns are
  deterministic.
- stdio is inherited by default, so interactive output and prompts stream
  through. Backup and restore instead wire the child's stdout/stdin to files
  natively (no shell, no `sh -c "… > file"`).
- Before each spawn, tug echoes a one-line rendering of the command to stderr
  (`+ docker compose …`).
- On SIGINT/SIGTERM, the signal is forwarded to the running child and tug exits
  with the child's exit code. A failed child's exit code is propagated as tug's
  own.
- Commands run **sequentially, never in parallel** — they mutate shared docker
  state (networks, volumes) and share `.env.local`.
- Read-only queries tug needs for its own decisions (`docker volume ls`,
  `docker network ls`, `docker compose ls`) use machine-readable output
  (`--format json`), never scraped from a human-readable table.

**`--dry-run`** (on `up`, `down`, `restart`, `pull`) prints the full `docker`
invocations that would run — full argv, in order — without executing anything.
Because ordering and argv are deterministic, dry-run output is a trustworthy
preview and parity-testing tool.

---

## 4. Command reference

```
tug up <project> [--no-deps] [--force] [--no-detach] [--wait] [--wait-timeout <SECS>] [--dry-run]
tug down <project> [--dry-run]
tug restart <project> [--no-detach] [--dry-run]
tug pull <project> [--dry-run]
tug logs <project> [--follow]
tug all down|restart|pull
tug list
tug deps <project> [--reverse] [--transitive] [--json]
tug ps
tug configure <project> [--use-defaults]
tug create-variables <project> [--use-defaults]
tug create-volumes <project> [--force]
tug create-networks <project>
tug backup-volumes <project> [--target-path <dir>]
tug restore-volumes <project> [--source-path <dir>] [--force]
tug new <name>
tug completions <bash|zsh|fish>
```

General rules:

- Every command takes exactly one project name, except `list`, `ps`, `new`, the
  `all` family, and `completions`.
- An unknown project name produces an error listing the valid names.
- Every command supports `--help`; a bare `tug` prints an overall usage
  summary.
- Exit code 0 on success; non-zero on failure, propagating docker's exit code
  where one exists.

### 4.1 Lifecycle commands

#### `up <project>`

Starts the project **and all its transitive dependencies**, dependencies first,
in topological order, sequentially — unless `--no-deps` asks for the named
project alone. Graph-only projects in the chain are skipped; naming a
graph-only project directly is an error. Per compose project in the chain:

```
docker compose --file <files…> up --yes --remove-orphans [--detach] [--force-recreate] [--wait --wait-timeout <secs>]
```

- `--detach` is passed **by default**; `--no-detach` omits it from every leg
  (used when starting a single dependency-less project interactively).
- `--force` maps to compose's `--force-recreate`, applied to every leg.
- `--wait` appends `--wait --wait-timeout <secs>` to each leg, so every
  dependency reports healthy before the next one starts.
  `--wait-timeout` defaults to `120` and requires `--wait`. Because compose
  rejects `--wait` in attached mode, `--wait` and `--no-detach` are mutually
  exclusive and rejected up front.
- `--no-deps` skips the cascade entirely: only the named project is started,
  one invocation, composing with every flag above. Naming a graph-only project
  is still an error. tug does not check whether the skipped dependencies are
  running — like `restart`, dependencies that are down stay down, silently.

  **This is not compose's `--no-deps`.** The flag cuts *tug's* dependency
  graph — the `implicitDependencies` edges `tug deps <project>` reports — and
  is deliberately **not** forwarded to `docker compose`, so the project's own
  `depends_on` services still start normally. A compose project is tug's
  atomic unit; no tug command starts a subset of one.
- `--yes` auto-confirms compose's interactive prompts.

#### `down <project>`

```
docker compose --file <files…> down --remove-orphans
```

**No dependency cascade** — only the named project (you do not want
`down bonob` to also take down `traefik`). Graph-only projects cannot be named.

#### `restart <project>`

A strict bounce of the named project — **no dependency cascade in either
direction**, so every other project's state is exactly as before. Two
invocations, sequentially:

```
docker compose --file <files…> down --remove-orphans
docker compose --file <files…> up --yes --remove-orphans [--detach]
```

If the down leg exits non-zero, the up leg does not run and the exit code
propagates. `--no-detach` omits `--detach` from the up leg. There is no
`--force`: after the down leg the containers are gone, so `--force-recreate`
could never matter. Dependencies that are down stay down. Graph-only projects
cannot be named.

#### `pull <project>`

```
docker compose --file <files…> pull
```

No cascade. Graph-only projects cannot be named.

#### `logs <project> [--follow]`

```
docker compose --file <files…> logs [--follow]
```

No cascade — only the named project. `--follow` keeps streaming until
interrupted; on Ctrl-C the signal is forwarded to the child, its exit code
propagates, and the terminal returns to a prompt with no orphaned child. There
is no `--dry-run`: logs is a read-only passthrough with nothing to preview.

### 4.2 Bulk operations — `all <verb>`

Runs one lifecycle verb over **every running project** in one command. `<verb>`
is one of `down`, `restart`, `pull`. There is deliberately no `all up`: over
the running set it is a near no-op, and over every project it would start
deliberately-down services — the batching `up` needs is already a project's own
dependency cascade.

- **Selection** is exactly `ps`'s running set (4.6), so `all` and `ps` never
  disagree.
- **Order** is direction-aware: `all down` tears down dependents first
  (reverse-topological); `all restart` / `all pull` go dependencies first
  (topological).
- **Per-project op** is the ordinary `down`/`restart`/`pull` on that project,
  identical to naming it directly.
- **Failure policy** is **best-effort**: a project whose op exits non-zero is
  recorded and the batch continues; a one-line summary is printed at the end
  (e.g. `all pull: 2 ok, 1 failed (navidrome)`; an all-clear reads
  `all pull: 3 ok`). tug's exit code is the first non-zero child exit seen, else
  0. A genuine spawn failure still aborts. The summary and the per-spawn `+ …`
  echoes share stderr; stdout stays clean.
- **No flags.** An empty running set prints `no running projects` and exits 0.

### 4.3 Inspection

#### `list`

Prints every discovered project with its direct `implicitDependencies`, sorted
by name, one line each: `name (dep, dep)`. No flags: what counts as discovered
is settled by `.tugignore` (1.3) before `list` runs, so there is nothing for it
to reveal or hide.

#### `deps <project> [--reverse] [--transitive] [--json]`

Reports a project's dependency edges. By default it reports the project's
**dependencies** (its `implicitDependencies`); `--reverse` reports its
**dependents** (what depends on it). By default only **direct** edges are
shown; `--transitive` expands to the full closure in the chosen direction.
`--reverse` and `--transitive` compose.

The queried project is excluded from its own result; results are deduplicated
and sorted lexicographically. A leaf yields an empty result and exit 0; an
unknown project is an error listing the valid names.

By default the output is a human `list`-style line. `--json` emits a stable
object for machine consumers — field order is part of the contract:

```json
{"project":"navidrome","direction":"reverse","transitive":true,"deps":["bonob"]}
```

`deps` carries both directions; the meaning of `deps` is fixed by the echoed
`direction` and `transitive` fields, so a consumer never has to scrape text.

#### `ps`

Lists the repository's **running** projects, from one `docker compose ls
--format json` query: one line per match — project name, compose's status
string verbatim (e.g. `running(2)`), and the loaded config files rendered
relative to the repo root, columns aligned, sorted by project name.

Compose derives a project's name from the directory of its first `--file`, so
entries are matched to discovered projects by **directory name** and displayed
under the `project.json` name. Compose projects matching no discovered project
are not listed — `ps` reports on this repository only, and a project inside an
ignored directory is not a discovered project even while its containers run
(1.3). No running projects → empty output.

### 4.4 Provisioning

`configure` is the primary onboarding command; the three `create-*` commands
underneath it can also be run individually. Each `create-*` command works over
the named project's **cascade**: it collects the specs of all transitive
dependencies first (in topological order), then the named project's own,
deduplicated by name (first occurrence wins), and processes the result in order
— stopping at the first failure.

#### `configure <project> [--use-defaults]`

Runs, in order, stopping at the first failure:

1. `create-variables <project>` (passing `--use-defaults` through),
2. `create-volumes <project>`,
3. `create-networks <project>`.

So `configure bonob` provisions variables, volumes, and networks for the whole
`networks` → `traefik` → `navidrome` → `bonob` cascade in one go. CI uses it
with `--use-defaults`.

#### `create-variables <project> [--use-defaults]`

A **variable spec**:

```jsonc
{
  "name": "TRAEFIK_DOCKER_GID",        // required
  "description": "GID of the …",       // required; shown as the prompt text
  "optional": true,                    // default false; optional = empty answer allowed
  "defaultValue": "traefik.${DEFAULT_DOMAIN_SUFFIX}",
                                       // optional; ${VAR} references are expanded against
                                       // the loaded environment before prompting; an
                                       // unset reference expands to the empty string
  "type": "password",                  // optional: password | generated | boolean
                                       //           | docker-group | hostname
  "isSecret": true                     // default false
}
```

Per variable, in list order:

1. **Skip if already set.** Non-secrets: the name is in the merged environment
   or was persisted earlier in the same run. Secrets: the file
   `.secrets/<NAME>` exists.
2. Otherwise obtain a value by type:
   - *no `type`* — text prompt; the suggested value is the expanded
     `defaultValue`; an empty answer accepts the suggestion, or is re-prompted
     when there is no suggestion, unless `optional`.
   - `password` — text prompt labelled "(leave empty to generate)"; an empty
     answer generates a random secret.
   - `generated` — no prompt ever; always generates a random secret.
   - `boolean` — yes/no prompt; persists `true`/`false`.
   - `docker-group` — text prompt suggesting the GID of the local `docker`
     group (read natively from `/etc/group`).
   - `hostname` — text prompt suggesting the machine hostname (OS API).
3. **`--use-defaults`** (CI mode): never prompt; take the computed default —
   generate for `password`/`generated`, the GID for `docker-group`, the
   hostname for `hostname`, the expanded `defaultValue` otherwise — falling back
   to an existing environment value, else the empty string. Persisting an empty
   value for a required variable is intentional (CI relies on it) but logs a
   warning.
4. **Persist.** Non-secret → append to root `.env.local`, and make it visible to
   later variables in the same run. Secret → write to `.secrets/<NAME>`.

Prompting is plain line-based terminal I/O. **Random secrets** are 64 characters
from `[A-Za-z0-9]`, drawn from the OS CSPRNG.

#### `create-volumes <project> [--force]`

A **volume spec** is either a plain string (name only) or:

```jsonc
{
  "name": "traefik-certificates",  // required
  "userId": 65001,                  // optional; chown the volume after creation
  "image": "busybox",               // optional, default "busybox" (used for the chown)
  "mountPath": "/volume"            // optional, default "/volume"
}
```

Existing local volumes are listed once (machine-readable). For each spec, in
order, stopping at the first failure:

- If it exists and `--force` is not set: skip.
- If `--force` is set and it exists: `docker volume rm <name>`, then recreate.
- Create: `docker volume create <name>`.
- If `userId` is set, chown after creation:

  ```
  docker run --rm -v <name>:<mountPath> --entrypoint /bin/sh <image> \
    -c "chown -R <uid>:<uid> <mountPath>"
  ```

#### `create-networks <project>`

A **network spec**:

```jsonc
{ "name": "default-frontend", "subnet": "172.20.0.0/24", "ipRange": "172.20.0.128/25" }
```

Existing networks are listed once; each missing network is created:

```
docker network create --subnet <subnet> --ip-range <ipRange> <name>
```

### 4.5 Backup & restore

Both act on the project's **own** `create-volumes` names only — **no cascade**.

#### `backup-volumes <project> [--target-path <dir>]`

Default `--target-path` is `.backups`; a relative path resolves against the
repo root, an absolute path is used as-is. Per volume:

```
docker run --rm --log-driver none -v <name>:/volume loomchild/volume-backup backup
```

with the child's stdout wired natively to `<target>/<name>.tar.bz2`. Each
archive is written to a `.partial` file and renamed on success, so a failed
backup never clobbers a previous archive.

#### `restore-volumes <project> [--source-path <dir>] [--force]`

Same volume list, no cascade, same default/resolution for `--source-path`.
Before spawning anything, **every** required `<source>/<name>.tar.bz2` must
exist, or tug fails with a clear error (a half-restored project is worse than a
clear refusal). Per volume:

```
docker run --rm -i -v <name>:/volume loomchild/volume-backup restore [-f]
```

with the child's stdin fed natively from the archive. `--force` passes `-f`.

### 4.6 Scaffolding — `new <name>`

Scaffolds `apps/<kebab>/` with four files rendered from embedded templates. The
input name is normalized into three casings, splitting on `-`, `_`, whitespace,
and lower→upper camelCase transitions:

- `kebab` — lower-case, `-`-separated (`my-app`),
- `CONSTANT` — upper-case, `_`-separated (`MY_APP`),
- `Class` — capitalized and concatenated (`MyApp`).

If a project named `<kebab>` already exists anywhere in the repository, `new`
fails without writing anything. The generated app declares
`implicitDependencies: ["networks", "traefik"]`, one `<kebab>-data` volume, and
one `<CONSTANT>_DOMAIN` variable defaulting to `<kebab>.${DEFAULT_DOMAIN_SUFFIX}`.

### 4.7 Shell completion — `completions <shell>`

Prints a shell completion script to stdout, ready to be sourced or installed.
`<shell>` is one of `bash`, `zsh`, `fish`; any other value is an error listing
the valid shells. This command touches nothing but stdout — no repo discovery,
no docker — so it works from any directory.

Completion is **dynamic**: it completes subcommand names, the chosen
subcommand's long flags, and — for project-taking commands — the repository's
discovered project names, resolved at TAB-time against the current directory's
repo. The `all` umbrella completes its three verbs; `new` takes a *new* name, so
it is not offered existing project names.

The emitted script is a small static forwarder that calls a hidden
`tug __complete <words…>` helper at TAB-time; all grammar logic lives in the
binary, read from its own CLI definition so it never drifts from the real
surface. `__complete` is fault-tolerant: run outside a repository it still
completes subcommands and flags (dropping only project names) and always exits 0
with nothing on stderr — a completer must never spill errors into the shell.

Install:

- **bash:** `source <(tug completions bash)` in `~/.bashrc`, or write it to
  `~/.local/share/bash-completion/completions/tug`.
- **zsh:** `tug completions zsh > ~/.zsh/completions/_tug` on `$fpath` (before
  `compinit`).
- **fish:** `tug completions fish > ~/.config/fish/completions/tug.fish`.

`tug` must be on `$PATH` when completion runs, since the script shells back out
to it.

---

## 5. Error handling

| Condition | Behaviour |
|---|---|
| Not inside a git repository | error, non-zero exit |
| Unknown project name | error listing valid project names |
| Duplicate project names in repo | error |
| Dependency on unknown project | error |
| Dependency cycle | error naming the cycle |
| Lifecycle command on a graph-only project | error |
| docker child exits non-zero | stop, propagate the exit code |
| Missing backup archive on restore | error before any docker invocation |
| `new` name collision | error before writing any file |
| Unknown shell to `completions` | error listing `bash`/`zsh`/`fish` |

`all <verb>` is the one exception to "stop at the first failure": it is
best-effort across the running set (4.2).

---

## 6. Guarantees

- **Distribution.** A single self-contained executable; the only runtime
  dependency is the `docker` binary on `PATH`. No Node.js, npm, or other
  toolchain.
- **Determinism.** Given the same repo state and environment, the ordering and
  argv of every spawned command are identical across runs. This is what makes
  `--dry-run` a usable parity and regression tool.
- **Config compatibility.** Existing `project.json` files are read as-is;
  unknown fields are ignored. tug invents no new config format.
- **Native, not shelled out.** dotenv parsing, secret generation, hostname and
  docker-group lookups, and stdout/stdin redirection are all done natively — tug
  shells out only to `docker`.
