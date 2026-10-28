# Pascoda

A modern Pascal IDE with coding agents for macOS ARM | Pascal reimagined with AI

A modern IDE for Pascal (macOS ARM for now), written in Rust. It wraps the
Free Pascal compiler and aims at apps with a macOS-native look and feel.
Supports coding agents (OpenAI API and compatible, OpenAI Codex CLI, Claude Console API, Claude Code CLI for now).
OpenAI Codex CLI and Claude Code CLI (and other CLI's) also run in the Pascoda terminal inside the IDE.

> ** Status: early work in progress. The Pascoda IDE app builds and runs Pascal code ** 

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
| AI assistant | working | OpenAI-compatible and Anthropic Claude providers  |
| Packaging | partly | `Pascoda.app` and `Pascoda.dmg` build; the notarization path has not been run |

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
FPC sources and the `pasls` binary, about 360 MB), so it does not depend on this checkout. Free Pascal
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

Other folders: `packaging` (app bundle, DMG and icon scripts), `tests/fixtures` (sample Pascal programs), `examples/notes` (a three-form Notes
app: menus, shortcuts, dialogs, status bar, settings and about forms; open `examples/notes/notes.lpr`
and use Build & Run), `docs/design` (design notes), `.claude` (project notes and agent definitions
for Claude Code).

## What works

- Editor with tabs, a file tree, line numbers and Pascal syntax highlighting; hover tooltips,
  completion as you type (Ctrl+Space also works) and go-to-definition (F12) through the Pascal
  language server `pasls`; compiler errors and warnings are underlined in the code
- External-change detection: the project folder (and the folders of open files outside it) is
  watched with FSEvents, so edits made by other programs show up. This is what makes the terminal
  workflow work: run `claude` (Claude Code), git or any editor in the integrated terminal and the
  open tabs follow. A tab without unsaved edits reloads silently (the caret stays in place, a note
  in the status bar fades after 5 seconds); a tab with unsaved edits is not touched and shows a
  banner "unit1.pas changed on disk" with **Reload** (take the disk) and **Keep mine** (the next
  Save overwrites on purpose); a deleted file offers **Close tab** or **Keep (save to recreate)**;
  an unreadable (for example non-UTF-8) file keeps the tab and says so. The form designer follows
  its `.lfm` the same way. The file tree refreshes when files appear or disappear. Pascoda's own
  saves are recognised by their content and cause nothing. Ignored: `.git`, `lib/`, `target/`,
  `vendor/`, `*.o`, `*.ppu`, inside `.app` bundles, `.DS_Store`. Event storms (git checkout) are
  coalesced (200 ms quiet, at most 1 s wait).
- Build and run for console programs (Free Pascal, `-Mobjfpc`), with a progress bar in the toolbar
- A tabbed bottom pane (drag its top edge to resize): **Problems** (compiler errors and warnings;
  click one to jump to the line), **Debug** (continue/step buttons, variables; "Not debugging" when
  no session runs), **Output** (compiler and program output, Clear button) and **Terminal**. The
  pane switches by itself: to Output when a build starts or the program runs, to Problems when a
  build fails, to Debug when the program stops at a breakpoint. It does not switch within about
  2.5 seconds of your own click on a tab, and never away from the Terminal while you type in it.
- Integrated terminal: your login shell (`$SHELL`, else `/bin/zsh`) on a real pseudo-terminal, started
  in the project folder the first time the tab is shown; `TERM=xterm-256color`; ANSI 16/256/true
  colour, underline, inverse, 5000 lines of scrollback (mouse wheel), full-screen programs such as
  `vim`, `less` and `top`. Keys: everything goes to the shell while the terminal has focus (Tab,
  Ctrl+letters, arrows, Esc, Option+arrows for words); **Ctrl+C** interrupts, **Cmd+C** copies the
  mouse selection, **Cmd+V** pastes (bracketed paste when the shell asks for it), **Cmd+K**
  clears. Menu shortcuts (Cmd+R, Cmd+S, ...) keep working. "Clear terminal" wipes the screen and history; "Restart" (shown once the shell has exited) starts a
  fresh shell; quitting hangs up the shell and its jobs. Limits: no mouse reporting to programs, no
  tabs or split panes, bold is shown as the bright colour (no bold face), no IME composition, the
  16 colours are fixed (not configurable).
- Debugger (through `lldb-dap`): gutter breakpoints, continue, step over/into/out, call stack, and
  a variables pane like Xcode's with type badges, expandable records, classes and arrays, and
  Pascal-formatted values
- GUI programs built against the Lazarus LCL (Cocoa widgetset), packaged as a `.app`; a demo
  multi-form project is in `examples/notes`
- Visual form designer with live native macOS controls (`File > New GUI Application`, then the
  `Form` toggle):
  - a tabbed component palette with Lazarus's own icons: 43 components on the Standard, Additional,
    Common Controls and System pages; the unit's `uses` clause is kept in step
  - a component tray for non-visual components (timers, dialogs, action lists, image lists, menus)
  - place, move and resize; multi-select (also across containers), group resize, copy, cut and
    paste, duplicate, alignment guides, align and spacing commands, tab order
  - anchors, `Align`, `AutoSize`, `AnchorSide` relative anchoring and `BorderSpacing`; resize the
    form and watch the controls reflow
  - an inline menu editor for `TMainMenu` and `TPopupMenu`, and a list editor for `TActionList`
  - forms of any size scroll in the canvas; the native controls are clipped to it
  - edit properties, double-click for an event handler, and the `.pas` unit stays in step
- Xcode-style window: toolbar with grouped buttons and an activity viewer, navigator with file
  tree, tabs and breadcrumb bar, tabbed bottom pane (Problems, Output, Debug, Terminal), status bar; light and
  dark (View > Appearance); system fonts; native menu bar with an Edit menu
- AI assistant (View > Show Assistant, Cmd+Opt+A, or the sparkle button): a chat panel on the right
  that talks to a model: your own Anthropic API key, your own OpenAI key (or an OpenAI-compatible
  server such as Ollama), or your installed Claude Code and its subscription (see Providers below). It sees the open file, your selection,
  the current build errors and a compact project overview (file tree plus kind, name and `uses` of
  each Pascal file, about 10,000 characters at most); a fifth chip, "Open tabs" (off by default),
  adds the other tabs and the text of unsaved ones. Each chip can be switched off; the message
  says what was used ("with debug.pas, project, 1 error") and "What was sent" shows the exact text), can read project files, and *proposes* edits and new files as diffs that
  you Apply or Reject; it builds only if you Allow it. Answers are Markdown with code blocks
  (Copy, Insert at cursor, and Apply when a block is clearly the whole open file). Applying edits
  the open tab (so undo, save and the language server behave as for typing), reloads an open
  form designer from an edited `.lfm` and keeps the unit in step. Stop cancels the turn; the model
  picker offers Sonnet, Opus, Haiku and Fable.
  - Setup: create a key at console.anthropic.com (Settings > API keys), open the panel and paste
    it into "Connect to Claude". It is stored in the macOS Keychain (service `Pascoda`, account
    `anthropic`) and nowhere else; the gear button removes it. `ANTHROPIC_API_KEY` in the
    environment also works.
  - Privacy: what you type, the context you attach and any project file the assistant reads or
    edits are sent to Anthropic (`api.anthropic.com`) with your key. Nothing else is uploaded; it
    cannot reach files outside the project folder or run commands. With the OpenAI provider the
    same text goes to the configured endpoint instead: OpenAI, or your own server (the panel says
    which, and that a server on this Mac keeps it on this Mac).
  - Providers (gear button, or the radio rows on the setup card; the choice is remembered as plain
    text in `~/Library/Application Support/Pascoda/assistant-provider`):
    - **Anthropic API key**: as above.
    - **OpenAI API key**: your own OpenAI key, or any server that speaks the OpenAI Chat
      Completions API. Chat, tools, diff cards, chips, Stop, Clear and the usage line work as with
      the Anthropic key (the same tools and approval rules; the model decides what to call).
      Pick the provider, then fill the card: *Base URL* (default `https://api.openai.com/v1`),
      *Model* (free text: type a model name your account or server offers; Pascoda keeps no list
      and will not send until one is set) and the key. The key goes to the macOS Keychain
      (service `Pascoda`, account `openai`); `OPENAI_API_KEY` in the environment also works. The
      base URL and model are saved as plain JSON in
      `~/Library/Application Support/Pascoda/openai-settings`, never the key. For a local
      server, set the base URL to `http://localhost:11434/v1` (Ollama) or
      `http://localhost:1234/v1` (LM Studio): no key is needed when the URL is not
      `api.openai.com`. Tool use needs a model that supports function calling; a model without it
      still chats but cannot read or edit files. The key is sent to whatever base URL is set, so
      only point it at servers you trust. The model field sits under the message box. A rejected
      key (401) is forgotten and the card comes back.
    - **Claude Code (subscription)**: for people with a Claude subscription and no API key.
      Pascoda starts your own `claude` program (found on `PATH`, `~/.local/bin`, `~/.claude/local`,
      `/usr/local/bin`, `/opt/homebrew/bin`, or `PASCODA_CLAUDE_BIN`) in the project folder and
      talks to it over stdin/stdout. Install Claude Code, then log in once yourself: open a
      terminal, run `claude` and type `/login`. The panel shows what it found (version, logged in
      or not) and a "Check again" button. Pascoda never reads, copies or stores your Claude login,
      the Keychain entry of Claude Code or its settings files; it only runs the program (and
      removes `ANTHROPIC_API_KEY` from that program's environment so a key meant for the other
      provider cannot turn a subscription session into a billed one). The panel, diff cards,
      chips, Stop/Clear and "What was sent" work as with the API key. The model picker maps to
      Claude Code's aliases (sonnet, opus, haiku, fable). Claude Code also keeps its own session
      files for the conversation (named "Pascoda").
      Limits in this mode: the program may only Read, Glob, Grep (inside the project folder) and
      propose Edit, MultiEdit and Write; there is no Bash, web or MCP, so **it cannot run builds or
      commands** (build errors reach it in your next message through the "errors" chip).
      Every Edit, MultiEdit and Write is shown as a diff card first. **Apply** answers the program
      "allow" and Claude Code's own tool then writes the file (as in a normal Claude Code
      session, so it keeps track of the file's state); Pascoda reloads the file from the disk
      into the open tab or the form designer, keeps your caret in place, and opens a newly
      created file. If the write fails the card shows the tool's error. Because the tool checks
      the edit against the file on disk, a file with unsaved changes (an edited tab, or a form
      changed in the designer) must be saved first: the card says "Unsaved changes in X: save
      first" and offers **Save and apply**. If Claude Code then reports that the file changed
      since it read it, it re-reads and proposes the edit again. A stopped turn is interrupted
      and the same process goes on; a changed model restarts it with the same conversation.
  - Confirmation per provider: with the Anthropic and OpenAI providers Pascoda applies the
    approved edit itself (in the open tab, through the form designer, or by writing a file that
    is not open) and then tells the model; with Claude Code the tool writes the file after your
    Apply. Rejecting never touches a file.
  - **Allow edits this session** (switch above the message box, also in the gear settings): while
    it is on, proposed edits and new files are applied at once, with no click, for every
    provider. It is off at every launch and never saved; an orange "Edits applied automatically"
    chip shows in the panel header while it is on (a click on it switches it off), and the first
    time you turn it on a warning line appears. Only files inside the project folder are ever
    touched (anything else is refused), builds still ask, and for Claude Code a file with unsaved
    changes is saved automatically before the edit. **Risk:** the model can rewrite or add
    project files without you looking first; each change stays in the chat as an "Applied" card
    with its diff and a **Revert** button, but unsaved work in a file is written to disk before
    Claude Code edits it, and a Revert of a Claude Code edit rewrites the file from Pascoda. Use
    it in a project under version control.
  - **Revert** restores the exact previous text the way the edit was applied (the tab, the form
    designer and the unit that was brought in step with it, a direct write, or, for Claude Code,
    a write of the previous text to the disk and a reload of the tab; a file the edit created is
    deleted and its tab closed). It is available only while the file still has exactly the text
    the edit left; after any other change the button is disabled and says so. Claude Code will
    see a reverted or reloaded file as "changed since read" and re-read it.
- A packaged `Pascoda.app` and `Pascoda.dmg` that carry the toolchain sources they need


## Not done yet

- Per-project settings (the Delphi language mode exists in code but has no setting)
- A real Developer ID signature and notarization (the code path exists but has never been run),
  and a real bundle identifier
- Menu editor: dragging items with the mouse; designer: zoom, `ChildSizing`, a minimum canvas height
- Live diagnostics from the language server (`pasls` sends none; the build's errors are shown)
- Prompts for unsaved changes
- Debugger: set values cannot be shown (lldb cannot read Pascal set types)
- Assistant: Claude Code mode has no builds or commands; the OpenAI provider has not been run against the real OpenAI or a real local server (mock servers only), sends no token limit and no cost figure, and gives no token counts if the server omits them; and a terminal pane like the Claude CLI; the chat has no
  history across restarts, no cost figure (tokens only) and no bold text weight in answers
- Most of the window interaction (mouse, keyboard) has been checked through tests and offscreen
  renders only; please report anything that misbehaves
- Design notes are in `docs/design/`
- app windows/panels customization
- Build step for app, DMG in Pascoda
- Multilingual
- rewrite dependencies in Rust

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

