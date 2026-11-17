# CI coverage and build cost

The [CI workflow](../.github/workflows/ci.yml) runs on branch pushes and pull
requests. Tag pushes do not repeat CI: check that the tagged commit passed on
its branch before releasing. This workflow does not publish releases or packages.
There are deliberately no path filters: documentation, workflow, manifest, lock,
and vendored changes all receive the same checks.

## Coverage and scheduling

- Ubuntu: Rust **1.88.0 (MSRV)** and **stable**, both with default features disabled.
  Each builds, checks formatting, runs Clippy with warnings denied, tests, and
  builds workspace documentation with rustdoc warnings denied.
- macOS: stable Rust, default GUI features, the same five checks, plus
  `cargo test -p ruckus --features ui-tests --locked` for synthetic native-window
  coverage. `macos-26` pins the OS family, retaining the Apple Silicon architecture
  used by the observed successful `macos-latest` runs. It does not pin the runner
  image revision, SDK, or Xcode version. No Intel or Linux GUI coverage is implied.
- Formatting runs before compilation to fail cheap. No compile/test/doc check
  has been removed or combined across feature sets.
- New runs cancel superseded runs of the same workflow/event and branch or PR
  number. Separate PRs and branch refs cannot cancel each other. Push and PR runs
  remain independent (including when an internal feature branch has an open PR);
  eliminating those duplicates would require additional scheduling logic.
- Headless jobs have a 15-minute limit; the desktop job has a 30-minute limit,
  allowing substantial room over the observed cold baseline while bounding hangs.

Tests are hermetic; CI must not require public IRC networks or real credentials.
Synthetic native-window tests are not proof of a complete interactive desktop
workflow or full Lurk parity; see [PORTING.md](PORTING.md).

## Diagnosis: measured uncached baseline

Inspection via `gh run list`, `gh run view --json jobs`, and actual logs found:

| Run | Ref / commit | Result |
|---|---|---|
| [44008029946](https://github.com/exec/ruckus/actions/runs/44008029946) | `v0.1` / `2961e7e` | Success (latest at inspection) |
| [44006834850](https://github.com/exec/ruckus/actions/runs/44006834850) | `main` / `2961e7e` | Success |
| [44005976855](https://github.com/exec/ruckus/actions/runs/44005976855) | `main` / `85985cd` | Failed: stable Clippy denied deprecated `AtomicU64::fetch_update` |

The old failure is already fixed by the compare/exchange implementation in
`2961e7e`; it is not a current CI failure and does not justify relaxing warnings
or raising the MSRV.

The successful main macOS job took **8m48s**; the tag repeated the same commit
and took **9m20s**, with no dependency-cache step in either run. Tag job step
timestamps give this breakdown:

| macOS tag step | Elapsed |
|---|---:|
| Build workspace | 3m39s |
| Formatting | <1s |
| Clippy | 1m56s |
| Workspace tests | 1m05s |
| `ui-tests` | 1m48s |
| Documentation | 30s |

The actual `ui-tests` log reports 1m43s compilation, 48 unit tests in 3.90s,
and three CLI tests in 0.09s: compilation, not test execution, dominates.
The tag's headless jobs took 1m05s (MSRV) and 45s (stable). Runner logs show
Ubuntu 26.04 x86_64 and macOS 26 arm64. These are elapsed job/step times, **not
an invoice**; billing depends on repository plan, runner rates, and rounding.

Removing tag CI avoids that entire duplicate suite when tagging a tested commit.
Dependency caching should reduce downloads and dependency compilation, especially
on macOS; workspace compilation, linking, tests, and cache transfer still cost
time. **No warm-cache speedup has been measured yet.** Do not promise a percentage
or dollar saving until comparing subsequent equivalent runs.

## Cache contents, keys, and trust

`Swatinem/rust-cache` is pinned to the official `v2.9.3` commit
`d1527c8f67ef9af357290475d4af4b0fab8f12e0` (verified through the upstream tag).
Checkout and toolchain actions are also pinned; toolchain installation precedes
cache restoration so the actual compiler participates in the key.

This follows Etch's
[compiled-dependency cache fix `4a8c5bc`](https://github.com/exec/etch/commit/4a8c5bc832907ae2c6e4e086be109e656a82ffd4),
specifically `.github/workflows/release.yml` and `docs/RELEASING.md`. Etch previously
cached registry/Git downloads but **not compiled dependencies**; that fix uses
the same pinned action, excludes binaries/workspace artifacts, saves only on
main, and separates platform/native inputs. Ruckus had no dependency cache at
all. Ruckus adds MSRV/feature/vendor separation and keeps its debug-profile CI
instead of Etch's release packaging matrix. Etch must build tag release artifacts;
Ruckus's check-only workflow need not repeat branch checks on tags. No Etch
files were changed, and no Etch warm-cache speedup is assumed here.

The action caches Cargo registry archives/index and Git dependency downloads,
plus dependency artifacts from the workspace's `target` directory.
`cache-workspace-crates: "false"` excludes workspace artifacts, incremental output
is removed before saving, and `cache-bin: "false"` excludes installed tools.
No extra cache directories are configured.
The checkout, Cargo credentials/configuration files, runtime settings, chat logs,
user files, secrets, and release artifacts are not cache inputs.

The cache key has these layers:

1. `ruckus-v1`: manual namespace; bump it to abandon incompatible caches.
2. Action job ID and explicit OS/architecture, matrix toolchain, and feature-mode
   discriminator (`headless` versus `gui-ui-tests`; macOS also includes `macos-26`).
   One desktop cache intentionally contains dependencies for both its default GUI
   commands and its additional UI-test command; headless jobs never share it.
   macOS also fingerprints the selected developer directory, Xcode build version,
   SDK path/version, and Clang version before restoring. This detects native
   toolchain updates even when the runner does not export `SDKROOT` or
   `DEVELOPER_DIR`.
3. A hash of the root `Cargo.toml` (including workspace settings/patch declarations)
   and **all `vendor/**` files**, not just the vendored manifest. Changes to the
   GPUI layout patch must not reuse old local-dependency artifacts, even on a
   fallback restore.
4. The action's automatic compiler release/host/commit hash and Rust environment
   hash: dependency-relevant Cargo metadata, workspace/member manifests, workspace
   lockfile, Cargo config/toolchain files, and compiler-related environment
   variables. The action normalizes package versions/local paths and hashes the
   dependency portion of the lockfile rather than treating every textual edit as
   a dependency change. The explicit vendor hash also covers the vendored lockfile.
   macOS additionally includes deployment target, SDK root, and developer directory
   environment variables when set.

Exact hits require matching dependency environments. The action can fall back to an
older dependency environment within the same OS/architecture/compiler/feature/
vendor/root-manifest partition after a lockfile or member-manifest change; Cargo
then rebuilds changed dependencies. Compiler, native-toolchain, root-manifest, and vendor changes
form hard boundaries. Cargo's own
fingerprints remain authoritative, and **every check runs even on an exact hit**.
If changing feature commands, native toolchains, or cache policy, update the
explicit discriminator or namespace as appropriate.

Only successful **pushes to `main`** save caches. PRs (including forks) and other
branches may restore caches allowed by GitHub's branch scoping, but this workflow
does not save theirs. It uses `pull_request`, not `pull_request_target`, and only
requests `contents: read`. PR code is untrusted: never add secrets to cached
paths or assume a cache is private. Workflow edits in a PR are themselves
untrusted; main protection and workflow review remain necessary.

GitHub caches are immutable and subject to retention, eviction, storage quotas,
and repository billing policy. A miss is normal and must still pass. Monitor
cache sizes and transfer/save time, especially for GPUI; main-only saves and
dependency-only artifacts limit churn rather than guaranteeing zero storage cost.
Inspect without modifying caches:

```sh
gh cache list --repo exec/ruckus --limit 30
gh run list --repo exec/ruckus --workflow CI --limit 10
gh run view RUN_ID --repo exec/ruckus --json headSha,headBranch,jobs
gh run view RUN_ID --repo exec/ruckus --log
```

## Validation and follow-up measurement

Before landing workflow edits, run `actionlint .github/workflows/ci.yml` and
`git diff --check`; verify pinned action SHAs against their official repositories
and inputs against their pinned `action.yml`. A YAML parser alone does not
validate GitHub expressions. Run the root `AGENTS.md` Cargo verification commands,
plus the explicit native UI-test command on macOS. To reproduce headless CI,
use Rust 1.88.0 and stable and add `--no-default-features` to build, Clippy, test,
and doc commands (not formatting); use `RUSTDOCFLAGS='-D warnings'` for docs.

After an authorized landing, observe the first successful main run's cache save
and a later ordinary run's restore. Compare compiler, runner image, lockfile,
features, and vendor hash; record cache hit/miss, size, restore/save duration,
compilation versus test time, and total job times. Confirm all matrix checks and
native tests still execute. Check that a PR restores main's compatible cache
without saving, that a newer push cancels an older same-ref run, and that a tag
push does not start CI. A compiler/vendor change should miss the old partition;
lock updates may partially restore and rebuild. Do not push tags, rerun remote
jobs, delete caches, or publish just to validate this change without authorization.
