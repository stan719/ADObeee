# Theme foundation and automation validation

Validated on macOS on 2026-10-11 on `feature/extensible-themes`. Original commits
`70e2846` and `af43d9a` are preserved. Upstream main
`798650e8a09607471dec48e1aa7949a4d37b9aec` was merged as `dc687c1`, then
`b8ddf71e30959dc8a5ca4b66c40b8fcaea68cf76` was fetched, reviewed and integrated
without rewriting existing commits. The latest merge retains Linux desktop
appearance tracking and adds the preset names to Polish. Closed PR #616 and its separate checkout remain untouched at
`2a884c911205332160808e05b326d6862a78a711`.

## Naming refinement (PR #1090)

Atelier, Editorial, Heritage, Executive, Sienna, and Paper & Ink are the display
names. The original IDs and all palette values remain unchanged. A before/after
CLI comparison verified every one of the 31 colors for all nine selections.
All 16 localization catalogs contain the six display-name keys exactly once.
The original Catppuccin/Solarized license notices and license files are unchanged.
Regression coverage checks the GUI selectors, CLI/MCP display metadata, serialized
IDs, and imported partial custom definitions resolving the old base IDs after a
restart, export and active-theme deletion.

The naming commit was initially blocked by upstream merge conflicts. The follow-up
merges upstream `8f8e0236aaef16a4f3ca52a074ad5d48da6498f7`, preserving the shared
theme model unchanged. It combines the version/dependency updates, localized
catalogs, toolbar/context-menu behavior and new Appearance preferences. The
Appearance picker includes every preset and custom theme; its cards preview the
actual selected palette. Regression coverage exercises those choices and a custom
palette through the new preferences section.

At the start of this follow-up, the checkout was clean and fully pushed. GitHub
reported content conflicts, no check runs for the naming commit, and no visible
main-branch protection rule. Actions-policy access was denied, so fork-workflow
approval requirements could not be confirmed. No repository settings were changed.

The final naming rerun passed formatting, workspace Clippy with warnings denied,
all 2,125 workspace tests (zero failures, six existing ignores), native desktop/CLI
builds, assets and layering. The 34-image visual test was regenerated and the menu
was inspected with the new labels. That run’s parity failure was fixed in the newer upstream merge. Missing local
WASM/cargo-deny limits still need external validation.

The final integration run passed 2,398 tests (zero failures, six existing ignores),
formatting, Clippy, native builds, assets, layering and parity. The 34-image visual
check passed; the Executive Appearance card was inspected. Before/after CLI output
confirmed identical names, stable IDs and all 31 colors across the merge.

## Latest conflict resolution (PR #1090)

Merged upstream `f3571c591f0ec4ee7faea67c011b3ee1eb6ba913` into the published branch
without rewriting history. Retained upstream’s Linux portal lifecycle, inherited
native window appearance, outlined controls, localization additions and grouped
UI tests. Both egui style branches receive the selected palette. System mode uses
Light when the OS supplies no preference, matching upstream. The shared theme
model, storage schema, stable IDs, all 31 colors and ten operations are unchanged.
An exact before/after CLI JSON comparison confirms all nine built-in selections.
All 20 catalogs now include the preset names and custom-theme label. The 34-image
visual test passed; the Executive Preferences and preset menu were inspected.

The first run found two new catalog tests that needed the same proper-name
allowlist as the existing Czech test. After that integration correction, the full
workspace rerun passed **2,555 tests, zero failures, six existing ignores** across
149 test groups. Formatting, all-target Clippy, native desktop/CLI builds, CLI
without MCP, assets (269 repository, six dependency, 22 build inputs), layering
(36 crates), and parity passed. WASM still fails for the missing target standard
library; cargo-deny reports SKIPPED. Other operating systems were not built here.

## 2026-10-12 upstream review and merge

Reviewed and merged upstream `87b448b664e1b3c960b479cc0f2c2e7a82c7ac71` into the
existing feature branch, preserving published history. The 27 conflicts below
were resolved by retaining both independently added functionality. Upstream’s
0.7.0 workspace version, press/hover interactions, animation preference, reading
colors, localization and font/editing improvements remain intact. Reading colors
remain document-render settings, separate from the application palette. The theme
core and storage schema are unchanged; exact CLI JSON equality verifies all nine
built-in names, IDs and all 31 colors. All 21 catalogs include the preset labels.

Prior-head CI passed Linux, WASM, dependency audit, FreeBSD and both Windows ARM64
jobs; scheduled fuzzing was skipped. Windows failed only the theme document-state
regression: repeated saves differed by one byte in ModDate, crossing a second.
The test now uses the engine’s fixed-clock API and retains exact PDF byte equality.
No production save behavior was changed.

Local final validation: **2,865 passed, zero failed, 13 upstream ignores**. Formatting,
all-target Clippy, native macOS desktop/CLI builds, CLI without MCP, assets (269
repository, six dependency, 22 build inputs), layering (36 crates), and parity pass.
WASM still lacks its target standard library locally; cargo-deny reports SKIPPED.
The 34-image visual check passed; the Executive selector was inspected.
The additional upstream ignores require private document/font fixtures or are a
Windows font-raster measurement probe. Hosted results for the new head are tracked
in PR #1090; prior-head successes do not establish new-head success.

No reviews or main-branch protection rule were visible at diagnosis; the listed
repository ruleset applies only to release. Maintainer review remains appropriate.
Keep this PR intact: palette, persistence, GUI selection and automation are one
coherent API/state change. A graphical editor remains a separate future PR.

The table attributes upstream additions in each conflicting hunk using git blame;
several translation/progress append conflicts naturally span multiple commits.

| Conflicting file | Upstream commits touching the conflicting additions |
|---|---|
| `ATTRIBUTION.toml` | `0cb311d`, `0e55d07`, `4bfd3c2` |
| `Cargo.toml` | `78f00e4` |
| `ROADMAP.md` | `09287e5`, `c52072b` |
| `crates/automation/README.md` | `6d843a8` |
| `crates/automation/tests/automation.rs` | `186c1dc`, `6d843a8` |
| `crates/ui-egui/src/home.rs` | `a786c8f` |
| `crates/ui-egui/src/i18n/ar.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/bg.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/de.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/el.tsv` | `012b34a`, `06480e3`, `09287e5`, `0e55d07`, `4541974`, `7183bf4`, `a8940f0`, `b1784fc`, `c1d9894` |
| `crates/ui-egui/src/i18n/fr.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/hu.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/it.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/ja.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/kk.tsv` | `012b34a`, `06480e3`, `09287e5`, `4541974`, `7183bf4`, `a8940f0`, `b1784fc`, `c1d9894` |
| `crates/ui-egui/src/i18n/pt-br.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/ru.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/sl.tsv` | `012b34a`, `06480e3`, `09287e5`, `7183bf4`, `a8940f0`, `b1784fc`, `c1d9894` |
| `crates/ui-egui/src/i18n/te.tsv` | `2aea7dc` |
| `crates/ui-egui/src/i18n/tr.tsv` | `012b34a`, `06480e3`, `09287e5`, `4541974`, `7183bf4`, `a8940f0`, `b1784fc`, `c1d9894` |
| `crates/ui-egui/src/i18n/uk.tsv` | `4541974` |
| `crates/ui-egui/src/i18n/zh-hans.tsv` | `4541974` |
| `crates/ui-egui/src/icons.rs` | `a786c8f` |
| `crates/ui-egui/src/lib.rs` | `4541974` |
| `crates/ui-egui/src/panels.rs` | `a786c8f` |
| `crates/ui-egui/src/widgets.rs` | `8cacb44`, `a786c8f` |
| `docs/roadmap.md` | `13637b8`, `186c1dc`, `2839152`, `51982db`, `7caaa35`, `87b448b`, `8cacb44`, `c1d9894`, `c52072b`, `fcf6e39` |

## Passed

- `cargo test --workspace --no-fail-fast`: **2,865 passed, zero failed, 13 ignored**.
  This repeats and expands the original 1,765-test baseline after upstream integration.
  The upstream ignores require network, corpus, private document/font and local
  signature/keychain inputs, or are timing/raster measurement probes.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo build -p pdfcraft -p pdfcraft-cli` on native macOS, plus
  `cargo check -p pdfcraft-cli --no-default-features`.
- `cargo xtask parity`: passes after integrating the upstream manifest repair.
- `cargo xtask assets`: 269 repository assets, six dependency assets, 22 build inputs.
- `cargo xtask layers`: 36 crates, no layering violations.
- Focused final reruns: all 21 shared-core/store tests, all twelve theme integration
  tests, and the adjusted MCP structured-error regression. The core tests include
  refusal of a standalone export accidentally supplied as a store. Upstream integration
  regressions cover native appearance inheritance, unknown-OS fallback and saved
  System preferences alongside preset and custom-palette coverage.

Native control-channel and screenshot tests need loopback/GPU access, so the full
workspace and visual runs used an execution environment with those facilities.

## Coverage

The shared core tests all six presets, stable IDs, all 31 token names and edits,
complete palette inspection, creation, cloning, patches, renaming, activation,
deleting the active theme, versioned import/export, duplicate names, ID collisions,
legacy preferences, unknown extensions and token data, unsupported schemas,
invalid colors, and preservation of exact translucent input values. Failed
operations leave the state unchanged. A 1,024-entry library test retains the
original guarantee of no arbitrary entry-count cap.

Store tests cover atomic replacement, invalid/malformed files, migration from
legacy app.ron without changing it, revisions, lock contention, concurrent
independent edits, symlink refusal, future schemas and failure-safe writes. GUI
integration verifies that a CLI edit survives a subsequent GUI selection and
restart, and that a malformed store is not replaced by a preference change.

Headless CLI tests run the complete create/inspect/update/rename/activate/clone/
export/delete/import workflow in separate processes, check all palette keys,
System/Light/Dark, malformed input, JSON errors and nonzero exits. MCP tests verify
normal and compact tool discovery/invocation, structured failures and registry
schemas. Existing hostile-input and root-confinement tests also cover the new tools.

Contrast tests check critical text and interaction states. Editing reports warnings
rather than changing user-selected colors; translucent roles report that effective
contrast depends on the surface below them.

Visual validation generated **34** local home/document/menu/dialog images covering
Light, Dark, all six presets, and a custom navy-and-cream theme. The custom menu was
inspected and selected through the existing GUI selector. PDF paper stayed white.
System-font loading was disabled; images and logs are ignored local artifacts.

## Remaining upstream and environment limits

- `cargo xtask wasm` was attempted and failed because this Rust installation lacks
  the `wasm32-unknown-unknown` standard library. Browser compilation/persistence
  still need CI validation. Native filesystem tools are explicitly gated out on
  WASM; browser hosts use the shared in-memory API.
- Windows, Linux and FreeBSD builds were not run on this macOS host.
- `cargo xtask deny` reported **SKIPPED** because cargo-deny is not installed; its
  zero wrapper exit code is not a successful dependency audit.
- Optional craft-fonts and corpus inputs were not installed. No fonts or new
  third-party dependency packages were added.

Natural-language generation and a graphical custom-theme editor remain future
work. Their integration plan, native storage concurrency protocol, browser limits
and import/export contract are documented in [themes.md](themes.md).

## Maintainer feedback follow-up (2026-10-12)

Integrated upstream `0a0986483378b57d62103a36490bd66166d32c8f` once into the
existing feature branch. No stored palette values, preset names or IDs, schema-1
format, or CLI/MCP operations changed.

The follow-up addresses the four review findings:

- Primary-button resting, hover and pressed fills now use the same derived-state
  calculations in diagnostics and GUI painting. Contrast tests exercise the actual
  colors across all presets and interpolation; Editorial's failing pressed state is
  adjusted as a derived treatment without changing its stored palette.
- Advisory contrast findings now cover cards, panels, inputs, placeholders,
  selection, badges, feedback colors and button states. The Ocean `#e0effa`
  text-on-card example reports 1:1. Low-contrast custom palettes remain usable;
  diagnostics never block saving, importing or activation, and translucent colors
  are reported as surface-dependent.
- Native GUI theme loading and watching run on one bounded background worker.
  An immutable indexed library and active-palette cache avoid reparsing the full
  store on every frame. A synthetic 10,000-theme store parsed once over 63
  unchanged polls; the initial decode/index took 124 ms, index plus first active
  resolution took 46 ms, and 60 cached resolutions took 5 microseconds in this
  local optimized test profile. These are measurements, not timing thresholds.
- New imports validate built-in and custom base references, reject invalid,
  missing, self-referential and cyclic bases atomically, and inherit the exact raw
  supported color values. Tests verify unknown/future stored data remains intact.

The current full workspace run passed **2,903 tests, zero failures, 13 ignored**
across 157 test groups. It includes 38 shared theme-core tests, five focused GUI
reload tests and four headless CLI theme tests. Six core regression cases and the worker lock-retry regression were also
run with their respective fixes removed and failed as expected; a separate
translucent-inheritance regression reproduced the bug before its fix. Formatting,
all-target Clippy, native desktop/CLI builds, CLI without MCP, assets, layering and
parity passed. The 34-image visual check passed; the Editorial Appearance dialog was inspected.
Exact CLI JSON comparison
confirmed all nine selections still have the same names, IDs and 31 palette
values.

Local WASM validation could not run because `wasm32-unknown-unknown` is not
installed. `cargo-deny` is unavailable, so the dependency audit is **SKIPPED**.
The new PR-head hosted checks had not started at this validation snapshot; earlier
head CI results do not establish the result for this commit.


## Current-main integration audit (2026-10-13)

Integrated upstream `7437bde378f43856eada40af25b983812e615e8a` without rewriting
published theme history. Seven actual conflicts were resolved by retaining both
sides: CLI batch/theme help, automation modules and documentation, Lithuanian
strings, roadmap records and recomputed translation hashes. The theme domain,
primary-button calculations and background theme worker are byte-identical to
`58deadacecb124185b5fe2577cfecf49f6ed73dc`. All four maintainer corrections, stable
IDs/names, all 31 palette values, storage compatibility and ten operations remain.

Two necessary compile corrections accompany the integration: import the existing
`ThemeContext` adapter in upstream's new window-controls module, and increase the
UI crate's compile-time macro recursion limit to 256 for the expanded settings
JSON. Neither changes the persisted schema or runtime behavior.

Validation on macOS / Rust 1.99.0:

- Theme-domain suite: 38 passed, zero failed.
- Theme UI group: 12 passed, zero failed; the visual matrix generated 34 images.
  The Editorial Appearance dialog and custom-theme menu were inspected.
- CLI suite: 58 passed, zero failed, including four theme process tests and MCP
  theme/compact-discovery coverage. Automation theme-schema discovery passed.
- Desktop suite: 68 passed, zero failed, one existing ignore.
- Strict all-target Clippy for themes/UI/automation/CLI/desktop, formatting,
  native desktop/CLI builds, CLI without MCP, layering, assets and parity passed.
- Exact FreeBSD-failing renderer test passed locally: one passed in 1.28 seconds.
- Broader UI suite: 1,086 passed, 13 failed, five existing ignores. Twelve failures
  involve upstream background-open/command/password/recovery tests; the other is
  the existing mouse-wheel resumption test. These are not a green acceptance
  result. Current-main CI independently fails the command/open path; its deferred
  command is lost when `open_path` schedules work without a continuation.
  Upstream [PR #1764](https://github.com/storytold/pdfcraft/pull/1764) proposes a
  separate fix for continuation/startup ordering and completion-aware tests.
- A broader automation run passed 253 tests with one existing ignore before
  failing the upstream invalid-UTF-8 filename fixture: this macOS filesystem
  rejects creating the fixture (`Illegal byte sequence`). Two timestamp-only
  batch fixture comparisons passed on rerun; no test assertions were changed.

No renderer, timing, scrolling or unrelated UI tests were weakened or modified.
Local WASM cannot run because its target library is absent; Windows/FreeBSD
runtime and COM tests require their platform CI. Dependency auditing was green on
the prior published head, but was not rerun locally. Prior hosted checks do not
establish results for this integration.

The previous FreeBSD run 38316543061 remains failed at unchanged renderer test
`page_contents_and_nested_forms_share_one_budget` (255 passed, one timeout).
Both connected-app and local-credential reruns were denied (403 / admin rights
required). This suggests a FreeBSD VM timing problem but does not prove the
underlying cause. A maintainer must rerun or investigate if it recurs on fresh CI.


## Renewed current-main validation (2026-10-13)

Integrated upstream `5e12f5fd6d5dcf86bb332ca5940c622059168fea`, including the
already-landed `b5467d93` native-opening correction, without duplicating that fix
or rewriting published history. Retained both roadmap histories and combined
M3 milestone descriptions. Translation-only checksum conflicts were resolved by
hashing the combined catalogs and regenerating attribution; all 270 repository
asset hashes verify. No theme-domain, button-state or background-worker logic
changed. All four maintainer corrections and existing palette/storage/API
contracts remain intact.

Local macOS validation at this upstream revision:

- Theme core: 38 passed, zero failed.
- Full UI package: 1,116 passed, zero failed, five existing ignores. Both previously
  failing command/open-recent tests now pass, as do password/recovery/startup,
  theme GUI and tab-navigation coverage.
- Automation: 192 passed, zero failed.
- CLI: 58 passed, zero failed, including theme process and MCP discovery tests.
- Desktop: 68 passed, zero failed, one existing ignore.
- Formatting, strict all-target Clippy, layers, assets, parity, native desktop/CLI
  builds and CLI without MCP passed. Tests and builds were run serially, bounded
  by timeouts, using at most two compilation workers.

The earlier failed UI and batch results above are historical and superseded by
this passing integrated validation. No tests were independently weakened,
skipped or altered to obtain these results; upstream's correction and tests are
included as part of its normal merge. Local WASM/Windows/FreeBSD runtime checks
remain unavailable on this macOS host; fresh hosted CI must validate this HEAD.
