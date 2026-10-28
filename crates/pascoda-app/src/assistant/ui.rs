//! Drawing the assistant panel. Nothing here changes the app directly: clicks are collected as
//! `Act`s while drawing (drawing borrows the conversation) and carried out afterwards.

use super::*;
use crate::highlight;
use crate::style::{Icon, Palette, hline, icon_button, paint_icon};
use egui::epaint::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Frame, Key, Layout, Margin, RichText, Sense, Ui, vec2};
use markdown::{Block, Span};

pub(super) enum Act {
    Send,
    Stop,
    Clear,
    SaveKey,
    /// The model field of the OpenAI provider changed.
    SaveModel,
    /// The Claude model field changed or a suggestion was picked.
    SaveClaudeModel,
    /// The Codex model field changed.
    SaveCodexModel,
    RemoveKey,
    ToggleSettings,
    SetProvider(Provider),
    CheckCli,
    Copy(String),
    Insert(String),
    ApplyBlock(String),
    Decide(usize, bool),
    /// Save the card's file, then apply (Claude Code checks edits against the disk).
    SaveAndApply(usize),
    /// Take an auto-applied edit back.
    Revert(usize),
    /// The "Allow edits this session" switch.
    ToggleAuto,
    Suggest(String),
}

/// What the chips can offer right now.
struct ChipView {
    file: Option<String>,
    selection_lines: Option<usize>,
    errors: usize,
    /// Other open tabs.
    tabs: usize,
}

/// Facts about the IDE the panel needs, gathered before drawing.
struct View {
    active: Option<ActiveFile>,
    can_insert: bool,
    chips: ChipView,
    /// Files with changes the disk does not have (tabs, forms).
    dirty: Vec<PathBuf>,
    /// The backend writes approved edits itself and wants them saved first (Claude Code).
    writes: bool,
    /// Indexes of auto-applied cards whose file still has the text the edit left.
    revertable: Vec<usize>,
}

const AUTO_LABEL: &str = "Allow edits this session";

const SUGGESTIONS: [&str; 3] = [
    "Explain what this file does",
    "Fix the build errors",
    "Find bugs in the selected code",
];

impl App {
    fn assistant_view(&self, ctx: &egui::Context) -> View {
        let doc = self.docs.get(self.active);
        let selection = self.selection_range(ctx).and_then(|(a, b)| {
            let text = &doc?.text;
            Some(
                text.chars()
                    .skip(a)
                    .take(b - a)
                    .filter(|c| *c == '\n')
                    .count()
                    + 1,
            )
        });
        let mut dirty: Vec<PathBuf> = self
            .docs
            .iter()
            .filter(|d| d.dirty())
            .map(|d| d.path.clone())
            .collect();
        dirty.extend(
            self.designers
                .iter()
                .filter(|f| f.dirty())
                .map(|f| f.lfm_path.clone()),
        );
        let revertable = self
            .assistant
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| match it {
                Item::Card(c) if c.state == CardState::Applied && c.auto => {
                    let u = c.undo.as_ref()?;
                    (self.revert_text(&c.abs).as_deref() == Some(u.after.as_str())).then_some(i)
                }
                _ => None,
            })
            .collect();
        View {
            dirty,
            writes: self.assistant.backend_writes,
            revertable,
            active: doc.map(|d| ActiveFile {
                name: d
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                text: d.text.clone(),
            }),
            can_insert: doc.is_some() && !self.form_showing(),
            chips: ChipView {
                file: doc.map(|d| self.display_name(&d.path)),
                selection_lines: selection,
                tabs: self.docs.len().saturating_sub(usize::from(doc.is_some())),
                errors: self
                    .diagnostics
                    .iter()
                    .filter(|d| matches!(d.severity, Severity::Fatal | Severity::Error))
                    .count(),
            },
        }
    }

    pub(crate) fn assistant_panel(&mut self, ui: &mut Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        self.assistant.ctx = Some(ctx.clone());
        let view = self.assistant_view(&ctx);
        let mut acts = Vec::new();
        egui::Panel::right("assistant")
            .resizable(true)
            .default_size(380.0)
            .min_size(300.0)
            // leave the editor (or designer) room: at least 520 pt
            .max_size((ui.available_width() - 520.0).clamp(300.0, 720.0))
            .show_separator_line(true)
            .frame(Frame::new().fill(pal.chrome))
            .show(ui, |ui| {
                panel(ui, pal, &mut self.assistant, &view, &mut acts);
            });
        for act in acts {
            match act {
                Act::Send => {
                    self.assistant_send(&ctx);
                    self.assistant.focus_input = true;
                }
                Act::Stop => self.assistant.stop(),
                Act::Clear => self.assistant.clear(),
                Act::SaveKey => self.assistant.save_key(),
                Act::RemoveKey => self.assistant.remove_key(),
                Act::ToggleSettings => {
                    self.assistant.show_settings = !self.assistant.show_settings;
                    self.assistant.openai_edit = self.assistant.openai.clone();
                }
                Act::SaveModel => self.assistant.persist_openai(),
                Act::SaveClaudeModel => self.assistant.persist_claude_model(),
                Act::SaveCodexModel => self.assistant.persist_codex_model(),
                Act::SetProvider(p) => self.assistant.set_provider(p),
                Act::CheckCli => self.assistant.refresh_chosen_cli(&ctx),
                Act::Copy(t) => ctx.copy_text(t),
                Act::Insert(t) => self.insert_at_cursor(&ctx, &t),
                Act::ApplyBlock(code) => self.assistant_propose_block(&code),
                Act::Decide(i, ok) => self.assistant_decide(i, ok),
                Act::SaveAndApply(i) => self.assistant_save_and_apply(i),
                Act::Revert(i) => self.assistant_revert(i),
                Act::ToggleAuto => {
                    let on = !self.assistant.auto_edits;
                    self.assistant.set_auto_edits(on);
                }
                Act::Suggest(t) => {
                    self.assistant.input = t;
                    self.assistant.focus_input = true;
                }
            }
        }
    }
}

fn panel(ui: &mut Ui, pal: &Palette, a: &mut Assistant, view: &View, acts: &mut Vec<Act>) {
    egui::Panel::top("assistant-head")
        .exact_size(34.0)
        .show_separator_line(true)
        .frame(
            Frame::new()
                .fill(pal.sidebar)
                .inner_margin(Margin::symmetric(10, 0)),
        )
        .show(ui, |ui| header(ui, pal, a, acts));
    // The provider page is hidden until the gear is clicked; it then fills the panel.
    if a.show_settings {
        if a.ready() {
            settings(ui, pal, a, acts);
        } else {
            match a.provider {
                Provider::Api => key_card(ui, pal, a, acts),
                Provider::OpenAi => openai_card(ui, pal, a, acts),
                Provider::ClaudeCode | Provider::Codex => cli_card(ui, pal, a, acts),
            }
        }
        return;
    }
    if !a.settled() {
        // Still looking up the key / the CLI: show nothing rather than a card that disappears.
        Frame::new().fill(pal.editor).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
        });
        return;
    }
    if !a.ready() {
        not_connected(ui, pal);
        return;
    }
    egui::Panel::bottom("assistant-input")
        .resizable(false)
        .show_separator_line(true)
        .frame(
            Frame::new()
                .fill(pal.chrome)
                .inner_margin(Margin::symmetric(10, 8)),
        )
        .show(ui, |ui| input_area(ui, pal, a, view, acts));
    Frame::new().fill(pal.editor).show(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(8.0);
                if a.items.is_empty() {
                    empty_state(ui, pal, acts);
                    return;
                }
                let streaming = a.streaming();
                let n = a.items.len();
                for (i, item) in a.items.iter().enumerate() {
                    let live = streaming && i + 1 == n;
                    item_ui(ui, pal, i, item, live, view, acts);
                }
                if streaming && !matches!(a.items.last(), Some(Item::Assistant(_))) {
                    let waiting = matches!(a.items.last(), Some(Item::Card(c)) if c.state == CardState::Pending);
                    if !waiting {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(RichText::new("Working...").size(12.0).color(pal.text_tertiary));
                        });
                    }
                }
                ui.add_space(8.0);
            });
    });
}

fn header(ui: &mut Ui, pal: &Palette, a: &Assistant, acts: &mut Vec<Act>) {
    ui.horizontal_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        paint_icon(ui.painter(), r, Icon::Sparkle, pal.accent);
        ui.label(RichText::new("Assistant").size(13.0).color(pal.text));
        // Which agent answers: the provider that is selected right now.
        ui.label(
            RichText::new(format!("\u{b7} {}", a.provider.short_name()))
                .size(12.0)
                .color(pal.text_secondary),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, pal, Icon::Gear, true, a.show_settings)
                .on_hover_text("Provider and privacy")
                .clicked()
            {
                acts.push(Act::ToggleSettings);
            }
            let can_clear = !a.items.is_empty() && !a.streaming();
            if icon_button(ui, pal, Icon::Trash, can_clear, false)
                .on_hover_text("Clear the conversation")
                .clicked()
                && can_clear
            {
                acts.push(Act::Clear);
            }
        });
    });
}

const PRIVACY_CLI: &str = "What leaves this Mac: the messages you type, the project overview (file names, and the units each file uses), the file, selection, other tabs and build errors you attach with the chips above the input, and any file the assistant reads or edits in this project folder. They go to Anthropic through your own Claude Code (the `claude` program on this Mac, signed in with your Claude account). Pascoda only starts that program and talks to it; it never reads or stores your Claude login. Claude Code keeps its own session files for the conversation. Nothing else is uploaded, and no change is made to a file without your approval (unless you switch on \"Allow edits this session\"). \"What was sent\" under each message shows the exact text.";

const PRIVACY_CODEX: &str = "What leaves this Mac: the messages you type, the project overview (file names, and the units each file uses), the file, selection, other tabs and build errors you attach with the chips above the input, and any file the assistant reads or edits in this project folder. They go to OpenAI through your own Codex (the `codex` program on this Mac, signed in with your ChatGPT or OpenAI login). Pascoda only starts that program and talks to it; it never reads or stores your Codex login. Codex runs in its read-only mode here: it looks at your files, and every change it wants to make is shown to you first (unless you switch on \"Allow edits this session\"); commands that need permission are refused. Codex keeps its own session files and uses your Codex settings, including any tools you have set up there. \"What was sent\" under each message shows the exact text.";

const PRIVACY: &str = "What leaves this Mac: the messages you type, the project overview (file names, and the units each file uses), the file, selection, other tabs and build errors you attach with the chips above the input, and any file the assistant reads or edits in this project folder. They go to Anthropic (api.anthropic.com) with your key. Nothing else is uploaded, and the assistant never changes a file without your approval (unless you switch on \"Allow edits this session\") or runs a build without it. \"What was sent\" under each message shows the exact text.";

/// Where the OpenAI provider's text goes, for the privacy note.
pub(super) fn privacy_openai(s: &OpenAiSettings) -> String {
    let dest = if s.needs_key() {
        "OpenAI (api.openai.com) with your key".to_string()
    } else if pascoda_ai::openai::is_local(&s.base_url) {
        format!(
            "the server you configured at {}, which runs on this Mac, so nothing leaves it",
            s.base_url
        )
    } else {
        format!(
            "the server you configured at {} (not OpenAI), with your key if you saved one",
            s.base_url
        )
    };
    format!(
        "What leaves this Mac: the messages you type, the project overview (file names, and the units each file uses), the file, selection, other tabs and build errors you attach with the chips above the input, and any file the assistant reads or edits in this project folder. They go to {dest}. Nothing else is uploaded, and the assistant never changes a file without your approval (unless you switch on \"Allow edits this session\") or runs a build without it. \"What was sent\" under each message shows the exact text."
    )
}

/// The providers as radio rows, grouped by vendor (Claude, then OpenAI).
fn provider_choice(ui: &mut Ui, pal: &Palette, a: &Assistant, acts: &mut Vec<Act>) {
    let enabled = !a.streaming();
    let mut vendor = "";
    for p in Provider::ALL {
        if p.vendor() != vendor {
            vendor = p.vendor();
            ui.add_space(4.0);
            ui.label(
                RichText::new(vendor)
                    .size(12.0)
                    .strong()
                    .color(pal.text_secondary),
            );
        }
        let on = a.provider == p;
        let galley = ui.painter().layout_no_wrap(
            p.label().to_string(),
            FontId::proportional(12.5),
            if enabled { pal.text } else { pal.text_tertiary },
        );
        let (rect, resp) = ui.allocate_exact_size(
            vec2(ui.available_width(), 24.0),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, on, p.label())
        });
        if resp.hovered() && enabled {
            ui.painter().rect_filled(rect, 6.0, pal.row_hover);
        }
        let c = egui::pos2(rect.left() + 12.0, rect.center().y);
        ui.painter()
            .circle_stroke(c, 6.0, egui::Stroke::new(1.0, pal.text_tertiary));
        if on {
            ui.painter().circle_filled(c, 3.5, pal.accent);
        }
        ui.painter().galley(
            egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text,
        );
        if resp.clicked() && !on {
            acts.push(Act::SetProvider(p));
        }
    }
}

/// Shown instead of the chat while Claude Code is not usable: not found, still looking, or not
/// logged in.
fn cli_card(ui: &mut Ui, pal: &Palette, a: &mut Assistant, acts: &mut Vec<Act>) {
    Frame::new().fill(pal.editor).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                        paint_icon(ui.painter(), r, Icon::Sparkle, pal.accent);
                    });
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(format!("Connect to {}", a.provider.vendor()))
                                .size(17.0)
                                .color(pal.text),
                        );
                    });
                    ui.add_space(8.0);
                    provider_choice(ui, pal, a, acts);
                    ui.add_space(10.0);
                    cli_status_card(ui, pal, a, acts);
                    ui.add_space(12.0);
                    privacy_box(
                        ui,
                        pal,
                        if a.provider == Provider::Codex {
                            PRIVACY_CODEX
                        } else {
                            PRIVACY_CLI
                        },
                    );
                });
            });
    });
}

fn privacy_box(ui: &mut Ui, pal: &Palette, text: &str) {
    Frame::new()
        .fill(pal.sidebar)
        .corner_radius(8)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Privacy").size(13.0).color(pal.text));
            ui.add_space(2.0);
            ui.label(RichText::new(text).size(13.0).color(pal.text_secondary));
        });
}

/// What the app knows about the `claude` or `codex` program, and what to do about it.
fn cli_status_card(ui: &mut Ui, pal: &Palette, a: &Assistant, acts: &mut Vec<Act>) {
    let codex = a.provider == Provider::Codex;
    let (name, program, state) = if codex {
        ("Codex", "codex", &a.codex)
    } else {
        ("Claude Code", "claude", &a.cli)
    };
    let (install, login_how) = if codex {
        (
            "Pascoda uses the Codex program installed on this Mac, so your ChatGPT or OpenAI login works without a key in Pascoda. Install it with `npm i -g @openai/codex` (or `brew install codex`), then log in once: open a terminal and run `codex login`.",
            "Open a terminal, run `codex login` and sign in. Then come back and check again.",
        )
    } else {
        (
            "Pascoda uses the Claude Code program installed on this Mac, so your Claude subscription works without an API key. Install it from claude.com/claude-code, then log in once: open a terminal, run `claude`, and type /login.",
            "Open a terminal, run `claude` once, type /login and sign in with your Claude account. Then come back and check again.",
        )
    };
    Frame::new()
        .fill(pal.sidebar)
        .corner_radius(8)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let note = |ui: &mut Ui, t: &str, color: Color32| {
                ui.label(RichText::new(t).size(13.0).color(color));
            };
            match state {
                CliState::Unchecked | CliState::Checking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(13.0));
                        note(ui, &format!("Looking for {name}..."), pal.text_secondary);
                    });
                }
                CliState::Missing => {
                    ui.label(
                        RichText::new(format!("{name} was not found"))
                            .size(13.0)
                            .color(pal.error),
                    );
                    ui.add_space(2.0);
                    note(ui, install, pal.text_secondary);
                    ui.add_space(6.0);
                    if text_button(ui, pal, "Check again", true, true).clicked() {
                        acts.push(Act::CheckCli);
                    }
                }
                CliState::Found(s) => {
                    let v = s.version.as_deref().unwrap_or("(version unknown)");
                    if s.logged_in == Some(false) {
                        ui.label(
                            RichText::new(format!("{name} is not logged in"))
                                .size(13.0)
                                .color(pal.error),
                        );
                        ui.add_space(2.0);
                        note(ui, &format!("Found {name} {v}. {login_how}"), pal.text_secondary);
                        ui.add_space(6.0);
                        if text_button(ui, pal, "Check again", true, true).clicked() {
                            acts.push(Act::CheckCli);
                        }
                    } else {
                        ui.label(RichText::new(format!("{name} {v}")).size(13.0).color(pal.text));
                        let login = match s.logged_in {
                            Some(true) => format!("Logged in ({name} keeps the login itself)."),
                            _ => format!("Login state unknown; if a message fails, run `{program} login` in a terminal."),
                        };
                        note(ui, &login, pal.text_secondary);
                        note(ui, &s.binary.display().to_string(), pal.text_secondary);
                        ui.add_space(6.0);
                        if text_button(ui, pal, "Check again", false, true).clicked() {
                            acts.push(Act::CheckCli);
                        }
                    }
                }
            }
        });
}

fn key_card(ui: &mut Ui, pal: &Palette, a: &mut Assistant, acts: &mut Vec<Act>) {
    Frame::new().fill(pal.editor).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                    paint_icon(ui.painter(), r, Icon::Sparkle, pal.accent);
                });
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Connect to Claude").size(17.0).color(pal.text));
                });
                ui.add_space(8.0);
                provider_choice(ui, pal, a, acts);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Pascoda uses your own Claude Console API key. Create one at console.anthropic.com (Settings > API keys) and paste it here.",
                    )
                    .size(13.0)
                    .color(pal.text_secondary),
                );
                ui.add_space(10.0);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut a.key_input)
                        .password(true)
                        .hint_text("sk-ant-...")
                        .desired_width(f32::INFINITY),
                );
                field.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::PasswordInput, true, "API key")
                });
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if let Some(m) = &a.key_message {
                    ui.add_space(4.0);
                    ui.label(RichText::new(m).size(13.0).color(pal.error));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let can = !a.key_input.trim().is_empty();
                    if text_button(ui, pal, "Save to Keychain", true, can).clicked() && can
                        || enter && can
                    {
                        acts.push(Act::SaveKey);
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("The key is kept in the macOS Keychain (service \"Pascoda\"). It is never written to a file or shown again. ANTHROPIC_API_KEY in the environment also works.")
                        .size(13.0)
                        .color(pal.text_secondary),
                );
                ui.add_space(12.0);
                Frame::new()
                    .fill(pal.sidebar)
                    .corner_radius(8)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Privacy").size(13.0).color(pal.text));
                        ui.add_space(2.0);
                        ui.label(RichText::new(PRIVACY).size(13.0).color(pal.text_secondary));
                    });
            });
        });
    });
}

/// The OpenAI provider's fields: endpoint, model and key, with Save and Remove key.
fn openai_fields(ui: &mut Ui, pal: &Palette, a: &mut Assistant, acts: &mut Vec<Act>) {
    let label = |ui: &mut Ui, t: &str| {
        ui.label(RichText::new(t).size(13.0).color(pal.text_secondary));
    };
    label(ui, "Base URL");
    let url = ui.add(
        egui::TextEdit::singleline(&mut a.openai_edit.base_url)
            .hint_text(pascoda_ai::openai::DEFAULT_OPENAI_BASE_URL)
            .desired_width(f32::INFINITY),
    );
    url.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "OpenAI base URL"));
    ui.add_space(4.0);
    label(ui, "Model");
    let model = ui.add(
        egui::TextEdit::singleline(&mut a.openai_edit.model)
            .hint_text("model name, as your provider lists it")
            .desired_width(f32::INFINITY),
    );
    model.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::TextInput, true, "OpenAI model name")
    });
    ui.horizontal_wrapped(|ui| {
        for m in pascoda_ai::openai::SUGGESTED_MODELS {
            if ui
                .selectable_label(a.openai_edit.model == m, RichText::new(m).size(13.0))
                .clicked()
            {
                a.openai_edit.model = m.into();
            }
        }
    });
    ui.add_space(4.0);
    let needs = pascoda_ai::openai::needs_key(&a.openai_edit.base_url);
    label(
        ui,
        if needs {
            "API key"
        } else {
            "API key (optional here)"
        },
    );
    let key = ui.add(
        egui::TextEdit::singleline(&mut a.key_input)
            .password(true)
            .hint_text(if needs {
                "sk-..."
            } else {
                "not needed for most local servers"
            })
            .desired_width(f32::INFINITY),
    );
    key.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::PasswordInput, true, "OpenAI API key")
    });
    let enter = key.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    // Where the key stands now: shown on the card itself, so Save visibly did something.
    ui.add_space(4.0);
    let (status, stored) = match a.openai_key_state {
        KeyState::Keychain => ("API key: stored in the macOS Keychain.", true),
        KeyState::Env => ("API key: from OPENAI_API_KEY in the environment.", true),
        _ if needs => ("No API key yet.", false),
        _ => ("No API key (not needed for this server).", false),
    };
    // The normal text colour (white in dark mode) once a key is stored, not green.
    ui.label(RichText::new(status).size(13.0).color(if stored {
        pal.text
    } else {
        pal.text_secondary
    }));
    if let Some(m) = &a.key_message {
        ui.add_space(2.0);
        ui.label(RichText::new(m).size(13.0).color(pal.error));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let can = !a.key_input.trim().is_empty() || !needs;
        if text_button(ui, pal, "Save", true, can).clicked() && can || enter && can {
            acts.push(Act::SaveKey);
        }
        if a.openai_key_state == KeyState::Keychain
            && text_button(ui, pal, "Remove key", false, true).clicked()
        {
            acts.push(Act::RemoveKey);
        }
    });
    ui.add_space(4.0);
    ui.label(
        RichText::new("The key is kept in the macOS Keychain (service \"Pascoda\", account \"openai\"); OPENAI_API_KEY in the environment also works. The base URL and model are saved in a plain settings file, never the key. For a local server use e.g. http://localhost:11434/v1 (Ollama) or http://localhost:1234/v1 (LM Studio).")
            .size(13.0)
            .color(pal.text_secondary),
    );
}

/// Shown instead of the chat while the OpenAI provider has no key.
fn openai_card(ui: &mut Ui, pal: &Palette, a: &mut Assistant, acts: &mut Vec<Act>) {
    Frame::new().fill(pal.editor).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                    paint_icon(ui.painter(), r, Icon::Sparkle, pal.accent);
                });
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Connect to OpenAI").size(17.0).color(pal.text));
                });
                ui.add_space(8.0);
                provider_choice(ui, pal, a, acts);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Pascoda uses your own OpenAI API key (platform.openai.com), or any server that speaks the same API: change the base URL.",
                    )
                    .size(13.0)
                    .color(pal.text_secondary),
                );
                ui.add_space(10.0);
                openai_fields(ui, pal, a, acts);
                ui.add_space(12.0);
                Frame::new()
                    .fill(pal.sidebar)
                    .corner_radius(8)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Privacy").size(13.0).color(pal.text));
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(privacy_openai(&a.openai_edit))
                                .size(13.0)
                                .color(pal.text_secondary),
                        );
                    });
            });
        });
    });
}

/// Shown instead of the chat while no provider is usable and the settings are closed.
fn not_connected(ui: &mut Ui, pal: &Palette) {
    Frame::new().fill(pal.editor).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
            paint_icon(ui.painter(), r, Icon::Sparkle, pal.text_secondary);
            ui.add_space(4.0);
            ui.label(
                RichText::new("The assistant is not connected")
                    .size(15.0)
                    .color(pal.text_secondary),
            );
            ui.label(
                RichText::new("Click the gear to choose a provider.")
                    .size(13.0)
                    .color(pal.text_secondary),
            );
        });
    });
}

fn settings(ui: &mut Ui, pal: &Palette, a: &mut Assistant, acts: &mut Vec<Act>) {
    Frame::new().fill(pal.sidebar).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                Frame::new()
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("Provider")
                                .size(13.0)
                                .color(pal.text_secondary),
                        );
                        provider_choice(ui, pal, a, acts);
                        ui.add_space(6.0);
                        match a.provider {
                            Provider::Api => {
                                let (text, removable) = match a.key {
                                    KeyState::Keychain => {
                                        ("API key: stored in the macOS Keychain.", true)
                                    }
                                    KeyState::Env => (
                                        "API key: from ANTHROPIC_API_KEY in the environment.",
                                        false,
                                    ),
                                    _ => ("No API key.", false),
                                };
                                ui.label(RichText::new(text).size(13.0).color(pal.text));
                                if let Some(m) = &a.key_message {
                                    ui.label(RichText::new(m).size(13.0).color(pal.error));
                                }
                                if removable {
                                    ui.add_space(4.0);
                                    if text_button(ui, pal, "Remove key", false, true).clicked() {
                                        acts.push(Act::RemoveKey);
                                    }
                                }
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(PRIVACY).size(13.0).color(pal.text_secondary),
                                );
                            }
                            Provider::OpenAi => {
                                openai_fields(ui, pal, a, acts);
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(privacy_openai(&a.openai))
                                        .size(13.0)
                                        .color(pal.text_secondary),
                                );
                            }
                            Provider::ClaudeCode | Provider::Codex => {
                                cli_status_card(ui, pal, a, acts);
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(if a.provider == Provider::Codex {
                                        PRIVACY_CODEX
                                    } else {
                                        PRIVACY_CLI
                                    })
                                    .size(13.0)
                                    .color(pal.text_secondary),
                                );
                            }
                        }
                    });
            });
    });
}

fn empty_state(ui: &mut Ui, pal: &Palette, acts: &mut Vec<Act>) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
        paint_icon(ui.painter(), r, Icon::Sparkle, pal.text_tertiary);
        ui.add_space(4.0);
        ui.label(
            RichText::new("Ask about your Pascal code")
                .size(15.0)
                .color(pal.text_secondary),
        );
        ui.label(
            RichText::new("The assistant receives the attached context items as input.")
                .size(12.0)
                .color(pal.text_tertiary),
        );
        ui.add_space(14.0);
        for s in SUGGESTIONS {
            if text_button(ui, pal, s, false, true).clicked() {
                acts.push(Act::Suggest(s.to_string()));
            }
            ui.add_space(2.0);
        }
    });
}

// ---- the input ------------------------------------------------------------------------

fn input_area(ui: &mut Ui, pal: &Palette, a: &mut Assistant, view: &View, acts: &mut Vec<Act>) {
    // The session switch for edits, and the warning the first time it goes on.
    ui.horizontal(|ui| {
        if switch(ui, pal, AUTO_LABEL, a.auto_edits) {
            acts.push(Act::ToggleAuto);
        }
    });
    ui.add_space(4.0);
    // Context chips.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        if let Some(name) = &view.chips.file {
            chip(ui, pal, name, &mut a.chips.file);
        }
        chip(ui, pal, "Project", &mut a.chips.project);
        if view.chips.tabs > 0 {
            let n = view.chips.tabs;
            let label = format!("Open tabs, {n}");
            chip(ui, pal, &label, &mut a.chips.tabs);
        }
        if let Some(n) = view.chips.selection_lines {
            let label = format!("Selection, {n} line{}", if n == 1 { "" } else { "s" });
            chip(ui, pal, &label, &mut a.chips.selection);
        }
        if view.chips.errors > 0 {
            let n = view.chips.errors;
            let label = format!("{n} error{}", if n == 1 { "" } else { "s" });
            chip(ui, pal, &label, &mut a.chips.errors);
        }
    });
    ui.add_space(4.0);

    let id = a.input_id;
    let streaming = a.streaming();
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    if focused && !streaming {
        // Enter sends; Shift+Enter (and Alt+Enter) stay a new line.
        let (pressed, mods) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers));
        if pressed && !mods.shift && !mods.alt {
            ui.input_mut(|i| i.consume_key(mods, Key::Enter));
            acts.push(Act::Send);
        }
    }
    Frame::new()
        .fill(pal.editor)
        .stroke(egui::Stroke::new(
            1.0,
            if focused { pal.accent } else { pal.separator },
        ))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .id_salt("assistant-input-scroll")
                .show(ui, |ui| {
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut a.input)
                            .id(id)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .frame(Frame::NONE)
                            .hint_text(if a.provider == Provider::Codex {
                                "Ask Codex...  Enter sends, Shift+Enter adds a line"
                            } else if a.provider == Provider::OpenAi {
                                "Ask the assistant...  Enter sends, Shift+Enter adds a line"
                            } else {
                                "Ask Claude...  Enter sends, Shift+Enter adds a line"
                            }),
                    );
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Message")
                    });
                    if a.focus_input {
                        r.request_focus();
                        a.focus_input = false;
                    }
                });
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!streaming, |ui| {
                    if a.provider == Provider::Codex {
                        // Empty = Codex's own default model; any name Codex accepts can be typed.
                        if model_field(
                            ui,
                            &mut a.codex_model,
                            &[],
                            "codex-model-list",
                            "default model",
                        ) {
                            acts.push(Act::SaveCodexModel);
                        }
                        return;
                    }
                    if a.provider == Provider::OpenAi {
                        let suggestions: Vec<(String, String)> =
                            pascoda_ai::openai::SUGGESTED_MODELS
                                .iter()
                                .map(|m| (m.to_string(), m.to_string()))
                                .collect();
                        if model_field(
                            ui,
                            &mut a.openai.model,
                            &suggestions,
                            "openai-model-list",
                            "model name",
                        ) {
                            acts.push(Act::SaveModel);
                        }
                        return;
                    }
                    let mut suggestions: Vec<(String, String)> = pascoda_ai::MODELS
                        .iter()
                        .map(|(m, n)| (m.to_string(), n.to_string()))
                        .collect();
                    suggestions.extend(a.extra_models.iter().map(|m| (m.clone(), m.clone())));
                    if model_field(
                        ui,
                        &mut a.model,
                        &suggestions,
                        "assistant-model",
                        "model name",
                    ) {
                        acts.push(Act::SaveClaudeModel);
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if streaming {
                        if send_button(ui, pal, true, true)
                            .on_hover_text("Stop")
                            .clicked()
                        {
                            acts.push(Act::Stop);
                        }
                    } else {
                        let can = !a.input.trim().is_empty();
                        if send_button(ui, pal, false, can)
                            .on_hover_text("Send (Enter)")
                            .clicked()
                            && can
                        {
                            acts.push(Act::Send);
                        }
                    }
                });
            });
        });
    ui.add_space(3.0);
    ui.label(
        RichText::new(usage_line(&a.usage))
            .size(11.0)
            .color(pal.text_tertiary),
    );
}

/// The model chooser: a text field for any model name plus a small dropdown of suggestions
/// (`(id, label)`). True when the model changed.
fn model_field(
    ui: &mut Ui,
    model: &mut String,
    suggestions: &[(String, String)],
    list_id: &str,
    hint: &str,
) -> bool {
    let mut changed = false;
    let r = ui.add(
        egui::TextEdit::singleline(model)
            .hint_text(hint)
            .font(FontId::proportional(12.0))
            .desired_width(150.0),
    );
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Model"));
    changed |= r.changed();
    if suggestions.is_empty() {
        return changed;
    }
    let mut picked = None;
    let list = egui::ComboBox::from_id_salt(list_id)
        .selected_text("")
        .width(14.0)
        .show_ui(ui, |ui| {
            for (id, label) in suggestions {
                if ui.selectable_label(model == id, label).clicked() {
                    picked = Some(id.clone());
                }
            }
        });
    list.response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Suggested models"));
    if let Some(id) = picked {
        *model = id;
        changed = true;
    }
    changed
}

/// `12,345 tokens in, 987 out` (and how many of the input came from the cache).
pub fn usage_line(u: &Usage) -> String {
    let n = |v: u64| {
        let s = v.to_string();
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
        }
        out
    };
    let input = u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens;
    if input + u.output_tokens == 0 {
        return "No tokens used yet".into();
    }
    let mut s = format!("{} tokens in, {} out", n(input), n(u.output_tokens));
    if u.cache_read_input_tokens > 0 {
        s.push_str(&format!(" ({} cached)", n(u.cache_read_input_tokens)));
    }
    s
}

/// A small on/off switch with a label; true when clicked.
fn switch(ui: &mut Ui, pal: &Palette, label: &str, on: bool) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(12.0),
        if on { pal.text } else { pal.text_secondary },
    );
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 40.0, 20.0), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, enabled, on, label));
    let track = egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 2.0), vec2(28.0, 16.0));
    ui.painter()
        .rect_filled(track, 8.0, if on { pal.warning } else { pal.separator });
    let knob_x = if on {
        track.right() - 8.0
    } else {
        track.left() + 8.0
    };
    ui.painter()
        .circle_filled(egui::pos2(knob_x, track.center().y), 6.0, Color32::WHITE);
    ui.painter().galley(
        rect.left_center() + vec2(36.0, -galley.size().y / 2.0),
        galley,
        if on { pal.text } else { pal.text_secondary },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn chip(ui: &mut Ui, pal: &Palette, label: &str, on: &mut bool) {
    let color = if *on { pal.text } else { pal.text_tertiary };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::proportional(11.5), color);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 26.0, 20.0), Sense::click());
    let enabled = ui.is_enabled();
    let on_now = *on;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, on_now, label));
    let fill = if *on {
        pal.row_selected
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect(
        rect,
        10.0,
        if resp.hovered() && !*on {
            pal.row_hover
        } else {
            fill
        },
        egui::Stroke::new(1.0, pal.separator),
        egui::StrokeKind::Inside,
    );
    let dot = rect.left_center() + vec2(10.0, 0.0);
    if *on {
        ui.painter().circle_filled(dot, 3.0, pal.accent);
    } else {
        ui.painter()
            .circle_stroke(dot, 3.0, egui::Stroke::new(1.0, pal.text_tertiary));
    }
    ui.painter().galley(
        rect.left_center() + vec2(19.0, -galley.size().y / 2.0),
        galley,
        color,
    );
    if resp.clicked() {
        *on = !*on;
    }
    resp.on_hover_text("Attach to the next message");
}

fn send_button(ui: &mut Ui, pal: &Palette, stop: bool, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    let label = if stop { "Stop" } else { "Send" };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let fill = if enabled {
        pal.accent
    } else {
        pal.row_selected
    };
    ui.painter().circle_filled(rect.center(), 12.0, fill);
    let ink = if enabled {
        Color32::WHITE
    } else {
        pal.text_tertiary
    };
    paint_icon(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), vec2(12.0, 12.0)),
        if stop { Icon::Stop } else { Icon::Send },
        ink,
    );
    resp
}

/// A small rounded text button (about 22 points high).
fn text_button(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    primary: bool,
    enabled: bool,
) -> egui::Response {
    let ink = if !enabled {
        pal.text_tertiary
    } else if primary {
        Color32::WHITE
    } else {
        pal.text
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::proportional(12.0), ink);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 20.0, 22.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let fill = match (primary, enabled, resp.hovered()) {
        (true, true, true) => pal.accent.gamma_multiply(0.85),
        (true, true, false) => pal.accent,
        (_, _, true) if enabled => pal.row_hover,
        _ => pal.chrome,
    };
    let stroke = if primary && enabled {
        egui::Stroke::NONE
    } else {
        egui::Stroke::new(1.0, pal.separator)
    };
    ui.painter()
        .rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, ink);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

// ---- conversation items ----------------------------------------------------------------

fn item_ui(
    ui: &mut Ui,
    pal: &Palette,
    i: usize,
    item: &Item,
    live: bool,
    view: &View,
    acts: &mut Vec<Act>,
) {
    match item {
        Item::User { text, sent, chips } => {
            Frame::new()
                .fill(pal.sidebar)
                .stroke(egui::Stroke::new(1.0, pal.separator))
                .corner_radius(10)
                .inner_margin(Margin::symmetric(10, 8))
                .outer_margin(Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(text).size(13.0).color(pal.text)).wrap());
                    if !chips.is_empty() {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!("with {}", chips.join(", ")))
                                .size(11.0)
                                .color(pal.text_tertiary),
                        );
                    }
                    egui::CollapsingHeader::new(
                        RichText::new("What was sent")
                            .size(11.0)
                            .color(pal.text_tertiary),
                    )
                    .id_salt(("assistant-sent", i))
                    .show(ui, |ui| {
                        egui::ScrollArea::both()
                            .id_salt(("assistant-sent-scroll", i))
                            .max_height(260.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(sent)
                                            .monospace()
                                            .size(11.0)
                                            .color(pal.text_secondary),
                                    )
                                    .extend(),
                                );
                            });
                    });
                });
        }
        Item::Assistant(text) => {
            Frame::new()
                .inner_margin(Margin::symmetric(14, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    markdown_ui(ui, pal, text, live, i, view, acts);
                });
        }
        Item::Tool(t) => tool_row(ui, pal, t),
        Item::Card(c) => card(ui, pal, i, c, view, acts),
        Item::Error(e) => {
            Frame::new()
                .fill(pal.error.gamma_multiply(0.10))
                .stroke(egui::Stroke::new(1.0, pal.error.gamma_multiply(0.5)))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(10, 6))
                .outer_margin(Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(e).size(12.0).color(pal.error)).wrap());
                });
        }
        Item::Note(n) => {
            Frame::new()
                .inner_margin(Margin::symmetric(14, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(n).size(11.5).color(pal.text_tertiary))
                            .wrap(),
                    );
                });
        }
    }
}

fn tool_row(ui: &mut Ui, pal: &Palette, t: &ToolRow) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let dot = rect.left_center() + vec2(20.0, 0.0);
    let p = ui.painter();
    let (ink, reason) = match &t.state {
        ToolState::Running => {
            p.circle_stroke(dot, 4.0, egui::Stroke::new(1.4, pal.accent));
            (pal.text_secondary, None)
        }
        ToolState::Done => {
            let s = egui::Stroke::new(1.5, pal.ok);
            p.line_segment([dot + vec2(-3.5, 0.0), dot + vec2(-1.0, 3.0)], s);
            p.line_segment([dot + vec2(-1.0, 3.0), dot + vec2(4.0, -3.5)], s);
            (pal.text_secondary, None)
        }
        ToolState::Failed(why) => {
            let s = egui::Stroke::new(1.5, pal.error);
            p.line_segment([dot + vec2(-3.0, -3.0), dot + vec2(3.0, 3.0)], s);
            p.line_segment([dot + vec2(-3.0, 3.0), dot + vec2(3.0, -3.0)], s);
            (pal.text_secondary, Some(why.as_str()))
        }
    };
    let galley = p.layout_no_wrap(t.label.clone(), FontId::proportional(12.0), ink);
    let label_w = galley.size().x;
    p.galley(
        rect.left_center() + vec2(34.0, -galley.size().y / 2.0),
        galley,
        ink,
    );
    if let Some(why) = reason {
        let x = rect.left() + 34.0 + label_w + 8.0;
        let room = (rect.right() - x - 8.0).max(0.0);
        let text = p.layout(why.to_string(), FontId::proportional(11.0), pal.error, room);
        // One line: the layout wraps at `room`, the clip keeps the first row.
        let clip = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(room, rect.height()));
        ui.painter_at(clip)
            .galley(egui::pos2(x, rect.center().y - 7.0), text, pal.error);
    }
    if !t.output.is_empty() {
        let shown: String = t.output.chars().take(700).collect();
        resp.on_hover_text(shown);
    }
}

// ---- proposals -------------------------------------------------------------------------

const MAX_DIFF_ROWS: usize = 60;
const DIFF_ROW_H: f32 = 15.0;

fn card(ui: &mut Ui, pal: &Palette, idx: usize, c: &Card, view: &View, acts: &mut Vec<Act>) {
    let build = c.change == Change::Build;
    let verb = match &c.change {
        Change::Build => "Build",
        Change::Create { .. } => "Create",
        Change::Edit { .. } if c.local => "Replace",
        Change::Edit { .. } => "Edit",
    };
    Frame::new()
        .fill(pal.chrome)
        .stroke(egui::Stroke::new(
            1.0,
            if c.state == CardState::Pending {
                pal.accent.gamma_multiply(0.7)
            } else {
                pal.separator
            },
        ))
        .corner_radius(8)
        .inner_margin(Margin::same(8))
        .outer_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(verb).size(12.0).color(pal.text_secondary));
                if !build {
                    ui.label(
                        RichText::new(&c.path)
                            .monospace()
                            .size(12.0)
                            .color(pal.text),
                    );
                }
                if let Ok((_, (add, del))) = &c.preview
                    && !build
                {
                    ui.label(RichText::new(format!("+{add}")).size(11.5).color(pal.ok));
                    ui.label(RichText::new(format!("-{del}")).size(11.5).color(pal.error));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (text, color) = match &c.state {
                        CardState::Pending => ("", pal.text),
                        CardState::Writing => ("Writing...", pal.text_secondary),
                        CardState::Reverted => ("Reverted", pal.text_secondary),
                        CardState::Applied if build => ("Allowed", pal.ok),
                        CardState::Applied => ("Applied", pal.ok),
                        CardState::Rejected if build => ("Denied", pal.text_secondary),
                        CardState::Rejected => ("Rejected", pal.text_secondary),
                        CardState::Failed(_) => ("Not applied", pal.error),
                        CardState::Cancelled => ("No answer", pal.text_tertiary),
                    };
                    ui.label(RichText::new(text).size(11.5).color(color));
                });
            });
            if let CardState::Failed(why) = &c.state {
                ui.add(egui::Label::new(RichText::new(why).size(11.5).color(pal.error)).wrap());
            }
            if c.auto && matches!(c.state, CardState::Applied | CardState::Reverted) {
                // An automatic edit keeps its diff, and can be taken back.
                if let Ok((rows, _)) = &c.preview {
                    ui.add_space(4.0);
                    diff_view(ui, pal, rows);
                }
                if c.state == CardState::Applied && c.undo.is_some() {
                    ui.add_space(4.0);
                    let can = view.revertable.contains(&idx);
                    ui.horizontal(|ui| {
                        if text_button(ui, pal, "Revert", false, can).clicked() && can {
                            acts.push(Act::Revert(idx));
                        }
                        if !can {
                            ui.label(
                                RichText::new("The file changed after this edit.")
                                    .size(11.5)
                                    .color(pal.text_tertiary),
                            );
                        }
                    });
                }
            }
            if c.state != CardState::Pending {
                return;
            }
            ui.add_space(4.0);
            if build {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "The assistant wants to compile the project and read the result.",
                        )
                        .size(12.0)
                        .color(pal.text_secondary),
                    )
                    .wrap(),
                );
            } else {
                match &c.preview {
                    Ok((rows, _)) => diff_view(ui, pal, rows),
                    Err(e) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("Cannot be applied: {e}"))
                                    .size(12.0)
                                    .color(pal.error),
                            )
                            .wrap(),
                        );
                    }
                }
            }
            ui.add_space(6.0);
            // Claude Code's tool checks the edit against the disk text.
            let unsaved = !build && !c.local && view.writes && view.dirty.contains(&c.abs);
            if unsaved {
                let name = c
                    .abs
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Unsaved changes in {name}: save first"))
                            .size(11.5)
                            .color(pal.warning),
                    )
                    .wrap(),
                );
                ui.add_space(2.0);
            }
            ui.horizontal(|ui| {
                let ok = c.preview.is_ok();
                let (yes, no) = if build {
                    ("Allow", "Deny")
                } else if unsaved {
                    ("Save and apply", "Reject")
                } else {
                    ("Apply", "Reject")
                };
                if text_button(ui, pal, yes, true, ok).clicked() && ok {
                    acts.push(if unsaved {
                        Act::SaveAndApply(idx)
                    } else {
                        Act::Decide(idx, true)
                    });
                }
                if text_button(ui, pal, if c.local { "Dismiss" } else { no }, false, true).clicked()
                {
                    acts.push(Act::Decide(idx, false));
                }
            });
        });
}

fn diff_view(ui: &mut Ui, pal: &Palette, rows: &[diff::Row]) {
    let width = ui.available_width();
    let font = FontId::monospace(11.5);
    Frame::new()
        .fill(pal.editor)
        .stroke(egui::Stroke::new(1.0, pal.separator))
        .corner_radius(6)
        .show(ui, |ui| {
            ui.set_width(width - 2.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in rows.iter().take(MAX_DIFF_ROWS) {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), DIFF_ROW_H), Sense::hover());
                let p = ui.painter_at(rect);
                match row {
                    diff::Row::Header(h) => {
                        p.rect_filled(rect, 0.0, pal.accent.gamma_multiply(0.10));
                        p.text(
                            rect.left_center() + vec2(6.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            h,
                            font.clone(),
                            pal.text_tertiary,
                        );
                    }
                    diff::Row::Line(l) => {
                        let (bg, sign, ink) = match l.kind {
                            diff::Kind::Add => (pal.ok.gamma_multiply(0.16), '+', pal.text),
                            diff::Kind::Del => (pal.error.gamma_multiply(0.16), '-', pal.text),
                            diff::Kind::Same => (Color32::TRANSPARENT, ' ', pal.text_secondary),
                        };
                        p.rect_filled(rect, 0.0, bg);
                        let no = l
                            .new_no
                            .or(l.old_no)
                            .map_or(String::new(), |n| n.to_string());
                        p.text(
                            rect.left_center() + vec2(34.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            no,
                            font.clone(),
                            pal.line_number,
                        );
                        let sign_color = match l.kind {
                            diff::Kind::Add => pal.ok,
                            diff::Kind::Del => pal.error,
                            diff::Kind::Same => pal.text_tertiary,
                        };
                        p.text(
                            rect.left_center() + vec2(42.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            sign,
                            font.clone(),
                            sign_color,
                        );
                        p.text(
                            rect.left_center() + vec2(54.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            &l.text,
                            font.clone(),
                            ink,
                        );
                    }
                }
            }
            if rows.len() > MAX_DIFF_ROWS {
                ui.label(
                    RichText::new(format!("... {} more lines", rows.len() - MAX_DIFF_ROWS))
                        .size(11.0)
                        .color(pal.text_tertiary),
                );
            }
        });
}

// ---- markdown --------------------------------------------------------------------------

fn spans_job(spans: &[Span], pal: &Palette, size: f32, strong: bool) -> LayoutJob {
    // No bold weight is available (the system font is loaded at one weight), so regular text is
    // a touch softer than **bold** text.
    let body = pal.text.lerp_to_gamma(pal.text_secondary, 0.3);
    let mut job = LayoutJob::default();
    for s in spans {
        let mut f = TextFormat::default();
        if s.code {
            f.font_id = FontId::monospace(size - 1.0);
            f.color = pal.text;
            f.background = pal.current_line;
        } else {
            f.font_id = FontId::proportional(size);
            f.color = if s.bold || strong { pal.text } else { body };
            f.italics = s.italic;
        }
        f.line_height = Some(size * 1.45);
        job.append(&s.text, 0.0, f);
    }
    job
}

fn markdown_ui(
    ui: &mut Ui,
    pal: &Palette,
    text: &str,
    live: bool,
    salt: usize,
    view: &View,
    acts: &mut Vec<Act>,
) {
    let blocks = markdown::parse(text);
    for (bi, b) in blocks.iter().enumerate() {
        match b {
            Block::Heading(level, spans) => {
                ui.add_space(4.0);
                let size = match level {
                    1 => 17.0,
                    2 => 15.0,
                    3 => 14.0,
                    _ => 13.0,
                };
                ui.add(egui::Label::new(spans_job(spans, pal, size, true)).wrap());
                ui.add_space(2.0);
            }
            Block::Paragraph(spans) => {
                ui.add(egui::Label::new(spans_job(spans, pal, 13.0, false)).wrap());
                ui.add_space(5.0);
            }
            Block::Quote(spans) => {
                ui.horizontal_top(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(3.0, 16.0), Sense::hover());
                    ui.painter().rect_filled(r, 1.5, pal.separator);
                    ui.add(egui::Label::new(spans_job(spans, pal, 13.0, false)).wrap());
                });
                ui.add_space(5.0);
            }
            Block::List(items) => {
                for it in items {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.add_space(4.0 + it.depth as f32 * 14.0);
                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                        ui.painter().text(
                            r.right_top() + vec2(0.0, 1.0),
                            egui::Align2::RIGHT_TOP,
                            &it.marker,
                            FontId::proportional(13.0),
                            pal.text_secondary,
                        );
                        ui.add(egui::Label::new(spans_job(&it.spans, pal, 13.0, false)).wrap());
                    });
                }
                ui.add_space(5.0);
            }
            Block::Table(rows) => {
                egui::ScrollArea::horizontal()
                    .id_salt(("assistant-table", salt, bi))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(rows.join("\n"))
                                    .monospace()
                                    .size(11.5)
                                    .color(pal.text),
                            )
                            .extend(),
                        );
                    });
                ui.add_space(5.0);
            }
            Block::Rule => {
                ui.add_space(3.0);
                hline(ui, pal);
                ui.add_space(3.0);
            }
            Block::Code { lang, code, closed } => {
                code_block(
                    ui,
                    pal,
                    lang,
                    code,
                    (salt, bi, *closed && !(live && bi + 1 == blocks.len())),
                    view,
                    acts,
                );
                ui.add_space(6.0);
            }
        }
    }
}

fn code_block(
    ui: &mut Ui,
    pal: &Palette,
    lang: &str,
    code: &str,
    (salt, index, done): (usize, usize, bool),
    view: &View,
    acts: &mut Vec<Act>,
) {
    let pascal_like = lang.is_empty()
        || [
            "pascal",
            "delphi",
            "objectpascal",
            "objfpc",
            "fpc",
            "lazarus",
            "pas",
            "lfm",
        ]
        .iter()
        .any(|l| lang.to_ascii_lowercase().starts_with(l));
    let applicable = done
        && view
            .active
            .as_ref()
            .is_some_and(|f| block_targets_file(lang, code, f));
    Frame::new()
        .fill(pal.sidebar)
        .stroke(egui::Stroke::new(1.0, pal.separator))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            Frame::new()
                .inner_margin(Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let name = if lang.is_empty() { "code" } else { lang };
                        ui.label(RichText::new(name).size(11.0).color(pal.text_tertiary));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if applicable && text_button(ui, pal, "Apply", true, true).clicked() {
                                acts.push(Act::ApplyBlock(code.to_string()));
                            }
                            if view.can_insert
                                && done
                                && text_button(ui, pal, "Insert at cursor", false, true).clicked()
                            {
                                acts.push(Act::Insert(code.to_string()));
                            }
                            if text_button(ui, pal, "Copy", false, true).clicked() {
                                acts.push(Act::Copy(code.to_string()));
                            }
                        });
                    });
                });
            let font = FontId::monospace(12.0);
            let mut job = if pascal_like {
                highlight::layout(code, pal, font.clone(), 17.0)
            } else {
                let mut j = LayoutJob::default();
                let mut f = TextFormat::simple(font, pal.code_text);
                f.line_height = Some(17.0);
                j.append(code, 0.0, f);
                j
            };
            job.wrap.max_width = f32::INFINITY;
            Frame::new()
                .fill(pal.editor)
                .corner_radius(egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 8,
                    se: 8,
                })
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::horizontal()
                        .id_salt(("assistant-code", salt, index))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(job).extend());
                        });
                });
        });
}
