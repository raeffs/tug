# Changelog

## [Unreleased]

### Fixed

- `ENVIRONMENT_NAME` in the repo-root `.env.local` or `.env` selects the
  compose overlay again, as it did under the Nx plugin. The process
  environment still wins over both files. (#223)

## [1.1.0] — 2026-08-01

### Added

- `up --no-deps` starts only the named project, skipping the dependency
  cascade.
- An optional `.tugignore` at the repo root names directories discovery never
  descends into.

### Changed

- `ps` no longer marks rows with `(archived)`.

### Removed

- `list --all`, since the concept of archived apps is gone.

## [1.0.0] — 2026-07-31

First public release.

[1.1.0]: https://github.com/raeffs/tug/releases/tag/v1.1.0
[1.0.0]: https://github.com/raeffs/tug/releases/tag/v1.0.0
