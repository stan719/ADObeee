# Extensible application themes

## Assessment and implemented scope

The original assessment used upstream main `68e91d4815ed43212895589411aa3d21b2a84bc3`.
The branch now integrates upstream `8f8e023` without rewriting the two initial
commits. Closed Windows PR #616 and its separate checkout are untouched.

Upstream PR #212 (`42078a8`) unified the toolbar and View menu around
ThemePreference and a single setter. Earlier #76 raised text contrast. Existing
semantic Tokens already supplied most custom chrome, with some feedback colors
and egui defaults scattered across components. Preferences lived as JSON inside
eframe storage; System was resolved against OS appearance each frame.

The initial theme commits added six presets and the custom storage model, but
only activation was available through UI control. The follow-up adds all ten
management operations to the existing CLI/MCP tool registry and a shared native
store. Natural-language theme generation and a graphical custom-theme editor are
**future work**. No new MCP server or third-party dependency package was added.

## Shared architecture

`pdfcraft-themes` is the GUI-independent L7 theme domain. It owns ThemeKind,
ThemePreference, Palette, ColorToken, PaletteDocument, ThemeDefinition, ThemeLibrary,
and ThemeState. It reuses `ecolor` for typed colors; it has no egui, windowing or
GPU dependency. The egui module reexports the model and adapts it to egui Visuals.
Native storage reuses the existing platform staging helper, standard-library file
locks, and the already-used ron/getrandom packages. No engine document state is
involved in a theme edit.

`ThemeState::execute(operation, arguments)` validates the operation on a copy,
then commits all changes or none. `PdfCraftApp::manage_theme` and automation tools
call this API. The same palette represents built-ins and custom themes. Built-ins
are immutable. UUID identities are independent of editable names; duplicate names
are allowed. Native creation generates UUID v4 IDs unless a caller supplies one;
browser callers currently supply an ID obtained from browser entropy.

Native `ThemeStore` transactions re-read the latest state under an advisory lock,
apply one operation and atomically replace the JSON file. GUI selectors use that
same path. The running GUI checks for external changes about once per second and
reloads them. Custom themes appear in the View selector and toolbar context menu, with a
scrolling list. Upstream’s Appearance preferences also use the shared selection
API and preview the active preset/custom palette. The toolbar’s primary click
keeps upstream’s System/Light/Dark cycle; its context menu exposes every theme.
The future editor should use this API rather than another preference or file format.

System, Light and Dark keep their historical appearance and encodings. System
continues following OS appearance changes, including upstream’s opt-in native Linux desktop
portal watcher. Unknown appearance uses Light; native windows retain inherited
appearance while both egui style branches receive the selected palette. A headless inspection of System returns
the Light fallback palette and `follows_system: true`; it does not guess desktop
appearance. PDF paper, pixels, annotations, redactions and signature ink are not
application theme tokens. Native file dialogs remain styled by the OS.

## Preset names and stable IDs

| Display name | Stable ID |
|---|---|
| Atelier | `catppuccin` |
| Editorial | `solarized` |
| Heritage | `forest` |
| Executive | `ocean` |
| Sienna | `sunset` |
| Paper & Ink | `paper-ink` |

These are presentation names, not new identities. Palette values, serialized
preferences, command IDs, CLI/MCP arguments and imported `base_theme_id` references
are unchanged. Existing custom themes keep their chosen names and base palettes.
For example, `base_theme_id=ocean` still creates a copy of Executive. The five new
proper names retain their spelling in localization catalogs; Paper & Ink keeps
its existing translations.

## Palette schema

The registry is exhaustive: adding a Palette color without registering it fails
compilation. All 31 stable field names are exposed in `pdfcraft-cli tools` schemas.
Values are `#RRGGBB` or `#RRGGBBAA`. Create and update accept partial objects;
unknown patch keys and malformed colors reject the whole operation. Supplied
hex values are retained without color correction (rendering uses egui's color
precision). Inspect returns the complete palette; export retains the definition.

| Group | Fields |
|---|---|
| Surfaces | `titlebar`, `chrome`, `panel`, `pasteboard`, `card`, `field` |
| Borders | `border`, `divider` |
| Text | `text`, `text_muted`, `text_faint`, `icon` |
| Controls | `hover`, `pressed`, `selected`, `focus`, `selection_text` |
| Accents | `accent`, `accent_text`, `accent_soft`, `on_accent`, `badge_new` |
| Feedback | `success`, `warning`, `error`, `info`, `notice_bg`, `notice_text` |
| Shadows | `page_shadow`, `window_shadow`, `popup_shadow` |

Panel is shared by sidebars/tool panels; card by dialogs/popups; chrome by the
mode bar/toolbar. Pasteboard is behind PDF pages. AccentText covers links and
accent-button hover; OnAccent is their foreground. Focus covers focus/caret strokes.
Layout metrics and fonts are not color tokens. `legacy_defaults` preserves the
historical Light/Dark defaults in untouched copies; editing a role overrides its
legacy consumers, while resetting to the legacy value restores that treatment.

Inspect and editing results report advisory `warnings` and structured
`contrast_diagnostics`, without rejecting or rewriting valid low-contrast colors.
Each finding has stable `role`, `foreground_token`, `background_token`, `ratio`,
`threshold`, `kind`, and `message` fields. Normal-sized text uses 4.5:1; icons and
focus outlines use 3:1 as non-text indicators. Checks cover actual card/panel/field
surfaces, placeholders, hover/pressed/selected states, badges, feedback, notices,
and primary/secondary buttons. These are practical checks, not an accessibility
certification. Translucent roles report an unknown ratio because their contrast
depends on compositing; shadows are excluded from text checks.

Both primary button painters share the same derived state calculation. Editorial’s
pressed fill is moved only as far toward its original accent as necessary to reach
4.5:1. The known legacy Dark primary fill also needs a minimal derived adjustment.
All built-in fills and tested animation states meet 4.5:1. Stored palette tokens
stay unchanged, and exact built-in copies render identically. Arbitrary custom
palettes with a low-contrast resting accent retain their chosen treatment and
receive advisory diagnostics. Derived button fills are computed once when a
palette is applied, rather than on every painted button.

## CLI command reference

Use the existing runner; no graphical app is required:

```sh
pdfcraft-cli --help
pdfcraft-cli tools
pdfcraft-cli run theme_list store=/absolute/path/themes.json
pdfcraft-cli run theme_inspect store=/absolute/path/themes.json id=ocean
pdfcraft-cli run theme_create store=/absolute/path/themes.json name='Navy and cream' base_theme_id=ocean 'palette={"text":"#fff4df","accent":"#efbd88"}'
pdfcraft-cli run theme_activate store=/absolute/path/themes.json id=custom:UUID
```

Every tool requires `store`, a JSON file path. Relative paths resolve under
`--root` when supplied; the store, lock and legacy settings paths must remain
inside that root. The parent folder must already exist. An explicit path avoids
silently changing a user's desktop profile from a confined agent session.

| Tool | Other arguments | Result/purpose |
|---|---|---|
| `theme_list` | none | Built-ins/custom entries, active ID, revision, diagnostics |
| `theme_inspect` | `id` | Complete palette, metadata, warnings, revision |
| `theme_create` | `name`, optional `base_theme_id`, `palette`, UUID `id` | New custom theme; base defaults to Light |
| `theme_clone` | source `id`, `name`, optional UUID `new_id` | Independent copy with complete palette |
| `theme_update` | custom `id`, `palette` | Validated partial color patch |
| `theme_rename` | custom `id`, `name` | Rename without changing identity |
| `theme_activate` | `id` | Persist selection; running native GUI reloads it |
| `theme_delete` | custom `id` | Delete; active theme falls back to its built-in base or Light |
| `theme_export` | custom `id` | Versioned definition JSON on stdout |
| `theme_import` | `document` object | Import without overwriting an existing ID |

Mutation tools accept optional `expected_revision` from list/inspect. A mismatch
returns `revision_conflict`; inspect again and deliberately reapply the edit.
Stable selection IDs are `system`, `light`, `dark`, `catppuccin`, `solarized`,
`forest`, `ocean`, `sunset`, `paper-ink`, and `custom:<uuid>`. A creation/copy UUID
argument omits the `custom:` prefix. System is a preference, not a clonable palette.

JSON results go to stdout. Theme operation failures emit a JSON object to stderr
and exit nonzero, for example:

```json
{"error":{"code":"builtin_protected","message":"…","tool":"theme_delete"}}
```

Codes include `invalid_args`, `not_found`, `conflict` (including ID collisions),
`builtin_protected`, `unsupported_schema`, `revision_conflict`, `store_busy`,
`invalid_store`, `invalid_legacy_settings`, `invalid_path`, `storage_error`,
`storage_limit`, and `entropy_unavailable`. Retry `store_busy`; do not automatically
retry a stale revision. General runner syntax/file errors retain CLI conventions.

For structured workflows, `pdfcraft-cli run --script steps.json --root PROFILE`
accepts the existing array format (each completed operation commits separately):

```json
[
  {"tool":"theme_create","args":{"store":"themes.json","id":"01234567-89ab-4def-8123-456789abcdef","name":"Navy and cream","base_theme_id":"ocean","palette":{"text":"#fff4df","accent":"#efbd88"}}},
  {"tool":"theme_activate","args":{"store":"themes.json","id":"custom:01234567-89ab-4def-8123-456789abcdef"}}
]
```

## Import/export and storage compatibility

Export returns the existing versioned definition directly, with no second wrapper:

```json
{
  "schema_version": 1,
  "id": "01234567-89ab-4def-8123-456789abcdef",
  "name": "Navy and cream",
  "kind": "Dark",
  "base_theme_id": "ocean",
  "palette": {"text":"#fff4df","accent":"#efbd88"},
  "metadata": {"author":"Example"}
}
```

Pass that object as `theme_import`'s `document`. Missing colors inherit a concrete
built-in base (or the record's light/dark kind). No recursive custom inheritance is
used; cloning a custom theme freezes all currently supported roles. Imported
unknown fields, metadata and unknown token values survive export and editing.
Known invalid colors, unsupported imported versions and ID collisions are refused.
Cloning generates a new ID; import never silently replaces an existing definition.
Unsupported stored definitions remain available for diagnosis and safe display
fallback, but cannot be activated, edited or cloned by this version.

Native `themes.json` stores `{schema_version, revision, theme, theme_library}` plus
retained extension fields. The library keeps its existing versioned `themes` array.
Selection retains legacy JSON strings `System`, `Light`, `Dark`, lowercase preset
IDs, or `{"Custom":"<uuid>"}`. Future migrations must be explicit version-to-version
transforms that preserve unknown data, not implicit rewrites.

The desktop store is next to `app.ron`:

- macOS: `~/Library/Application Support/PdfCraft/themes.json`
- Linux/FreeBSD: `$XDG_DATA_HOME/pdfcraft/themes.json`, or `~/.local/share/pdfcraft/themes.json`
- Windows: `%APPDATA%/PdfCraft/data/themes.json`
- Portable mode: `PdfCraftData/themes.json` beside the executable

GUI startup imports its restored theme preference/library only when this file does
not exist. Headless operations at a file named `themes.json` can also migrate the
adjacent `app.ron` (`pdfcraft` or legacy `printcraft` entry). CLI migration refuses
malformed legacy data and leaves it intact. Other filenames are isolated stores
with Light defaults. Former PrintCraft *folder* migration still belongs to desktop
startup: run the upgraded app once before pointing the CLI at that older profile.

The dedicated file is authoritative after migration. eframe settings retain a
compatibility mirror of accepted state, but GUI autosave never writes it back over
`themes.json`; an unvalidated pending native selection is not mirrored.
If a store is invalid/unreadable, the GUI keeps its last usable theme, reports the
error, and refuses persistence edits; it still launches. The invalid file is never
automatically replaced. Browser builds continue using eframe browser storage with
the same model; native CLI tools do not edit browser storage. Cross-tab browser
concurrency remains work for the future browser editor.

A persistent `.lock` sidecar is used for standard-library OS advisory locks. The
OS releases a lock when its file handle closes or the process crashes; the sidecar
is deliberately never unlinked. Writes use a new adjacent staging file, sync its
contents, and rename it over the target. Cooperating native clients cannot silently
lose unrelated edits. `expected_revision` also protects edits made from an old
snapshot. Manual edits or older app versions do not participate in this protocol.
Power-loss durability remains subject to the filesystem. There is no entry-count
cap; files have a 64 MiB practical safety bound. Reads/inspection do not create files.

### Native loading and active-color caching

Native GUI startup initializes/loads the dedicated store on one background worker.
The frontend polls its completion without waiting on file reads or JSON parsing.
Each second the worker checks file length, modification/creation times and, on
Unix, device/inode/change-time identity. Changed metadata triggers a bounded read.
During active GUI polling, once ten seconds have elapsed it also compares exact
bytes, even if all metadata is
unchanged; this catches coarse timestamps, timestamp-preserving in-place edits and
atomic replacement on filesystems with weaker metadata. There are no hash-collision
assumptions. Identical contents (valid or rejected) are not reparsed. Arbitrary
metadata-preserving changes are detected at the next verification, plus worker
loading/scheduling time; paused hosts catch up when polling resumes. Current and cached byte buffers retain the existing 64 MiB bound.

A single worker and bounded channels prevent reload jobs accumulating. Selector
changes coalesce to the newest choice. Generation tags discard old completions;
failed loads keep the last accepted valid palette/library. Native selections use
the same validated store transaction API on the worker. Shutdown drains the latest
pending choice before joining the worker. Transient cross-process lock contention
is retried on the worker for up to two seconds. Permanent lock or write failures
retain the last accepted state; shutdown cannot guarantee a write to an unavailable
store. No retries or joins run during normal UI frames.
Synchronous `manage_theme` and `enable_theme_store` remain available to headless
hosts; while frontend work is pending, synchronous management returns `store_busy`
instead of racing an older activation. Running GUI control selections are queued;
storage failures are reported through the existing notification path.

Library snapshots share their immutable JSON envelope through copy-on-write data.
A lazily cached ID index and selector metadata never enter the wire format. The
worker prewarms selector metadata and resolves the active palette before delivery;
GUI lists use cached names and virtualized rows. Display labels are bounded to 160
Unicode characters without truncating stored names or changing IDs.
The worker prepares active custom colors before handing a snapshot to the GUI.
The active palette is reused until selection, library snapshot (including revision
or definition changes), or a relevant System appearance/fallback input changes.
Manual palettes are not invalidated by irrelevant OS appearance changes. Legacy
eframe settings restoration still decodes its compatibility mirror once at startup;
browser hosts retain their existing persistence backend and build the index lazily.
The periodic native JSON parse and active-last linear frame scans are eliminated.

### New-import reference validation

Current-schema `theme_import` validates a declared `base_theme_id` before committing.
Concrete built-in IDs and existing current-schema custom bases are accepted. Unknown,
malformed, missing, ambiguous, self-referential, cyclic or unsupported-schema bases
return `invalid_base_theme` with an actionable message; System is not a concrete
base palette. Custom chains are walked iteratively with a visited-ID set, without
an arbitrary depth/count cap. Missing supported colors are materialized from the
resolved base at import, retaining exact inherited hex values (including translucent
input), the declared ID, explicit colors and unknown
extensions. These are snapshots, not live inheritance links.

Existing stored unfamiliar references/schemas are preserved and retain their old
fallback behavior. They are not silently migrated, rewritten or rejected during
loading. Unrelated imports leave them untouched; an import that newly depends on
an unresolvable reference fails atomically. The versioned JSON format remains
schema 1.

## MCP and future AI agents

All ten tools are already exposed by `pdfcraft-cli mcp` through the same registry,
validation and storage adapters. The existing stdio server remains opt-in. Compact
mode discovers them via `tool_search` and invokes them via `tool_call`; ordinary
`tools/call` also works. JSON Schemas include every palette field. Failed operations
return `isError: true` and `structuredContent.error` with a stable code/message.
No GUI manipulation is necessary to construct, inspect, save or activate a theme.

A future agent can translate “professional navy and cream with warm accents” into
a `theme_create` palette, inspect its warnings, refine it using `theme_update`, and
activate it after the user's requested workflow. The application does not yet
implement natural-language generation, a provider, or automatic contrast correction.

Atelier is inspired by Catppuccin Mocha; Editorial is inspired by Solarized.
Both source palettes are MIT-licensed, with pinned notices in
NOTICE and full licenses in `docs/licenses/`. Preset labels are present in all 16 current catalogs. Typography, layout, icons and PDF rendering are unchanged.

## Separate editor PR

1. Build on the shared operations to add a theme library with virtualized rows, built-in and custom sections, and no
   arbitrary entry cap. Offer Create, Duplicate, Rename and Delete. Use the core’s native UUID generation (or browser entropy for WASM); never derive it
   from a label. Deleting the active custom theme selects its base or Light.
2. Open a dedicated editor with collapsible Surfaces, Text, Controls, Borders,
   Feedback and Accents groups generated from the token registry. Each row has a
   color picker, validated hex input, sample and Restore default action. Include
   whole-palette Reset to base and a small preview of menus, inputs and status text.
3. Keep a draft separate from the saved library. Live preview changes only the
   effective palette. Apply keeps a session preview; Save atomically updates the
   definition and selection; Cancel restores the pre-edit selection and palette.
   Closing or switching with dirty drafts offers Save, Discard or Continue editing.
4. Show contrast warnings for text/surface combinations (4.5:1), focus and important
   non-text affordances (3:1). Invalid syntax blocks Save; accessibility warnings
   are explicit and explain the affected roles. Do not silently rewrite colors.
5. Add file pickers around the implemented import/export operations, preserving unknown metadata.
   Validate IDs, names, versions and colors. Imported ID collisions offer a new
   copy or explicit replacement. Unsupported schema versions are preserved but
   read-only; migrate only with an explicit versioned transformation and backup.
6. Add restart, storage-failure, Cancel/Apply/Save, duplicate-name, collision,
   corrupt-token, schema-migration and large-library tests. Exercise native and
   browser persistence, keyboard navigation and every token's live update.

The editor should use these models rather than introduce another appearance
preference, palette format or independent style pipeline.
