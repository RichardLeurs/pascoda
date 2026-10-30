# Pascoda

A modern Pascal IDE with coding agents for macOS.

A modern IDE for Pascal (macOS ARM for now), written in Rust. It wraps the Free Pascal compiler and aims at apps with a macOS-native look and feel.

Supports coding agents (OpenAI API and compatible, OpenAI Codex CLI, Claude Console API, Claude Code CLI for now). 
Connect local LLM-models using oMLX and Ollama. OpenAI Codex CLI and Claude Code CLI (and other CLI's) run in the Pascoda terminal inside the IDE.

> ** Status: early work in progress. The Pascoda IDE app builds and compiles Pascal code and builds .app and .dmg ** 

## Specs

| Area | Status | Notes |
|---|---|---|
| Debugger | working | lldb through `lldb-dap`: breakpoints, step, call stack, variables; Pascal sets are not shown |
| Code completion | working | `pasls`: Ctrl+Space and as-you-type completion, hover, F12 and Cmd-click go-to-definition; packages are not on its search path yet |
| Build and run | working | console and GUI programs, progress bar, Problems pane, export to `.app` and `.dmg` |
| Project files | partly | `.lpi` and `.dproj` give the main program, search paths, defines, target and switches; `.dfm` opens in the designer; `.lpk` and `.dpk` are not read yet |
| Form designer | working | native controls, palette, menus, action lists, anchors; the project's own components appear on a "Project" palette page |
| File tree | partly | new file, new folder, rename, delete to the Trash; move is not done |
| Terminal | working | a real login shell in the bottom pane |
| AI assistant | working | OpenAI-compatible and Anthropic Claude providers, local LLMs |
| Packaging | partly | `Pascoda.app` and `Pascoda.dmg` build; the notarization path has not been run 

## Requirements

You need an Apple Silicon Mac with macOS 14 Sonoma or later (generated apps target 14.0; the IDE itself was developed on macOS 27).

| Tool | Used for | Notes |
|---|---|---|
| Rust (stable, tested with 1.98) | building the IDE | edition 2024 features are used |
| Free Pascal 3.2.2 (`fpc`) | compiling Pascal | `brew install fpc`; expected at `/opt/homebrew/bin/fpc` |
| Xcode (Command Line Tools at minimum) | linker, `codesign`, `lldb-dap` | the debugger adapter `lldb-dap` ships only inside Xcode |

### Vendored sources (in `vendor/`, not committed)

| Folder | What | Source | Used for |
|---|---|---|---|
| `vendor/pasls` | Pascal language server | github.com/genericptr/pascal-language-server | completion, go-to-definition |
| `vendor/lazarus` | Lazarus source tree (tested at commit `2dbcbed8`) | gitlab.com/freepascal.org/lazarus/lazarus | `pasls` CodeTools backend and the LCL for GUI apps |
| `vendor/fpcsrc` | FPC 3.2.2 `rtl` and `packages` (tag `release_3_2_2`) | gitlab.com/freepascal.org/fpc/source | lets the language server read standard units |

Set them up from the project root:

```sh
mkdir -p vendor && cd vendor
git clone --depth 1 --recurse-submodules https://github.com/genericptr/pascal-language-server.git pasls
git clone --depth 1 --filter=blob:none https://gitlab.com/freepascal.org/lazarus/lazarus.git lazarus
git clone --depth 1 --branch release_3_2_2 --filter=blob:none --sparse \
  https://gitlab.com/freepascal.org/fpc/source.git fpcsrc
(cd fpcsrc && git sparse-checkout set rtl packages)
(cd pasls/src && LAZARUSDIR="$PWD/../../lazarus" bash build_fpc.sh)
```

The language server binary ends up in `vendor/pasls/dist/aarch64-darwin/pasls`. Without it the
IDE still works; completion is simply off. Without `vendor/lazarus`, GUI programs cannot be
built.

The built app does not need this step: `packaging/bundle.sh` ships the needed parts of `vendor/`
inside `Pascoda.app` (about 170 MB). A copy that lacks them (a checkout without `vendor/`, a damaged
app) opens a "Some tools are missing" window when a feature needs one: its **Install in Terminal**
button types a pinned install command at the Terminal's prompt, and you press Enter to run it. The
tools go to `~/Library/Application Support/Pascoda/vendor`, which the IDE searches too; restart
Pascoda afterwards. Free Pascal itself is not bundled (`brew install fpc`).

## Build and run

```sh
cargo run -p pascoda-app -- tests/fixtures/debug.pas
cargo test
```

The tests compile Pascal programs, launch `lldb-dap` and `pasls`, and open a GUI window for a few
seconds, so they need the tools above. The first GUI build compiles the LCL into
`~/Library/Caches/Pascoda/lcl/` (about 20 seconds); later builds reuse it.

## Packaging

```sh
packaging/bundle.sh [output-dir]     # Pascoda.app, default dir ~/Library/Caches/Pascoda/dist
packaging/make-dmg.sh [output-dir]   # runs bundle.sh, then builds Pascoda.dmg (app + Applications shortcut)
```

Builds `Pascoda.app` (release build, `packaging/Pascoda.icns`, `Info.plist` with version,
copyright and minimum macOS taken from the sources) and signs it ad hoc, which runs on this Mac only.
The bundle carries the toolchain sources it needs (`Contents/Resources/vendor`: the LCL, CodeTools,
FPC sources and the `pasls` binary, about 160 MB), so it does not depend on this checkout. Free Pascal
itself (`/opt/homebrew/bin/fpc`) and Xcode's `lldb-dap` must be installed on the Mac.
`packaging/make-icon.sh` regenerates the icons (`make-icon.swift`, no dependencies): `Pascoda.icns` is
the P whose counter is a run triangle (used by the bundle), `Pascoda-classic.icns` is the plain P.

For distribution set `CODESIGN_IDENTITY="Developer ID Application: ..."` (hardened runtime and
timestamp), and `NOTARY_PROFILE=<keychain profile from notarytool store-credentials>` to also notarize
and staple. The bundle identifier `com.richardleurs.pascoda` is a placeholder. The notarization path
has not been run.

The app finds `vendor/` (`pasls`, `lazarus`, `fpcsrc`) in this order: the opened file's folder and its
parents, `$PASCODA_HOME` (a folder that contains `vendor/`), the bundle's `Contents/Resources`, and,
in debug builds only, the checkout the app was built in. It adds `/opt/homebrew/bin` and
`/usr/local/bin` to `PATH` at start, because an app opened from Finder does not get them.

## Rust dependencies

| Crate | Version | Used in | Purpose |
|---|---|---|---|
| `eframe` / `egui` | 0.37.0 | `pascoda-app` | window, immediate-mode UI |
| `muda` | 0.21.2 | `pascoda-app` | native macOS menu bar |
| `rfd` | 0.17.2 | `pascoda-app` | native Open dialog |
| `image` | 0.25.10 | `pascoda-app` | decoding the component icons |
| `portable-pty` | 0.9.0 | `pascoda-app` | pseudo-terminal for the integrated terminal |
| `notify` | 8.2.1 | `pascoda-app` | file-system watcher (FSEvents) for external-change detection |
| `vt100` | 0.18.1 | `pascoda-app` | terminal emulator (escape sequences, colours, scrollback) |
| `libc` | 0.2.191 | `pascoda-app` | hanging up and killing the terminal's shell |
| `objc2`, `objc2-app-kit`, `objc2-foundation` | 0.6.5 / 0.3.2 / 0.3.2 | `pascoda-app` | native controls, window chrome |
| `serde_json` | 1.0.154 | `pascoda-app`, `pascoda-dap`, `pascoda-lsp` | JSON for the DAP and LSP protocols |

`pascoda-fpc` and `pascoda-project` use only the standard library; `pascoda-forms` has no UI dependency. See `Cargo.lock` for the
transitive dependencies.

## Layout

| Crate | Role |
|---|---|
| `pascoda-app` | the IDE: UI, editor, debugger and language-server threads, theme, menu |
| `pascoda-fpc` | runs `fpc` with progress reporting, parses its diagnostics, signs binaries for debugging; `lcl` module builds GUI apps and `.app` bundles |
| `pascoda-dap` | Debug Adapter Protocol client for `lldb-dap` |
| `pascoda-lsp` | Language Server Protocol client for `pasls` |
| `pascoda-ai` | AI assistant backend: Anthropic Messages API client and OpenAI-compatible Chat Completions client (SSE streaming, tool use), the agent loop whose file changes are only proposals, Keychain key storage, and the Claude Code (subscription) driver that runs the user's own `claude` CLI; no UI |
| `pascoda-forms` | Lazarus `.lfm` form files: lossless parser, writer and text-patch editing (`FormDoc`); no UI dependency |
| `pascoda-project` | project model (minimal so far) |

Other folders: `packaging` (app bundle, DMG and icon scripts), `tests/fixtures` (sample Pascal programs), `docs/design` (design notes), `.claude` (project notes and agent definitions for Claude Code).

Demo projects in `examples/` (open the main file and use Build & Run; the Tests pane for the last one):

| Folder | Kind | What it shows |
|---|---|---|
| `gui-demo` | GUI | a three-form Notes app: menus, shortcuts, dialogs, status bar, settings and about forms (`notes.lpr`) |
| `console-demo` | console | `WordCount.lpr`, reads standard input; run `./WordCount < file` in the Terminal |
| `library-demo` | library | a unit (`Maths.pas`) and the program that uses it (`MathsDemo.lpr`) |
| `package-demo` | package | a `.lpk` package (`TextTools`) and a program that finds its unit through the package |
| `db-demo` | GUI | a window on `ideas.sqlite3` (SQLite): a parameterised search in a grid |
| `vlc-demo` | GUI | a video and audio player on VLC (needs VLC.app and the Pascoda VLC package, imported first: File > Import Component Package...) |
| `tests-demo` | unit tests and program | `TestsDemo.lpr` runs `CalcTests.pas` (FPCUnit tests for `Calc.pas`) with the console runner; one test fails on purpose. Cmd+U runs the same tests in the Tests pane |

## What works

**Editor**
- Tabs, file tree, line numbers, Pascal highlighting. Compiler errors and warnings are underlined.
- Hover, completion as you type (Ctrl+Space) and go-to-definition (F12), through the language server `pasls`.
- Preferences (Cmd+,): font (SF Mono, Menlo or Courier) and size, Tab width, line numbers, word wrap, and colours for each code token and the background, set separately for light and dark. Saved in `~/Library/Application Support/Pascoda/preferences`.
- Files changed by other programs (`claude`, git, an editor in the terminal) show up. Clean tabs reload silently. A tab with unsaved edits shows a banner with **Reload** or **Keep mine**.
- Closing a changed tab asks Save, Don't Save or Cancel. The last session's tabs and project come back at launch, and File > Open Recent lists recent projects and files.

**Build, run and test**
- Console and GUI programs (the LCL, Cocoa widgetset). GUI programs are packaged as a `.app`, and Build > Export makes a `.app` or `.dmg`. Demos are in `examples/`.
- Component packages: the Pascoda VLC and Pascoda Data packages are always available. File > Import Component Package... adds any other Lazarus `.lpk`. The Data package puts the database components (SQLdb and SQLite connections, queries, data sources) on a Data page, built from the vendored FPC database sources. Its components appear on palette pages named by the package. VLC: a video and audio player on the VLC library installed in `/Applications/VLC.app`, with a video panel in the form.
- Unit tests (FPCUnit): Cmd+U runs the project's test units. The **Tests** pane lists each result and its failure message. You can run one test, the failed ones or all of them, and "Go to test" opens the test's method. Runner is generated in `lib/tests/`.
- Bottom pane tabs: Problems, Output, Debug, Tests and Terminal. It switches to Output when a build or run starts, to Problems when a build fails, and to Debug at a breakpoint. It leaves your choice alone for a few seconds after you click a tab.

**Debugger** (through `lldb-dap`)
- Gutter breakpoints, continue, step, call stack, and a variables pane with type badges, expandable records, classes and arrays, and Pascal-formatted values.

**Terminal**
- Your login shell on a real pseudo-terminal, with colours, 5000 lines of scrollback and full-screen programs such as `vim`, `less` and `top`.
- Cmd+C, Cmd+V and Cmd+K work as in Terminal, and Ctrl+C interrupts. Menu shortcuts keep working.

**Form designer** (`File > New GUI Application`, then the `Form` toggle)
- Native macOS controls. The palette has 43 Lazarus components on four pages, and a tray for non-visual ones (timers, dialogs, menus, action lists).
- Select, move, resize and multi-select, with copy and paste, alignment guides, align and spacing commands, and tab order.
- Database forms: TDBGrid (Data Controls page) and a SQLdb connection editor (Edit Connection... in the inspector for a TSQLConnector: SQLite3 or PostgreSQL, with the driver unit added to uses). The data components come with the app (no import needed).
- Data modules (File > New Data Module): a window-less unit for non-visual components (timers, dialogs, menus, action lists), used by forms through their unit.
- Anchors, `Align`, `AutoSize`, `AnchorSide` and `BorderSpacing`. The canvas scrolls for large forms.
- Inline editors for menus and action lists. Double-click a control to create its event handler, and the `.pas` unit stays in step.

**Window**
- Xcode-style layout with a toolbar, navigator (file tree, outline, debug), tabs and breadcrumbs, and a status bar. Light and dark (View > Appearance), with native menus.

**AI assistant** (View > Show Assistant, Cmd+Opt+A, or the sparkle button)
- Providers: your Anthropic API key, your OpenAI key or a compatible local server (Ollama, LM Studio), or your installed Claude Code with its subscription. Keys are kept in the macOS Keychain, and the OpenAI base URL and model in plain settings files. The choice is remembered.
- It can see the open file, your selection, the build errors and a project overview. It reads project files and proposes edits and new files as diffs that you Apply or Reject. It builds only when you allow it.
- What it sends goes only to the provider you chose, and each chip can be switched off. It cannot reach files outside the project folder or run commands. Claude Code mode has no Bash, so no builds.
- **Allow edits this session** applies edits without asking. It is off at launch and never saved. Every change stays in the chat with a **Revert** button.

## Not done yet

- A per-project settings screen. The Delphi language mode exists in code, but has no setting.
- Developer ID signing and notarization (the code path exists but has never been run), and a real bundle identifier.
- Debugger: Pascal set values cannot be shown (lldb cannot read set types).
- The language server publishes no diagnostics, so build errors are the only ones shown.
- Designer: zoom, `ChildSizing`, and dragging menu items with the mouse.
- Assistant: the OpenAI provider has only been tested against mock servers, not the real OpenAI or a live local server. Cost figures, a terminal pane like the Claude CLI and chat history across restarts are missing.
- Most mouse and keyboard interaction has been tested only through unit tests and offscreen renders. Please report anything that misbehaves.
- Multilingual forms and interface.
- Design notes are in `docs/design/`.

## Things worth knowing

- `fpc` binaries are only linker-signed, and `lldb` cannot launch them until they are re-signed
  with the `get-task-allow` entitlement. The build does this automatically.
- fpc 3.2.2's Objective-C output fails with Apple's current linker. GUI builds pass
  `-k-ld_classic` as a workaround; Apple has deprecated that flag, so it may stop working.
- `pasls` crashes during `initialize` if the client sends no `clientInfo`; the client always does.
- Generated app bundles must not set `NSPrincipalClass`, or the LCL app crashes at startup.
- Generated GUI apps target macOS 14.0 or later.
- Apps opened from Finder get a minimal `PATH`; Pascoda extends it so `fpc` is found.
- The component icons are Lazarus's (modified LGPL); see `crates/pascoda-app/assets/`.

## Licences

GNU GENERAL PUBLIC LICENSE Version 3

