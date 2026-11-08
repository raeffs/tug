# tug — Domain Context

The vocabulary tug is built around. These are the words the code, the CLI, and
[`docs/reference.md`](docs/reference.md) all use for the same things; if you're
reading the source or writing about it, use them too.

## Repository & projects

- **Repository** — the git repository tug operates on. Every command resolves
  the **repo root** by walking up from the current directory to the first
  `.git`, and runs relative to it.

- **Project** — any directory containing a `project.json`. This is the unit
  every command names. A project has a unique repo-wide **name** (the CLI
  argument), declared in its `project.json`.

- **Compose project** — a project that also has a `docker-compose.yml` next to
  its `project.json`. Only these can be started, stopped, pulled, restarted, or
  have their logs streamed. Docker Compose derives the running container
  project name from the compose file's **directory name**, which is how tug
  matches running containers back to discovered projects.

- **Graph-only project** — a project *without* a `docker-compose.yml` (for
  example a `networks` project that only declares shared networks). It has no
  containers, so it participates in the dependency graph and in provisioning
  cascades but is silently skipped by lifecycle commands; naming one directly
  for `up`/`down`/`pull`/etc. is an error.

- **Ignored directory** — a directory named by the repo root's `.tugignore`
  that discovery never descends into. Its `project.json` files are never read,
  so the projects inside do not exist as far as every command is concerned:
  they appear in no listing, and naming one is an unknown project.
  Dot-directories are always ignored. Directory names (`apps/`, `libs/`,
  `archive/`) remain **convention, not contract** — discovery scans the whole
  repository apart from what is ignored.

## The dependency graph

- **Dependency edge** — an entry in a project's `implicitDependencies`. `A`
  depending on `B` means `B` must be provisioned and started before `A`.

- **Dependency** vs **dependent** — `B` is a *dependency* of `A` (A → B);
  conversely `A` is a *dependent* of `B`. `deps` walks edges **forward**
  (dependencies) by default and **reverse** (dependents) with `--reverse`.

- **Dependency cascade** — the transitive closure of a project's dependency
  edges. The provisioning commands always act on the whole cascade; `up` does
  by default, but can be asked to start the named project alone.

- **Topological order** — the deterministic ordering tug spawns work in:
  dependencies first, then dependents, ties broken by declaration order.
  Its mirror, **reverse-topological order** (dependents first), is used to tear
  things down gracefully (`all down`).

## Environment & configuration

- **Environment name** — resolved once per run: `ci` when `CI` is set in
  tug's process environment, else `ENVIRONMENT_NAME` from the process
  environment, then the repo-root `.env.local`, then the repo-root `.env`,
  else `local`. Project dotenv files never take part. It selects the compose
  overlay.

- **Compose overlay** — an optional `docker-compose.<env>.yml` layered on top
  of the base `docker-compose.yml` for the active environment (Docker Compose
  overlay semantics).

- **Merged environment** — the single environment tug hands to every spawned
  `docker` process, assembled from (highest precedence first) the CLI's own
  environment, then the project's and the repo root's `.env.local` / `.env`
  files. A value set by a higher layer is never overridden.

- **Persistence files** — where `configure` writes what it collects:
  `.env.local` at the repo root (non-secret values, append-only), and one file
  per secret under `.secrets/`.

## Provisioning

- **Variable spec** / **volume spec** / **network spec** — declarations under a
  project's `targets.create-*` in `project.json`, describing the environment
  variables, docker volumes, and docker networks the project needs.

- **The five variable types** — the special prompting behaviours a variable
  spec may request: `password`, `generated`, `boolean`, `docker-group`,
  `hostname` (plus the default plain-text prompt).

- **Cascade collection & dedupe** — how a `create-*` command gathers its work:
  the specs of every transitive dependency in topological order, then the named
  project's own, deduplicated by name (first occurrence wins).

## Execution

- **The seam** — the single boundary every external effect flows through. A
  command builds an **Invocation** (a `docker` call described as pure data) and
  hands it to a **Runner**. Swapping the Runner is all that separates a real
  run, a `--dry-run` (which prints invocations instead of spawning), and a test
  (which records them).

- **Running set** — the repository's currently-running compose projects, from a
  single `docker compose ls` query matched to discovered projects by directory
  name. `ps` reports it; `all <verb>` acts over it.

- **Best-effort batch** — the failure policy of `all <verb>`: unlike a single
  command, which stops at the first failure, a batch continues past a failing
  project, collects the failures, and prints a one-line summary.

---

## How it fits together

A run always starts by finding the repo root and **discovering** every project
under it, apart from anything inside an **ignored directory**. Those projects
and their `implicitDependencies` form the **dependency graph**. A lifecycle
command like `up` walks that graph to build the named project's **cascade** in
**topological order**, then, for each **compose
project** in it (skipping **graph-only** ones), assembles one `docker compose`
**Invocation** — with the right **compose overlay** and **merged environment**
— and hands it to a **Runner**. Provisioning commands walk the same cascade but
collect **specs** instead, creating the variables, volumes, and networks a
project needs before it can start. Everything is deterministic and sequential,
which is what makes `--dry-run` a trustworthy preview of exactly what a real run
would do.
