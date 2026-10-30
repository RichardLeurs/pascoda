use super::*;
use pascoda_ai::{AiError, Finish};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn none(_: &Path) -> Option<String> {
    None
}

// ---- context -------------------------------------------------------------------------------

fn snap() -> Snapshot {
    Snapshot {
        file: Some(("unit1.pas".into(), "unit unit1;\nend.\n".into(), 2)),
        selection: Some(("unit1.pas".into(), 3, 5, "a\nb\nc".into())),
        errors: vec!["unit1.pas(3,5) Error: Identifier not found \"X\"".into()],
        ..Default::default()
    }
}

#[test]
fn context_is_prepended_and_labelled() {
    let (text, labels) = compose(Chips::default(), &snap(), "why?");
    assert!(text.starts_with("[Context from the IDE]\nActive file: unit1.pas (caret at line 2)"));
    assert!(text.contains("```pascal\nunit unit1;\nend.\n```"));
    assert!(text.contains("Selected text (lines 3-5 of unit1.pas):"));
    assert!(text.contains("Current build errors:\nunit1.pas(3,5) Error"));
    assert!(text.ends_with("[End of context]\n\nwhy?"));
    assert_eq!(labels, vec!["unit1.pas", "Selection, 3 lines", "1 error"]);
}

#[test]
fn chips_switch_parts_off() {
    let off = Chips {
        file: false,
        selection: false,
        errors: false,
        project: false,
        tabs: false,
    };
    assert_eq!(compose(off, &snap(), "hi"), ("hi".to_string(), vec![]));
    let only_errors = Chips {
        errors: true,
        ..off
    };
    let (text, labels) = compose(only_errors, &snap(), "hi");
    assert!(!text.contains("Active file") && text.contains("build errors"));
    assert_eq!(labels, vec!["1 error"]);
}

fn rich() -> Snapshot {
    Snapshot {
        overview: Some("Project: notes\nFiles:\nnotes.lpr\n".into()),
        tabs: vec![
            Tab {
                name: "a.pas".into(),
                unsaved: None,
            },
            Tab {
                name: "b.pas".into(),
                unsaved: Some("unit b; // UNSAVED_B\n".into()),
            },
        ],
        ..snap()
    }
}

#[test]
fn the_project_chip_is_on_and_the_tabs_chip_is_off_by_default() {
    let c = Chips::default();
    assert!(c.project && !c.tabs);
    let (text, labels) = compose(c, &rich(), "q");
    assert!(text.starts_with("[Context from the IDE]\nProject overview"));
    assert!(text.contains("Project: notes\nFiles:\nnotes.lpr\n"));
    assert!(!text.contains("Other open tabs") && !text.contains("UNSAVED_B"));
    assert_eq!(
        labels,
        vec!["unit1.pas", "Selection, 3 lines", "project", "1 error"]
    );
}

#[test]
fn the_tabs_chip_adds_names_and_unsaved_text() {
    let c = Chips {
        tabs: true,
        ..Chips::default()
    };
    let (text, labels) = compose(c, &rich(), "q");
    assert!(text.contains("Other open tabs: a.pas, b.pas (unsaved)\n"));
    assert!(text.contains(
        "Unsaved text of b.pas (newer than the file on disk):\n```pascal\nunit b; // UNSAVED_B\n```"
    ));
    assert!(!text.contains("Unsaved text of a.pas"));
    assert_eq!(labels[3], "2 other tabs");
    // Each chip removes only its own part.
    let (t, l) = compose(
        Chips {
            project: false,
            ..c
        },
        &rich(),
        "q",
    );
    assert!(!t.contains("Project overview") && t.contains("UNSAVED_B"));
    assert!(!l.contains(&"project".to_string()));
    let (t, _) = compose(Chips { tabs: false, ..c }, &rich(), "q");
    assert!(t.contains("Project overview") && !t.contains("UNSAVED_B"));
}

#[test]
fn unsaved_tab_text_is_capped_each_and_in_total() {
    let c = Chips {
        tabs: true,
        ..Chips::default()
    };
    let big = |name: &str, ch: &str| Tab {
        name: name.into(),
        unsaved: Some(ch.repeat(10_000)),
    };
    let s = Snapshot {
        tabs: vec![
            big("a.pas", "a"),
            big("b.pas", "b"),
            big("c.pas", "c"),
            big("d.pas", "d"),
        ],
        ..Default::default()
    };
    let (text, _) = compose(c, &s, "q");
    let count = |ch: char| text.chars().filter(|x| *x == ch).count();
    // 'a' and 'b' fill their 6,000; 'c' gets the 4,000 that are left; 'd' gets nothing.
    assert!(count('b') >= 6_000 && count('b') < 6_100, "{}", count('b'));
    assert!(text.contains("(cut after 6000 of 10000 characters)"));
    assert!(text.contains("(cut after 4000 of 10000 characters)"));
    assert!(text.contains("Unsaved d.pas: text not included"));
    assert!(text.len() < 17_500, "{}", text.len());
}

#[test]
fn a_long_file_is_named_not_pasted() {
    let mut s = snap();
    s.file = Some(("big.pas".into(), "x".repeat(MAX_FILE_CHARS + 1), 1));
    let (text, _) = compose(Chips::default(), &s, "q");
    assert!(text.contains("Active file: big.pas") && text.contains("use read_file"));
    assert!(!text.contains(&"x".repeat(100)));
}

#[test]
fn many_errors_are_capped() {
    let s = Snapshot {
        errors: (0..30).map(|i| format!("e{i}")).collect(),
        ..Default::default()
    };
    let (text, labels) = compose(Chips::default(), &s, "q");
    assert!(text.contains("e19\n") && !text.contains("e20\n") && text.contains("and 10 more"));
    assert_eq!(labels, vec!["30 errors"]);
}

// ---- the conversation state machine -----------------------------------------------------------

#[test]
fn events_build_the_conversation() {
    let mut a = Assistant::default();
    a.on_event(Event::Text("Hel".into()), &none);
    a.on_event(Event::Text("lo".into()), &none);
    a.on_event(
        Event::ToolCall {
            id: "t1".into(),
            name: "read_file".into(),
            input: json!({"path": "unit1.pas"}),
        },
        &none,
    );
    a.on_event(
        Event::ToolResult {
            id: "t1".into(),
            name: "read_file".into(),
            output: "unit unit1;".into(),
            is_error: false,
        },
        &none,
    );
    a.on_event(
        Event::ToolCall {
            id: "t2".into(),
            name: "build".into(),
            input: json!({}),
        },
        &none,
    );
    a.on_event(
        Event::ToolResult {
            id: "t2".into(),
            name: "build".into(),
            output: "denied\nmore".into(),
            is_error: true,
        },
        &none,
    );
    a.on_event(Event::Text("Bye".into()), &none);
    for (i, o) in [(10, 5), (20, 7)] {
        a.on_event(
            Event::Usage(Usage {
                input_tokens: i,
                output_tokens: o,
                ..Default::default()
            }),
            &none,
        );
    }
    assert_eq!(a.items.len(), 4, "{:?}", a.items);
    assert_eq!(a.items[0], Item::Assistant("Hello".into()));
    let Item::Tool(t) = &a.items[1] else { panic!() };
    assert_eq!(
        (t.label.as_str(), &t.state),
        ("Read unit1.pas", &ToolState::Done)
    );
    let Item::Tool(t) = &a.items[2] else { panic!() };
    assert_eq!(t.label, "Build");
    assert_eq!(t.state, ToolState::Failed("denied".into()));
    assert_eq!(a.items[3], Item::Assistant("Bye".into()));
    assert_eq!((a.usage.input_tokens, a.usage.output_tokens), (30, 12));
}

#[test]
fn a_proposal_becomes_a_card_with_a_diff() {
    let mut a = Assistant::default();
    let p = Proposal {
        id: "p1".into(),
        path: "unit1.pas".into(),
        abs_path: "/x/unit1.pas".into(),
        change: Change::Edit {
            old_text: "two".into(),
            new_text: "2".into(),
        },
    };
    a.on_event(Event::NeedsApproval(p), &|_| {
        Some("one\ntwo\nthree\n".to_string())
    });
    let Item::Card(c) = &a.items[0] else { panic!() };
    assert_eq!(c.state, CardState::Pending);
    let (rows, stats) = c.preview.as_ref().unwrap();
    assert_eq!(*stats, (1, 1));
    assert!(matches!(&rows[0], diff::Row::Header(h) if h == "@@ -1,3 +1,3 @@"));
    // The file vanished: the card says why, instead of a diff
    let p2 = Proposal {
        id: "p2".into(),
        path: "gone.pas".into(),
        abs_path: "/x/gone.pas".into(),
        change: Change::Edit {
            old_text: "a".into(),
            new_text: "b".into(),
        },
    };
    a.on_event(Event::NeedsApproval(p2), &none);
    assert!(matches!(&a.items[1], Item::Card(c) if c.preview.is_err()));
}

#[test]
fn ending_a_turn_cancels_open_cards_and_a_bad_key_is_forgotten() {
    let mut a = Assistant::default();
    a.use_key("k");
    a.items.push(Item::Card(Card {
        id: "p".into(),
        path: "a".into(),
        abs: "/a".into(),
        change: Change::Build,
        state: CardState::Pending,
        auto: false,
        undo: None,
        local: false,
        preview: Ok((vec![], (0, 0))),
    }));
    a.items.push(Item::Card(Card {
        id: String::new(),
        path: "a".into(),
        abs: "/a".into(),
        change: Change::Build,
        state: CardState::Pending,
        auto: false,
        undo: None,
        local: true,
        preview: Ok((vec![], (0, 0))),
    }));
    a.on_event(Event::Error(AiError::Auth("401".into())), &none);
    assert!(matches!(&a.items[0], Item::Card(c) if c.state == CardState::Cancelled));
    assert!(
        matches!(&a.items[1], Item::Card(c) if c.state == CardState::Pending),
        "local cards stay"
    );
    assert!(matches!(a.items.last(), Some(Item::Error(e)) if e.contains("key was rejected")));
    assert!(!a.ready() && a.key == KeyState::Missing && a.key_message.is_some());
    a.on_event(Event::Done(Finish::Cancelled), &none);
    assert!(matches!(a.items.last(), Some(Item::Note(n)) if n == "Stopped."));
}

#[test]
fn tool_labels() {
    let l = |n: &str, v: Value| tool_label(n, &v);
    assert_eq!(l("list_files", json!({})), "List files");
    assert_eq!(
        l(
            "read_file",
            json!({"path": "a.pas", "start_line": 3, "end_line": 9})
        ),
        "Read a.pas (lines 3-9)"
    );
    assert_eq!(
        l("read_form", json!({"path": "unit1.pas"})),
        "Read form of unit1.pas"
    );
    assert_eq!(l("get_diagnostics", json!({})), "Get build errors");
    assert_eq!(l("edit_file", json!({"path": "a.pas"})), "Edit a.pas");
    assert_eq!(l("create_file", json!({"path": "b.pas"})), "Create b.pas");
    assert_eq!(l("glob", json!({"pattern": "**/*.pas"})), "Find **/*.pas");
    assert_eq!(l("grep", json!({"pattern": "Label1"})), "Search for Label1");
}

#[test]
fn usage_text() {
    use ui::usage_line;
    assert_eq!(usage_line(&Usage::default()), "No tokens used yet");
    let u = Usage {
        input_tokens: 1234,
        output_tokens: 56,
        cache_read_input_tokens: 1000,
        ..Default::default()
    };
    assert_eq!(usage_line(&u), "2,234 tokens in, 56 out (1,000 cached)");
}

// ---- which code blocks can be applied ---------------------------------------------------------

fn file(name: &str, text: &str) -> ActiveFile {
    ActiveFile {
        name: name.into(),
        text: text.into(),
    }
}

#[test]
fn only_clear_targets_get_an_apply_button() {
    let unit = file(
        "unit1.pas",
        "unit Unit1;\n{$mode objfpc}\ninterface\nend.\n",
    );
    // same unit header
    assert!(block_targets_file(
        "pascal",
        "unit unit1;\ninterface\nimplementation\nend.",
        &unit
    ));
    // another unit
    assert!(!block_targets_file(
        "pascal",
        "unit other;\ninterface\nend.",
        &unit
    ));
    // a snippet
    assert!(!block_targets_file(
        "pascal",
        "procedure Foo;\nbegin\nend;",
        &unit
    ));
    // names the file
    assert!(block_targets_file(
        "pascal unit1.pas",
        "procedure Foo;\nbegin\nend;",
        &unit
    ));
    assert!(block_targets_file("unit1.pas", "x", &unit));
    assert!(!block_targets_file("pascal other.pas", "x", &unit));
    // a comment line before the header is skipped
    assert!(block_targets_file(
        "",
        "// new version\nunit unit1;\nend.",
        &unit
    ));
    // forms
    let form = file("unit1.lfm", "object Form1: TForm1\n  Caption = 'a'\nend\n");
    assert!(block_targets_file(
        "",
        "object Form1: TForm1\n  Caption = 'b'\nend",
        &form
    ));
    assert!(!block_targets_file("", "object Form2: TForm2\nend", &form));
    assert!(!block_targets_file("", "  ", &unit));
}

// ---- applying changes ----------------------------------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let dir = crate::target_tmp().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["hello.lpr", "unit1.pas", "unit1.lfm"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/gui")
                .join(f),
            dir.join(f),
        )
        .unwrap();
    }
    dir.canonicalize().unwrap()
}

fn edit(old: &str, new: &str) -> Change {
    Change::Edit {
        old_text: old.into(),
        new_text: new.into(),
    }
}

#[test]
fn approving_an_edit_changes_the_open_text_and_rejecting_does_not() {
    let dir = scratch("assist-apply");
    let unit = dir.join("unit1.pas");
    let mut app = App::bare(Some(unit.clone()));
    let before = app.docs[0].text.clone();

    let card = |id: &str, change: Change| {
        Item::Card(Card {
            id: id.into(),
            path: "unit1.pas".into(),
            abs: unit.clone(),
            preview: preview_of(
                &Proposal {
                    id: id.into(),
                    path: "unit1.pas".into(),
                    abs_path: unit.clone(),
                    change: change.clone(),
                },
                Some(before.clone()),
            ),
            change,
            state: CardState::Pending,
            auto: false,
            undo: None,
            local: true,
        })
    };
    app.assistant
        .items
        .push(card("a", edit("'Hello from Pascora'", "'Hi'")));
    app.assistant
        .items
        .push(card("b", edit("'Hello from Pascora'", "'Never'")));

    // Reject: nothing changes, nothing is written
    app.assistant_decide(1, false);
    assert_eq!(app.docs[0].text, before);
    assert!(matches!(&app.assistant.items[1], Item::Card(c) if c.state == CardState::Rejected));

    // Approve: the tab's text changes (unsaved, so undo/save/LSP see it like typing)
    app.assistant_decide(0, true);
    assert!(app.docs[0].text.contains("Label1.Caption := 'Hi';"));
    assert!(app.docs[0].dirty());
    assert_eq!(
        std::fs::read_to_string(&unit).unwrap(),
        before,
        "disk is untouched until save"
    );
    assert!(matches!(&app.assistant.items[0], Item::Card(c) if c.state == CardState::Applied));

    // A card whose old text is gone fails and says so
    app.assistant.items.push(card("c", edit("'Never'", "'x'")));
    // (built against the old text, so the preview is fine but applying fails)
    app.assistant_decide(2, true);
    assert!(
        matches!(&app.assistant.items[2], Item::Card(c) if matches!(&c.state, CardState::Failed(m) if m.contains("not found")))
    );
}

#[test]
fn an_edit_to_the_open_form_reloads_the_designer_and_keeps_the_unit_in_step() {
    let dir = scratch("assist-form");
    let unit = dir.join("unit1.pas");
    let lfm = dir.join("unit1.lfm");
    let mut app = App::bare(Some(unit.clone()));
    app.show_form();
    assert_eq!(app.designers.len(), 1);
    // The model sees the designer's text, not the disk.
    assert_eq!(
        current_text(&app.docs, &app.designers, &lfm).unwrap(),
        std::fs::read_to_string(&lfm).unwrap()
    );
    let change = edit(
        "  object Label1: TLabel",
        "  object Edit1: TEdit\n    Left = 16\n    Height = 28\n    Top = 100\n    Width = 120\n    OnChange = Edit1Change\n  end\n  object Label1: TLabel",
    );
    let note = app.apply_change(&lfm, &change).unwrap();
    assert_eq!(note, None);
    let d = &app.designers[0];
    assert!(d.doc.source().contains("object Edit1: TEdit"));
    assert!(d.dirty(), "the designer holds the edit until saved");
    let unit_text = &app.docs[0].text;
    assert!(unit_text.contains("Edit1: TEdit;"), "{unit_text}");
    assert!(
        unit_text.contains("procedure Edit1Change(Sender: TObject);"),
        "{unit_text}"
    );
    assert!(
        unit_text.contains("procedure TForm1.Edit1Change"),
        "{unit_text}"
    );
    // It is one undo step in the designer
    assert!(app.designers[0].undo());
    assert!(!app.designers[0].doc.source().contains("Edit1"));
    // A bad edit leaves everything alone
    let before = app.designers[0].doc.source().to_string();
    assert!(app.apply_change(&lfm, &edit("not there", "x")).is_err());
    assert_eq!(app.designers[0].doc.source(), before);
}

#[test]
fn files_that_are_not_open_are_written_and_new_files_open() {
    let dir = scratch("assist-files");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    // hello.lpr is not open
    let lpr = dir.join("hello.lpr");
    let old = std::fs::read_to_string(&lpr).unwrap();
    let first_word = old.split_whitespace().next().unwrap().to_string();
    app.apply_change(&lpr, &edit(&first_word, "PROGRAM"))
        .unwrap();
    assert!(
        std::fs::read_to_string(&lpr)
            .unwrap()
            .starts_with("PROGRAM")
    );

    let new = dir.join("sub/extra.pas");
    app.apply_change(
        &new,
        &Change::Create {
            content: "unit extra;\n".into(),
        },
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "unit extra;\n");
    assert_eq!(app.docs[app.active].path, new, "the new file is opened");
    assert!(
        app.apply_change(
            &new,
            &Change::Create {
                content: "x".into()
            }
        )
        .is_err()
    );
}

#[test]
fn a_code_block_becomes_a_local_card_for_the_whole_file() {
    let dir = scratch("assist-block");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    let new = app.docs[0]
        .text
        .replace("Hello from Pascora", "From a block");
    app.assistant_propose_block(&new);
    let Some(Item::Card(c)) = app.assistant.items.last() else {
        panic!()
    };
    assert!(
        c.local && c.preview.as_ref().unwrap().1 == (1, 1),
        "{:?}",
        c.preview
    );
    let idx = app.assistant.items.len() - 1;
    app.assistant_decide(idx, true);
    assert!(app.docs[0].text.contains("From a block"));
    assert!(app.docs[0].dirty());
}

// ---- one turn through the app, against a local mock server -----------------------------------------

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn sse(blocks: &[Value], stop: &str) -> String {
    let mut s = ev(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","usage":{"input_tokens":10,"output_tokens":1}}}),
    );
    for (i, b) in blocks.iter().enumerate() {
        if b["type"] == "text" {
            s += &ev(
                "content_block_start",
                json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}}),
            );
            s += &ev(
                "content_block_delta",
                json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":b["text"]}}),
            );
        } else {
            s += &ev(
                "content_block_start",
                json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":b["id"],"name":b["name"],"input":{}}}),
            );
            s += &ev(
                "content_block_delta",
                json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":b["input"].to_string()}}),
            );
        }
        s += &ev(
            "content_block_stop",
            json!({"type":"content_block_stop","index":i}),
        );
    }
    s += &ev(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
    );
    s + &ev("message_stop", json!({"type":"message_stop"}))
}

/// Serves one scripted reply per request; returns the address and the request bodies.
pub(crate) fn mock(script: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let (url, bodies, _) = mock_auth(script);
    (url, bodies)
}

/// Like [`mock`], and also the `authorization` header of each request (empty when it has none).
pub(crate) fn mock_auth(
    script: Vec<String>,
) -> (String, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    let auths = Arc::new(Mutex::new(Vec::new()));
    let seen_auths = auths.clone();
    std::thread::spawn(move || {
        let mut script = script.into_iter();
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let mut head = String::new();
            let Some(body) = read_body(&mut conn, &mut head) else {
                continue;
            };
            // The name is matched in any case; the value keeps its case (a key is case sensitive).
            let auth = head
                .lines()
                .find_map(|l| {
                    (l.to_ascii_lowercase().starts_with("authorization:"))
                        .then(|| l["authorization:".len()..].trim().to_string())
                })
                .unwrap_or_default();
            seen_auths.lock().unwrap().push(auth);
            seen.lock()
                .unwrap()
                .push(serde_json::from_str(&body).unwrap_or(Value::Null));
            let Some(reply) = script.next() else { break };
            std::thread::spawn(move || {
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
                for c in reply.as_bytes().chunks(41) {
                    if conn.write_all(c).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, bodies, auths)
}

/// Reads one request: its body, and its header block (lower case) in `head_out`.
fn read_body(conn: &mut TcpStream, head_out: &mut String) -> Option<String> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = conn.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(p) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            *head_out = String::from_utf8_lossy(&data[..p]).into_owned();
            let head = head_out.to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while data.len() < p + 4 + len {
                let n = conn.read(&mut buf).ok()?;
                if n == 0 {
                    return None;
                }
                data.extend_from_slice(&buf[..n]);
            }
            return Some(String::from_utf8_lossy(&data[p + 4..p + 4 + len]).to_string());
        }
    }
}

fn wait(app: &mut App, ctx: &egui::Context, what: &str, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out waiting for {what}: {:?}",
            app.assistant.items
        );
        app.poll_assistant(ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pending_card(app: &App) -> Option<usize> {
    app.assistant
        .items
        .iter()
        .position(|i| matches!(i, Item::Card(c) if c.state == CardState::Pending))
}

/// Runs one turn: the model reads the unit, proposes an edit, and (after the user's answer)
/// says it is done. Returns the app and the request bodies the mock saw.
fn one_turn(approve: bool, name: &str) -> (App, Vec<Value>) {
    let dir = scratch(name);
    let (url, bodies) = mock(vec![
        sse(
            &[
                json!({"type":"text","text":"Let me look."}),
                json!({"type":"tool_use","id":"t1","name":"read_file","input":{"path":"unit1.pas"}}),
            ],
            "tool_use",
        ),
        sse(
            &[
                json!({"type":"tool_use","id":"t2","name":"edit_file","input":{"path":"unit1.pas","old_text":"'Hello from Pascora'","new_text":"'Fixed'"}}),
            ],
            "tool_use",
        ),
        sse(&[json!({"type":"text","text":"All **done**."})], "end_turn"),
    ]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    // Unsaved text must be what the model sees.
    app.docs[0].text.push_str("// unsaved marker\n");
    app.assistant.use_key("test-key-not-real");
    app.assistant.base_url = Some(url);
    let ctx = egui::Context::default();
    app.assistant.input = "Please change the greeting".into();
    assert!(app.assistant_send(&ctx));
    assert!(app.assistant.input.is_empty() && app.assistant.streaming());

    wait(&mut app, &ctx, "a proposal", |a| pending_card(a).is_some());
    let idx = pending_card(&app).unwrap();
    assert!(app.assistant.streaming(), "the agent waits for the answer");
    assert!(
        !app.docs[0].text.contains("'Fixed'"),
        "nothing changes before the user answers"
    );
    app.assistant_decide(idx, approve);
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    let seen = bodies.lock().unwrap().clone();
    (app, seen)
}

#[test]
fn a_turn_through_the_app_with_an_approved_edit() {
    let (app, requests) = one_turn(true, "assist-e2e-ok");
    assert!(app.docs[0].text.contains("Label1.Caption := 'Fixed';"));
    let items = &app.assistant.items;
    assert!(
        matches!(&items[0], Item::User { text, sent, chips } if text == "Please change the greeting" && sent.contains("// unsaved marker") && chips[0] == "unit1.pas"),
        "{items:?}"
    );
    assert!(items.iter().any(
        |i| matches!(i, Item::Tool(t) if t.label == "Read unit1.pas" && t.state == ToolState::Done)
    ));
    assert!(items.iter().any(
        |i| matches!(i, Item::Tool(t) if t.label == "Edit unit1.pas" && t.state == ToolState::Done)
    ));
    assert!(
        items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.state == CardState::Applied))
    );
    assert_eq!(items.last(), Some(&Item::Assistant("All **done**.".into())));
    assert_eq!(app.assistant.usage.output_tokens, 15);
    assert_eq!(requests.len(), 3);
    // The first request carries the context message; the key is a header, not in the body.
    let first = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        first.contains("[Context from the IDE]") && first.ends_with("Please change the greeting")
    );
    assert!(!requests[0].to_string().contains("test-key-not-real"));
    // The model read the unsaved text and was told the edit was applied.
    assert!(requests[1].to_string().contains("unsaved marker"));
    assert!(requests[2].to_string().contains("approved the change"));
}

#[test]
fn a_turn_through_the_app_with_a_rejected_edit() {
    let (app, requests) = one_turn(false, "assist-e2e-no");
    assert!(!app.docs[0].text.contains("'Fixed'"));
    assert!(app.docs[0].text.contains("'Hello from Pascora'"));
    assert!(
        app.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.state == CardState::Rejected))
    );
    assert!(requests[2].to_string().contains("rejected this change"));
}

#[test]
fn stop_ends_a_turn_waiting_for_approval() {
    let dir = scratch("assist-e2e-stop");
    let (url, _) = mock(vec![sse(
        &[
            json!({"type":"tool_use","id":"t1","name":"edit_file","input":{"path":"unit1.pas","old_text":"'Hello from Pascora'","new_text":"'x'"}}),
        ],
        "tool_use",
    )]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.use_key("k");
    app.assistant.base_url = Some(url);
    let ctx = egui::Context::default();
    app.assistant.input = "go".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "a proposal", |a| pending_card(a).is_some());
    app.assistant.stop();
    wait(&mut app, &ctx, "the stop", |a| !a.assistant.streaming());
    assert!(
        app.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.state == CardState::Cancelled))
    );
    assert!(matches!(app.assistant.items.last(), Some(Item::Note(n)) if n == "Stopped."));
    // Clear forgets the conversation
    app.assistant.clear();
    assert!(app.assistant.items.is_empty());
}

#[test]
fn an_assistant_build_goes_through_the_ides_build_path() {
    // Only the plumbing: a request while a build is already running is refused at once; the
    // real build path is covered by the app's own build tests.
    let dir = scratch("assist-build");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    let ctx = egui::Context::default();
    app.building = Some(std::sync::mpsc::channel().1);
    let (tx, rx) = channel();
    app.assistant.build_tx.send(BuildReq(tx)).unwrap();
    app.poll_assistant(&ctx);
    assert!(rx.try_recv().unwrap().contains("Cannot build now"));
    // A finished build reaches a waiting reply, with the diagnostics refreshed first.
    let (tx, rx) = channel();
    app.assistant_build_reply = Some(tx);
    app.assistant_build_done(&BuildResult {
        success: false,
        diagnostics: vec![],
        raw_output: "unit1.pas(3,5) Error: x".into(),
    });
    let out = rx.try_recv().unwrap();
    assert!(out.starts_with("Build failed.") && out.contains("Error: x"));
}

#[test]
fn the_host_serves_unsaved_text_forms_and_diagnostics() {
    let dir = scratch("assist-host");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.show_form();
    app.docs[0].text.push_str("// edited\n");
    app.diagnostics = vec![Diagnostic {
        file: "unit1.pas".into(),
        line: 3,
        col: 5,
        severity: Severity::Error,
        message: "Identifier not found".into(),
    }];
    app.refresh_host_data();
    let (btx, _brx) = channel();
    let mut host = AppHost {
        data: app.assistant.data.clone(),
        build: btx,
        ctx: egui::Context::default(),
        stop: Arc::default(),
    };
    assert!(
        host.open_text(&dir.join("unit1.pas"))
            .unwrap()
            .ends_with("// edited\n")
    );
    assert!(
        host.read_form(&dir.join("unit1.lfm"))
            .unwrap()
            .starts_with("object Form1")
    );
    assert_eq!(host.open_text(&dir.join("other.pas")), None);
    assert_eq!(
        host.diagnostics(),
        "unit1.pas(3,5) Error: Identifier not found"
    );
    // A stop ends a build wait
    host.stop.store(true, Ordering::Relaxed);
    assert!(host.build().contains("stopped"));
}

pub(crate) fn text_reply(text: &str) -> String {
    sse(&[json!({"type":"text","text":text})], "end_turn")
}

// ---- the Claude Code (subscription) provider ----------------------------------------------------

use std::os::unix::fs::PermissionsExt;

fn cli_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../pascoda-ai/tests/fixtures/claude")
}

/// A `claude` stand-in that plays `script` (see the fixture folder of pascoda-ai). Returns the
/// program and the folder where it logs what it was sent.
fn fake_cli(name: &str, script: &str) -> (PathBuf, PathBuf) {
    let base = crate::target_tmp().join(format!("{name}-cli"));
    let _ = std::fs::remove_dir_all(&base);
    let (scripts, logs) = (base.join("scripts"), base.join("logs"));
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(scripts.join("1.jsonl"), script).unwrap();
    let wrapper = base.join("claude");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' '{}' '{}' \"$@\"\n",
            cli_fixtures().join("fake_claude.sh").display(),
            scripts.display(),
            logs.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    (wrapper, logs)
}

fn cli_found(logged_in: Option<bool>) -> CliState {
    CliState::Found(CliStatus {
        binary: "/usr/local/bin/claude".into(),
        version: Some("2.1.312".into()),
        logged_in,
    })
}

const GREETING: &str = "'Hello from Pascora'";

/// The fake CLI's `#write` line: `rel` (under the project) gets `text` once the tool is allowed.
fn write_line(rel: &str, text: &str) -> String {
    let esc = text.replace('\\', "\\\\").replace('\n', "\\n");
    format!("{{\"#write\":\"{{ROOT}}/{rel}|{esc}\"}}")
}

/// The CLI asks to use `tool`, waits for the answer, writes `write` when the answer is allow
/// (the stand-in for the CLI's own Edit/Write), reports `result` and ends the turn.
fn tool_script(
    tool: &str,
    input: Value,
    write: Option<(&str, &str)>,
    result: (&str, bool),
) -> String {
    let mut lines = vec![
        json!({"#wait": "user"}).to_string(),
        json!({"type":"system","subtype":"init","session_id":"sess-1","tools":["Edit"]}).to_string(),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":10,"output_tokens":1}}}}).to_string(),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}).to_string(),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I will change it."}}}).to_string(),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_stop","index":0}}).to_string(),
        json!({"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"tool_use","id":"tu1","name":tool,"input":input}]}}).to_string(),
        json!({"type":"control_request","request_id":"rq1","request":{"subtype":"can_use_tool","tool_name":tool,"tool_use_id":"tu1","input":input}}).to_string(),
        json!({"#wait": "control_response"}).to_string(),
    ];
    if let Some((rel, text)) = write {
        lines.push(write_line(rel, text));
    }
    lines.push(json!({"type":"user","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":result.0,"is_error":result.1}]}}).to_string());
    lines.extend([
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":10,"output_tokens":7}}}).to_string(),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}).to_string(),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"All **done**."}}}).to_string(),
        json!({"type":"result","subtype":"success","is_error":false,"result":"All done.","terminal_reason":"completed"}).to_string(),
    ]);
    lines.join("\n") + "\n"
}

/// An Edit of unit1.pas (the greeting becomes 'Fixed').
fn edit_input() -> Value {
    json!({"file_path":"{ROOT}/unit1.pas","old_string":GREETING,"new_string":"'Fixed'"})
}

/// A fake-CLI conversation in progress: the app, a context and where the CLI logs.
struct CliRun {
    app: App,
    ctx: egui::Context,
    logs: PathBuf,
    dir: PathBuf,
}

/// Sends a message and waits until the CLI's edit shows up as a card (waiting, or already
/// answered by the auto switch). `dirty` is typed into the unit's tab first and unsaved;
/// `fail` makes the CLI's write fail. `auto` turns the switch on before sending.
fn cli_start(name: &str, dirty: Option<&str>, fail: bool, auto: bool) -> CliRun {
    let dir = scratch(name);
    let before = std::fs::read_to_string(dir.join("unit1.pas")).unwrap() + dirty.unwrap_or("");
    let after = before.replace(GREETING, "'Fixed'");
    let script = if fail {
        tool_script(
            "Edit",
            edit_input(),
            None,
            ("File has been modified since read.", true),
        )
    } else {
        tool_script(
            "Edit",
            edit_input(),
            Some(("unit1.pas", &after)),
            ("The file unit1.pas has been updated successfully.", false),
        )
    };
    let (bin, logs) = fake_cli(name, &script);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::ClaudeCode;
    app.assistant.cli_bin = Some(bin);
    app.assistant.cli = cli_found(Some(true));
    assert!(app.assistant.ready());
    if let Some(d) = dirty {
        app.docs[0].text.push_str(d);
    }
    app.assistant.set_auto_edits(auto);
    let ctx = egui::Context::default();
    app.assistant.input = "Please change the greeting".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "a proposal", |a| {
        a.assistant.items.iter().any(|i| matches!(i, Item::Card(_)))
    });
    CliRun {
        app,
        ctx,
        logs,
        dir,
    }
}

impl CliRun {
    fn card(&self) -> (usize, &Card) {
        self.app
            .assistant
            .items
            .iter()
            .enumerate()
            .find_map(|(i, it)| match it {
                Item::Card(c) => Some((i, c)),
                _ => None,
            })
            .unwrap()
    }

    /// Waits for the end of the turn; returns what the CLI was sent.
    fn finish(&mut self) -> Vec<Value> {
        wait(&mut self.app, &self.ctx, "the end of the turn", |a| {
            !a.assistant.streaming()
        });
        let log = std::fs::read_to_string(self.logs.join("stdin.1")).unwrap();
        log.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn unit(&self) -> PathBuf {
        self.dir.join("unit1.pas")
    }
}

/// `char` index where `needle` starts in `text`.
fn char_at(text: &str, needle: &str) -> usize {
    text[..text.find(needle).unwrap()].chars().count()
}

fn set_caret(ctx: &egui::Context, id: egui::Id, at: usize) {
    let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(at))));
    st.store(ctx, id);
}

fn caret(ctx: &egui::Context, id: egui::Id) -> usize {
    egui::TextEdit::load_state(ctx, id)
        .and_then(|s| s.cursor.char_range())
        .map_or(0, |r| r.primary.index.0)
}

#[test]
fn a_claude_code_turn_with_an_approved_edit_is_written_by_the_cli_and_reloaded() {
    let mut r = cli_start("assist-cli-ok", None, false, false);
    let (idx, c) = r.card();
    assert_eq!(c.state, CardState::Pending);
    assert!(r.app.assistant.streaming(), "the CLI waits for the answer");
    let (tab, original) = (r.app.docs[0].editor_id, r.app.docs[0].text.clone());
    // one caret after the edit (at the end), one before it (in the uses clause)
    let at_end = original.chars().count();
    set_caret(&r.ctx, tab, at_end);
    r.app.assistant_decide(idx, true);
    // Pascoda does not touch the text: the CLI's own tool does
    assert_eq!(r.app.assistant.items[idx].card_state(), &CardState::Writing);
    assert_eq!(r.app.docs[0].text, original);
    let stdin = r.finish();
    let disk = std::fs::read_to_string(r.unit()).unwrap();
    assert!(disk.contains("Label1.Caption := 'Fixed';"));
    // the tab was reloaded from the disk and is clean
    let doc = &r.app.docs[0];
    assert_eq!(doc.text, disk);
    assert_eq!(doc.saved, disk);
    assert!(!doc.dirty());
    // the caret moved with the text and stayed in range
    let n = disk.chars().count();
    assert_eq!(caret(&r.ctx, tab), n, "end of file stays the end of file");
    assert!(
        matches!(r.app.assistant.items[idx].card_state(), CardState::Applied),
        "{:?}",
        r.app.assistant.items
    );
    let items = &r.app.assistant.items;
    assert!(
        matches!(&items[0], Item::User { text, sent, chips } if text == "Please change the greeting" && sent.contains("[Context from the IDE]") && chips[0] == "unit1.pas"),
        "{items:?}"
    );
    assert!(items.iter().any(
        |i| matches!(i, Item::Tool(t) if t.label == "Edit unit1.pas" && t.state == ToolState::Done)
    ));
    assert_eq!(items.last(), Some(&Item::Assistant("All **done**.".into())));
    assert_eq!(r.app.assistant.usage.output_tokens, 7);
    // what the CLI was sent: the context in the message, and an allow with the original input
    let user = stdin.iter().find(|j| j["type"] == "user").unwrap();
    assert!(
        user["message"]["content"]
            .as_str()
            .unwrap()
            .contains("Please change the greeting")
    );
    let resp = stdin
        .iter()
        .find(|j| j["type"] == "control_response")
        .unwrap();
    assert_eq!(resp["response"]["request_id"], "rq1");
    let body = &resp["response"]["response"];
    assert_eq!(body["behavior"], "allow");
    assert_eq!(body["updatedInput"]["old_string"], GREETING);
    assert_eq!(body["updatedInput"]["new_string"], "'Fixed'");
    assert!(
        body["updatedInput"]["file_path"]
            .as_str()
            .unwrap()
            .ends_with("/unit1.pas")
    );
    // a manual apply offers no Revert
    assert!(matches!(&items[idx], Item::Card(c) if !c.auto && c.undo.is_none()));
}

impl Item {
    fn card_state(&self) -> &CardState {
        match self {
            Item::Card(c) => &c.state,
            other => panic!("not a card: {other:?}"),
        }
    }
}

#[test]
fn a_caret_before_the_edit_stays_put_after_the_reload() {
    let mut r = cli_start("assist-cli-caret", None, false, false);
    let (idx, _) = r.card();
    let tab = r.app.docs[0].editor_id;
    let before_edit = char_at(&r.app.docs[0].text, "uses");
    set_caret(&r.ctx, tab, before_edit);
    r.app.assistant_decide(idx, true);
    r.finish();
    assert_eq!(caret(&r.ctx, tab), before_edit);
}

#[test]
fn a_claude_code_turn_with_a_rejected_edit_changes_nothing() {
    let mut r = cli_start("assist-cli-no", None, false, false);
    let (idx, _) = r.card();
    r.app.assistant_decide(idx, false);
    let stdin = r.finish();
    let doc = &r.app.docs[0];
    assert!(doc.text.contains(GREETING) && !doc.text.contains("'Fixed'"));
    assert!(
        std::fs::read_to_string(r.unit())
            .unwrap()
            .contains(GREETING)
    );
    assert_eq!(
        r.app.assistant.items[idx].card_state(),
        &CardState::Rejected
    );
    let resp = stdin
        .iter()
        .find(|j| j["type"] == "control_response")
        .unwrap();
    assert_eq!(resp["response"]["response"]["behavior"], "deny");
    let msg = resp["response"]["response"]["message"].as_str().unwrap();
    assert!(msg.contains("rejected"));
}

#[test]
fn a_dirty_tab_must_be_saved_before_claude_codes_tool_may_edit_it() {
    let mut r = cli_start(
        "assist-cli-dirty",
        Some("// my unsaved line\n"),
        false,
        false,
    );
    let (idx, _) = r.card();
    let unit = r.unit().canonicalize().unwrap();
    assert!(r.app.unsaved_in(&unit));
    // a plain Apply is refused: nothing is sent, nothing is saved
    r.app.assistant_decide(idx, true);
    assert_eq!(r.app.assistant.items[idx].card_state(), &CardState::Pending);
    assert!(
        !std::fs::read_to_string(r.unit())
            .unwrap()
            .contains("my unsaved line")
    );
    // Save and apply: the tab is saved, then the CLI edits the saved text
    r.app.assistant_save_and_apply(idx);
    assert_eq!(r.app.assistant.items[idx].card_state(), &CardState::Writing);
    r.finish();
    let disk = std::fs::read_to_string(r.unit()).unwrap();
    assert!(
        disk.contains("my unsaved line") && disk.contains("'Fixed'"),
        "{disk}"
    );
    assert_eq!(r.app.docs[0].text, disk);
    assert!(!r.app.docs[0].dirty());
}

#[test]
fn auto_mode_saves_a_dirty_tab_first_and_applies_without_a_click_for_claude_code() {
    let mut r = cli_start("assist-cli-auto", Some("// my unsaved line\n"), false, true);
    let stdin = r.finish();
    let (idx, c) = r.card();
    assert!(c.auto && c.state == CardState::Applied, "{c:?}");
    let disk = std::fs::read_to_string(r.unit()).unwrap();
    assert!(disk.contains("my unsaved line") && disk.contains("'Fixed'"));
    assert_eq!(r.app.docs[0].text, disk);
    assert_eq!(
        stdin
            .iter()
            .find(|j| j["type"] == "control_response")
            .unwrap()["response"]["response"]["behavior"],
        "allow"
    );
    // Revert: the previous text (with the saved dirty line) goes back to the disk and the tab
    let before = disk.replace("'Fixed'", GREETING);
    r.app.assistant_revert(idx);
    assert_eq!(
        r.app.assistant.items[idx].card_state(),
        &CardState::Reverted
    );
    assert_eq!(std::fs::read_to_string(r.unit()).unwrap(), before);
    assert_eq!(r.app.docs[0].text, before);
    assert!(!r.app.docs[0].dirty());
}

#[test]
fn revert_is_unavailable_once_the_file_changed_again() {
    let mut r = cli_start("assist-cli-revert-changed", None, false, true);
    r.finish();
    let (idx, _) = r.card();
    r.app.docs[0].text.push_str("// typed after\n");
    r.app.assistant_revert(idx);
    assert_eq!(r.app.assistant.items[idx].card_state(), &CardState::Applied);
    assert!(r.app.docs[0].text.contains("typed after"));
    assert!(r.app.docs[0].text.contains("'Fixed'"));
}

#[test]
fn a_failing_write_is_shown_on_the_card_and_changes_nothing() {
    let mut r = cli_start("assist-cli-fail", None, true, false);
    let (idx, _) = r.card();
    let original = r.app.docs[0].text.clone();
    r.app.assistant_decide(idx, true);
    r.finish();
    match r.app.assistant.items[idx].card_state() {
        CardState::Failed(m) => assert!(m.contains("modified since read"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.app.docs[0].text, original);
    assert_eq!(std::fs::read_to_string(r.unit()).unwrap(), original);
}

#[test]
fn a_new_file_written_by_claude_code_opens_in_a_tab_and_can_be_reverted() {
    let dir = scratch("assist-cli-create");
    let input = json!({"file_path":"{ROOT}/extra.pas","content":"unit extra;\ninterface\nimplementation\nend.\n"});
    let script = tool_script(
        "Write",
        input,
        Some((
            "extra.pas",
            "unit extra;\ninterface\nimplementation\nend.\n",
        )),
        ("File created successfully at: extra.pas", false),
    );
    let (bin, logs) = fake_cli("assist-cli-create", &script);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::ClaudeCode;
    app.assistant.cli_bin = Some(bin);
    app.assistant.cli = cli_found(Some(true));
    app.assistant.set_auto_edits(true);
    let ctx = egui::Context::default();
    app.assistant.input = "make a unit".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the end of the turn", |a| {
        a.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.state == CardState::Applied))
            && !a.assistant.streaming()
    });
    let new = dir.join("extra.pas");
    assert!(new.is_file());
    assert_eq!(app.docs[app.active].path, new, "the new file is opened");
    assert_eq!(
        app.docs[app.active].text,
        std::fs::read_to_string(&new).unwrap()
    );
    let idx = app
        .assistant
        .items
        .iter()
        .position(|i| matches!(i, Item::Card(_)))
        .unwrap();
    assert!(matches!(&app.assistant.items[idx], Item::Card(c) if c.path == "extra.pas" && c.auto));
    let _ = logs;
    // Revert removes the file and closes the tab
    app.assistant_revert(idx);
    assert!(!new.exists());
    assert!(app.docs.iter().all(|d| d.path != new));
}

#[test]
fn a_form_edited_by_claude_code_reloads_the_designer() {
    let dir = scratch("assist-cli-form");
    let lfm_after = std::fs::read_to_string(dir.join("unit1.lfm"))
        .unwrap()
        .replace("Caption = 'Click me'", "Caption = 'Press'");
    let input = json!({"file_path":"{ROOT}/unit1.lfm","old_string":"Caption = 'Click me'","new_string":"Caption = 'Press'"});
    let script = tool_script(
        "Edit",
        input,
        Some(("unit1.lfm", &lfm_after)),
        ("updated", false),
    );
    let (bin, _) = fake_cli("assist-cli-form", &script);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.show_form();
    assert_eq!(app.designers.len(), 1);
    app.assistant.provider = Provider::ClaudeCode;
    app.assistant.cli_bin = Some(bin);
    app.assistant.cli = cli_found(Some(true));
    let ctx = egui::Context::default();
    app.assistant.input = "rename the button".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "a proposal", |a| pending_card(a).is_some());
    let idx = pending_card(&app).unwrap();
    // a form with unsaved designer changes must be saved first as well
    let lfm = dir.join("unit1.lfm").canonicalize().unwrap();
    assert!(!app.unsaved_in(&lfm));
    let moved = app.designers[0]
        .doc
        .source()
        .replace("Top = 64", "Top = 65");
    app.designers[0].replace_source(&moved).unwrap();
    assert!(app.unsaved_in(&lfm));
    app.assistant_decide(idx, true);
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Pending);
    app.assistant_save_and_apply(idx);
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    // the CLI's fake wrote the text it was scripted with (without the saved move); the
    // designer shows exactly what the disk has and is clean
    let disk = std::fs::read_to_string(&lfm).unwrap();
    assert!(disk.contains("Caption = 'Press'"));
    assert_eq!(app.designers[0].doc.source(), disk);
    assert!(!app.designers[0].dirty());
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Applied);
}

#[test]
fn claude_code_not_logged_in_shows_the_login_steps_and_the_card() {
    let dir = scratch("assist-cli-login");
    let script = std::fs::read_to_string(cli_fixtures().join("not_logged_in.jsonl")).unwrap();
    let (bin, _) = fake_cli("assist-cli-login", &script);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::ClaudeCode;
    app.assistant.cli_bin = Some(bin);
    app.assistant.cli = cli_found(None);
    let ctx = egui::Context::default();
    app.assistant.input = "hi".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the error", |a| !a.assistant.streaming());
    let err = app
        .assistant
        .items
        .iter()
        .find_map(|i| match i {
            Item::Error(e) => Some(e.clone()),
            _ => None,
        })
        .expect("an error item");
    assert!(
        err.contains("run `claude` once") && err.contains("/login"),
        "{err}"
    );
    assert!(!err.contains("API key"));
    assert!(
        !app.assistant.ready(),
        "the panel shows the status card again"
    );
    assert!(matches!(&app.assistant.cli, CliState::Found(s) if s.logged_in == Some(false)));
}

#[test]
fn the_cli_is_looked_up_off_the_ui_thread() {
    let base = crate::target_tmp().join("assist-cli-status");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let bin = base.join("claude");
    std::fs::write(
        &bin,
        "#!/bin/sh\ncase \"$1\" in --version) echo '2.1.9 (Claude Code)';; auth) echo '{\"loggedIn\":true}';; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut a = Assistant {
        provider: Provider::ClaudeCode,
        cli_bin: Some(bin.clone()),
        ..Default::default()
    };
    assert!(!a.ready());
    let ctx = egui::Context::default();
    a.refresh_cli(&ctx);
    assert_eq!(a.cli, CliState::Checking);
    let start = Instant::now();
    while a.cli == CliState::Checking {
        assert!(start.elapsed() < Duration::from_secs(10));
        a.poll_cli();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        matches!(&a.cli, CliState::Found(s) if s.version.as_deref() == Some("2.1.9") && s.logged_in == Some(true) && s.binary == bin),
        "{:?}",
        a.cli
    );
    assert!(a.ready());
    // a program that is not there
    a.cli_bin = Some(base.join("nothing"));
    a.refresh_cli(&ctx);
    let start = Instant::now();
    while a.cli == CliState::Checking {
        assert!(start.elapsed() < Duration::from_secs(10));
        a.poll_cli();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        matches!(a.cli, CliState::Found(ref s) if s.version.is_none() && s.logged_in.is_none()),
        "{:?}",
        a.cli
    );
}

#[test]
fn the_provider_is_remembered_in_a_plain_text_file() {
    let _keychain = lock(&KEYCHAIN);
    let dir = crate::target_tmp().join("assist-provider");
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("Pascoda/assistant-provider");
    assert_eq!(
        Provider::load(&file),
        Provider::Api,
        "no file: the API key provider"
    );
    let mut a = Assistant {
        provider_file: Some(file.clone()),
        ..Default::default()
    };
    a.items.push(Item::Assistant("hello".into()));
    a.set_provider(Provider::ClaudeCode);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "claude-code");
    assert_eq!(Provider::load(&file), Provider::ClaudeCode);
    assert!(a.items.is_empty(), "switching agents empties the chat");
    a.set_provider(Provider::Api);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "api");
    std::fs::write(&file, "garbage").unwrap();
    assert_eq!(Provider::load(&file), Provider::Api);
    // no switching while a turn runs
    assert_eq!(Provider::Api.label(), "Claude Console API");
    assert_eq!(Provider::ClaudeCode.label(), "Claude Code CLI");
}

#[test]
fn readiness_depends_on_the_provider() {
    let mut a = Assistant::default();
    assert!(!a.ready());
    a.use_key("k");
    assert!(a.ready());
    a.provider = Provider::ClaudeCode;
    assert!(!a.ready(), "an API key does not make Claude Code ready");
    for (cli, ready) in [
        (CliState::Missing, false),
        (CliState::Checking, false),
        (cli_found(Some(false)), false),
        (cli_found(Some(true)), true),
        (cli_found(None), true),
    ] {
        a.cli = cli;
        assert_eq!(a.ready(), ready, "{:?}", a.cli);
    }
}

// ---- the OpenAI provider ----------------------------------------------------------------------------

fn oa_chunk(delta: Value, finish: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}

fn oa_text(text: &str) -> String {
    oa_chunk(json!({"role":"assistant","content":text}), None)
        + &oa_chunk(json!({}), Some("stop"))
        + &format!(
            "data: {}\n\n",
            json!({"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":6}})
        )
        + "data: [DONE]\n\n"
}

fn oa_tools(calls: &[(&str, &str, Value)]) -> String {
    let mut s = String::new();
    for (i, (id, name, input)) in calls.iter().enumerate() {
        s += &oa_chunk(
            json!({"tool_calls":[{"index":i,"id":id,"type":"function","function":{"name":name,"arguments":""}}]}),
            None,
        );
        let j = input.to_string();
        let chars: Vec<char> = j.chars().collect();
        for piece in chars.chunks(chars.len() / 2 + 1) {
            s += &oa_chunk(
                json!({"tool_calls":[{"index":i,"function":{"arguments":piece.iter().collect::<String>()}}]}),
                None,
            );
        }
    }
    s + &oa_chunk(json!({}), Some("tool_calls"))
        + &format!(
            "data: {}\n\n",
            json!({"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9}})
        )
        + "data: [DONE]\n\n"
}

/// The Keychain does not like several threads at once (calls stall for a minute), so the tests
/// that use it take turns.
pub(crate) static KEYCHAIN: Mutex<()> = Mutex::new(());

/// An assistant whose Keychain entry is a throwaway one, so no test touches the real one.
fn openai_assistant(tag: &str) -> Assistant {
    Assistant {
        provider: Provider::OpenAi,
        openai_key_state: KeyState::Missing,
        openai_keystore: Keystore::new(
            &format!("PascodaTest-oa-{tag}-{}", std::process::id()),
            "openai",
        ),
        ..Default::default()
    }
}

fn oa_turn(approve: bool, name: &str) -> (App, Vec<Value>) {
    let dir = scratch(name);
    let (url, bodies) = mock(vec![
        oa_tools(&[("t1", "read_file", json!({"path": "unit1.pas"}))]),
        oa_tools(&[(
            "t2",
            "edit_file",
            json!({"path":"unit1.pas","old_text":"'Hello from Pascora'","new_text":"'Fixed \u{e9}'"}),
        )]),
        oa_text("All **done**."),
    ]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.docs[0].text.push_str("// unsaved marker\n");
    app.assistant.provider = Provider::OpenAi;
    app.assistant.use_openai_key("oa-test-key-not-real");
    app.assistant.openai_base_url = Some(format!("{url}/v1"));
    app.assistant.openai.model = "test-model".into();
    let ctx = egui::Context::default();
    app.assistant.input = "Please change the greeting".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "a proposal", |a| pending_card(a).is_some());
    let idx = pending_card(&app).unwrap();
    assert!(
        !app.docs[0].text.contains("Fixed"),
        "nothing changes before the answer"
    );
    app.assistant_decide(idx, approve);
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    let seen = bodies.lock().unwrap().clone();
    (app, seen)
}

#[test]
fn an_openai_turn_with_an_approved_edit_uses_the_same_cards() {
    let (app, requests) = oa_turn(true, "assist-oa-ok");
    assert!(
        app.docs[0]
            .text
            .contains("Label1.Caption := 'Fixed \u{e9}';")
    );
    let items = &app.assistant.items;
    assert!(items.iter().any(
        |i| matches!(i, Item::Tool(t) if t.label == "Read unit1.pas" && t.state == ToolState::Done)
    ));
    assert!(
        items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.state == CardState::Applied))
    );
    assert_eq!(items.last(), Some(&Item::Assistant("All **done**.".into())));
    assert_eq!(app.assistant.usage.input_tokens, 40 + 50 + 50);
    assert_eq!(app.assistant.usage.output_tokens, 6 + 9 + 9);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["model"], "test-model");
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
    let first = requests[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        first.contains("[Context from the IDE]") && first.ends_with("Please change the greeting")
    );
    assert!(
        requests
            .iter()
            .all(|r| !r.to_string().contains("oa-test-key"))
    );
    // unsaved text reached the model through the tool message, with its call id
    let tool = &requests[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "t1");
    assert!(tool["content"].as_str().unwrap().contains("unsaved marker"));
    let tool = &requests[2]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(tool["tool_call_id"], "t2");
    assert!(
        tool["content"]
            .as_str()
            .unwrap()
            .contains("approved the change")
    );
}

#[test]
fn an_openai_turn_with_a_rejected_edit_changes_nothing() {
    let (app, requests) = oa_turn(false, "assist-oa-no");
    assert!(
        app.docs[0].text.contains("'Hello from Pascora'") && !app.docs[0].text.contains("Fixed")
    );
    assert!(
        app.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.state == CardState::Rejected))
    );
    assert!(requests[2].to_string().contains("rejected this change"));
}

#[test]
fn stop_ends_an_openai_turn_waiting_for_approval() {
    let dir = scratch("assist-oa-stop");
    let (url, _) = mock(vec![oa_tools(&[(
        "t1",
        "edit_file",
        json!({"path":"unit1.pas","old_text":"'Hello from Pascora'","new_text":"'x'"}),
    )])]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::OpenAi;
    app.assistant.use_openai_key("k");
    app.assistant.openai_base_url = Some(format!("{url}/v1"));
    app.assistant.openai.model = "m".into();
    let ctx = egui::Context::default();
    app.assistant.input = "go".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "a proposal", |a| pending_card(a).is_some());
    app.assistant.stop();
    wait(&mut app, &ctx, "the stop", |a| !a.assistant.streaming());
    assert!(matches!(app.assistant.items.last(), Some(Item::Note(n)) if n == "Stopped."));
    app.assistant.clear();
    assert!(app.assistant.items.is_empty());
}

#[test]
fn an_empty_model_is_explained_and_nothing_is_sent() {
    let dir = scratch("assist-oa-nomodel");
    let (url, bodies) = mock(vec![oa_text("x")]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::OpenAi;
    app.assistant.use_openai_key("k");
    app.assistant.openai_base_url = Some(format!("{url}/v1"));
    let ctx = egui::Context::default();
    app.assistant.input = "hello".into();
    assert!(!app.assistant_send(&ctx));
    assert!(
        matches!(app.assistant.items.last(), Some(Item::Error(e)) if e.contains("No model is set"))
    );
    assert_eq!(app.assistant.input, "hello", "the message stays in the box");
    std::thread::sleep(Duration::from_millis(100));
    assert!(bodies.lock().unwrap().is_empty());
}

/// A server that answers every request with this status and body.
fn status_server(code: u16, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = body.to_string();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            if read_body(&mut conn, &mut String::new()).is_none() {
                continue;
            }
            let _ = write!(
                conn,
                "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[test]
fn a_401_from_the_server_clears_the_key_and_brings_back_the_card() {
    let dir = scratch("assist-oa-401");
    let url = status_server(
        401,
        r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","code":"invalid_api_key"}}"#,
    );
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::OpenAi;
    app.assistant.use_openai_key("sk-wrong");
    app.assistant.openai.model = "m".into();
    // The official address is fixed; the test points the provider at the local server.
    app.assistant.openai_base_url = Some(url.replace("127.0.0.1", "localhost"));
    let ctx = egui::Context::default();
    app.assistant.input = "hi".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the error", |a| !a.assistant.streaming());
    let a = &app.assistant;
    assert!(matches!(a.items.last(), Some(Item::Error(e)) if e.contains("Incorrect API key")));
    assert_eq!(a.openai_key_state, KeyState::Missing);
    assert!(a.key_message.as_deref().unwrap_or("").contains("rejected"));
    assert!(!a.key_message.as_deref().unwrap().contains("sk-wrong"));
    // The official endpoint needs a key: the key card comes back.
    assert!(!a.ready() && a.show_settings);
}

#[test]
fn a_401_from_openai_itself_shows_the_key_card() {
    let mut a = openai_assistant("401");
    a.use_openai_key("sk-wrong");
    assert!(a.ready());
    a.on_event(
        Event::Error(pascoda_ai::AiError::Auth("bad key".into())),
        &none,
    );
    assert!(!a.ready(), "api.openai.com needs a key");
    assert_eq!(a.openai_key_state, KeyState::Missing);
    assert!(a.key_message.is_some() && a.show_settings, "the key card comes back");
    // The Anthropic key is not touched by an OpenAI failure
    let mut b = openai_assistant("401b");
    b.use_key("anthropic");
    b.use_openai_key("sk");
    b.on_event(Event::Error(pascoda_ai::AiError::Auth("x".into())), &none);
    assert_eq!(b.key, KeyState::Keychain);
}

#[test]
fn openai_readiness_needs_the_key() {
    let mut a = openai_assistant("ready");
    assert!(!a.ready(), "OpenAI itself needs a key");
    a.use_key("anthropic-key");
    assert!(!a.ready(), "an Anthropic key does not help");
    a.use_openai_key("sk");
    assert!(a.ready());
    assert_eq!(a.openai.base_url, "https://api.openai.com/v1", "the endpoint is the official one");
}

#[test]
fn the_openai_settings_are_saved_without_the_key() {
    let _keychain = lock(&KEYCHAIN);
    let dir = crate::target_tmp().join("assist-oa-settings");
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("Pascoda/openai-settings");
    let mut a = openai_assistant("settings");
    a.openai_file = Some(file.clone());
    a.openai_edit = OpenAiSettings {
        base_url: "  http://localhost:11434/v1 ".into(),
        model: " llama3.1 ".into(),
    };
    a.key_input = "sk-TOPSECRET".into();
    a.save_key();
    // The endpoint is fixed: a typed address is ignored, only the model is kept.
    assert_eq!(a.openai.base_url, "https://api.openai.com/v1");
    assert_eq!(a.openai.model, "llama3.1");
    assert_eq!(a.openai_key_state, KeyState::Keychain);
    assert!(a.key_input.is_empty());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains("TOPSECRET") && text.contains("llama3.1"),
        "{text}"
    );
    assert_eq!(OpenAiSettings::load(&file), a.openai);
    // the key went to the (throwaway) Keychain service, and Remove key takes it out again
    assert!(a.openai_keystore.load().unwrap().is_some());
    a.remove_key();
    assert!(a.openai_keystore.load().unwrap().is_none());
    // Typing the model in the chat updates the file; the endpoint stays the official one
    a.openai_edit.base_url = "  ".into();
    a.commit_openai();
    assert_eq!(a.openai.base_url, "https://api.openai.com/v1");
    a.openai.model = "m2".into();
    a.persist_openai();
    assert_eq!(OpenAiSettings::load(&file).model, "m2");
    // Save without a key for OpenAI itself says so, and keeps no key
    a.key_input.clear();
    a.save_key();
    assert!(
        a.key_message
            .as_deref()
            .unwrap()
            .contains("Paste your OpenAI API key")
    );
    assert!(!a.ready());
}

#[test]
fn the_keychain_is_read_only_for_the_openai_provider() {
    let _keychain = lock(&KEYCHAIN);
    let svc = format!("PascodaTest-oa-lazy-{}", std::process::id());
    let store = Keystore::new(&svc, "openai");
    store.save(&ApiKey::new("sk-stored")).unwrap();
    let mut a = Assistant {
        openai_keystore: store.clone(),
        key: KeyState::Missing, // the Anthropic side is settled, so it reads nothing either
        ..Default::default()
    };
    a.set_provider(Provider::ClaudeCode);
    a.set_provider(Provider::Api);
    assert_eq!(a.openai_key_state, KeyState::Unchecked, "not looked up");
    assert!(a.openai_key.is_none());
    a.set_provider(Provider::OpenAi);
    assert_eq!(a.openai_key_state, KeyState::Keychain);
    assert!(a.ready());
    store.delete().unwrap();
}

#[test]
fn the_provider_file_knows_openai_and_the_switch_resets_the_card() {
    let dir = crate::target_tmp().join("assist-oa-provider");
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("Pascoda/assistant-provider");
    let mut a = openai_assistant("prov");
    a.provider = Provider::Api;
    a.provider_file = Some(file.clone());
    a.key = KeyState::Missing;
    a.key_message = Some("old message".into());
    a.key_input = "half typed".into();
    a.set_provider(Provider::OpenAi);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "openai");
    assert_eq!(Provider::load(&file), Provider::OpenAi);
    assert!(a.key_message.is_none() && a.key_input.is_empty());
    assert_eq!(Provider::OpenAi.label(), "OpenAI API");
    assert_eq!(Provider::ALL.len(), 8);
}

#[test]
fn the_privacy_text_names_where_the_text_goes() {
    let t = ui::privacy_openai();
    assert!(t.contains("OpenAI (api.openai.com)"));
    assert!(!t.contains("Anthropic") && !t.contains("Claude"));
}

// ---- "Allow edits this session" ---------------------------------------------------------------

/// A proposal as an agent would send it, put in the conversation like the poller does.
fn propose(app: &mut App, id: &str, abs: &Path, change: Change) -> usize {
    let p = Proposal {
        id: id.into(),
        path: app.display_name(abs),
        abs_path: abs.to_path_buf(),
        change,
    };
    let (docs, designers) = (&app.docs, &app.designers);
    app.assistant.on_event(Event::NeedsApproval(p), &|p| {
        current_text(docs, designers, p)
    });
    app.assistant.items.len() - 1
}

#[test]
fn the_switch_is_off_at_startup_and_not_remembered() {
    let mut a = Assistant::default();
    assert!(!a.auto_edits);
    a.set_auto_edits(true);
    assert!(a.auto_edits);
    a.set_auto_edits(false);
    assert!(!a.auto_edits);
    // not remembered: a new app starts off
    assert!(!App::bare(None).assistant.auto_edits);
}

#[test]
fn auto_mode_applies_a_tab_edit_at_once_and_revert_restores_it_exactly() {
    let dir = scratch("assist-auto-tab");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    let original = app.docs[0].text.clone();
    app.assistant.set_auto_edits(true);
    let unit = app.docs[0].path.clone();
    let idx = propose(&mut app, "t1", &unit, edit(GREETING, "'Auto'"));
    app.assistant_auto_decide();
    let Item::Card(c) = &app.assistant.items[idx] else {
        panic!()
    };
    assert!(c.auto && c.state == CardState::Applied, "{c:?}");
    assert!(app.docs[0].text.contains("'Auto'"));
    assert!(
        app.docs[0].dirty(),
        "the API providers' edits stay in the tab"
    );
    assert_eq!(
        std::fs::read_to_string(&unit).unwrap(),
        original,
        "the disk is untouched"
    );
    app.assistant_revert(idx);
    assert_eq!(app.docs[0].text, original);
    assert!(!app.docs[0].dirty());
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Reverted);
}

#[test]
fn revert_of_a_tab_edit_is_refused_after_more_typing() {
    let dir = scratch("assist-auto-tab-changed");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.set_auto_edits(true);
    let unit = app.docs[0].path.clone();
    let idx = propose(&mut app, "t1", &unit, edit(GREETING, "'Auto'"));
    app.assistant_auto_decide();
    app.docs[0].text.push_str("// more\n");
    app.assistant_revert(idx);
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Applied);
    assert!(app.docs[0].text.contains("'Auto'") && app.docs[0].text.contains("// more"));
    assert!(
        matches!(app.assistant.items.last(), Some(Item::Note(n)) if n.contains("Could not revert"))
    );
}

#[test]
fn auto_mode_applies_to_files_that_are_not_open_by_writing_and_reverts_by_writing() {
    let dir = scratch("assist-auto-disk");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.set_auto_edits(true);
    let lpr = dir.join("hello.lpr");
    let original = std::fs::read_to_string(&lpr).unwrap();
    let word = original.split_whitespace().next().unwrap().to_string();
    let idx = propose(&mut app, "t1", &lpr, edit(&word, "PROGRAM"));
    app.assistant_auto_decide();
    assert!(
        std::fs::read_to_string(&lpr)
            .unwrap()
            .starts_with("PROGRAM")
    );
    app.assistant_revert(idx);
    assert_eq!(std::fs::read_to_string(&lpr).unwrap(), original);
    // someone else edits the file: no revert
    let idx = propose(&mut app, "t2", &lpr, edit(&word, "PROGRAM"));
    app.assistant_auto_decide();
    std::fs::write(&lpr, "changed elsewhere").unwrap();
    app.assistant_revert(idx);
    assert_eq!(std::fs::read_to_string(&lpr).unwrap(), "changed elsewhere");
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Applied);
}

#[test]
fn auto_mode_applies_a_form_edit_through_the_designer_and_revert_restores_form_and_unit() {
    let dir = scratch("assist-auto-form");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.show_form();
    app.assistant.set_auto_edits(true);
    let lfm = dir.join("unit1.lfm");
    let (form_before, unit_before) = (
        app.designers[0].doc.source().to_string(),
        app.docs[0].text.clone(),
    );
    let idx = propose(
        &mut app,
        "t1",
        &lfm,
        edit(
            "  object Label1: TLabel",
            "  object Edit1: TEdit\n    Left = 16\n    Height = 28\n    Top = 100\n    Width = 120\n    OnChange = Edit1Change\n  end\n  object Label1: TLabel",
        ),
    );
    app.assistant_auto_decide();
    assert!(app.designers[0].doc.source().contains("Edit1: TEdit"));
    assert!(app.docs[0].text.contains("procedure TForm1.Edit1Change"));
    app.assistant_revert(idx);
    assert_eq!(app.designers[0].doc.source(), form_before);
    assert_eq!(app.docs[0].text, unit_before, "the unit goes back too");
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Reverted);
}

#[test]
fn auto_mode_creates_files_and_revert_deletes_them() {
    let dir = scratch("assist-auto-create");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.set_auto_edits(true);
    let new = dir.join("made.pas");
    let idx = propose(
        &mut app,
        "t1",
        &new,
        Change::Create {
            content: "unit made;\n".into(),
        },
    );
    app.assistant_auto_decide();
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "unit made;\n");
    assert_eq!(app.docs[app.active].path, new);
    app.assistant_revert(idx);
    assert!(!new.exists() && app.docs.iter().all(|d| d.path != new));
}

#[test]
fn auto_mode_never_answers_a_build_and_leaves_local_cards_alone() {
    let dir = scratch("assist-auto-build");
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.set_auto_edits(true);
    let root = dir.clone();
    let idx = propose(&mut app, "b1", &root, Change::Build);
    app.assistant_auto_decide();
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Pending);
    app.assistant_propose_block("unit unit1;\ninterface\nimplementation\nend.\n");
    app.assistant_auto_decide();
    assert!(
        matches!(app.assistant.items.last(), Some(Item::Card(c)) if c.local && c.state == CardState::Pending)
    );
}

#[test]
fn auto_mode_keeps_the_sandbox_and_a_normal_apply_does_too() {
    let dir = scratch("assist-auto-sandbox");
    let outside = crate::target_tmp().join("assist-auto-outside.txt");
    let _ = std::fs::remove_file(&outside);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.set_auto_edits(true);
    for abs in [
        outside.clone(),
        dir.join("../assist-auto-outside.txt"),
        PathBuf::from("/etc/pascoda-not-here"),
    ] {
        let idx = propose(
            &mut app,
            "t1",
            &abs,
            Change::Create {
                content: "x".into(),
            },
        );
        app.assistant_auto_decide();
        assert!(
            matches!(app.assistant.items[idx].card_state(), CardState::Failed(m) if m.contains("outside the project")),
            "{abs:?}: {:?}",
            app.assistant.items[idx]
        );
    }
    assert!(!outside.exists() && !Path::new("/etc/pascoda-not-here").exists());
    // the same refusal for a click
    app.assistant.set_auto_edits(false);
    let idx = propose(
        &mut app,
        "t2",
        &outside,
        Change::Create {
            content: "x".into(),
        },
    );
    app.assistant_decide(idx, true);
    assert!(matches!(
        app.assistant.items[idx].card_state(),
        CardState::Failed(_)
    ));
    assert!(!outside.exists());
}

#[test]
fn auto_mode_with_the_anthropic_api_applies_without_a_click() {
    let dir = scratch("assist-auto-api");
    let (url, bodies) = mock(vec![
        sse(
            &[
                json!({"type":"tool_use","id":"t2","name":"edit_file","input":{"path":"unit1.pas","old_text":GREETING,"new_text":"'Auto'"}}),
            ],
            "tool_use",
        ),
        sse(&[json!({"type":"text","text":"Done."})], "end_turn"),
    ]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.use_key("test-key-not-real");
    app.assistant.base_url = Some(url);
    app.assistant.set_auto_edits(true);
    let ctx = egui::Context::default();
    app.assistant.input = "change it".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    assert!(app.docs[0].text.contains("'Auto'"));
    assert!(app.assistant.items.iter().any(
        |i| matches!(i, Item::Card(c) if c.auto && c.state == CardState::Applied && c.undo.is_some())
    ));
    // the contract is unchanged: the edit was applied before the agent heard "approved"
    let seen = bodies.lock().unwrap().clone();
    assert!(seen[1].to_string().contains("approved the change"));
    // nothing but the key given above was involved: no Keychain/settings lookups
    assert_eq!(app.assistant.openai_key_state, KeyState::Unchecked);
}

#[test]
fn auto_mode_with_openai_applies_without_a_click() {
    let dir = scratch("assist-auto-oa");
    let (url, bodies) = mock(vec![
        oa_tools(&[(
            "t2",
            "edit_file",
            json!({"path":"unit1.pas","old_text":GREETING,"new_text":"'Auto'"}),
        )]),
        oa_text("Done."),
    ]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::OpenAi;
    app.assistant.use_openai_key("oa-test-key-not-real");
    app.assistant.openai_base_url = Some(format!("{url}/v1"));
    app.assistant.openai.model = "test-model".into();
    app.assistant.set_auto_edits(true);
    let ctx = egui::Context::default();
    app.assistant.input = "change it".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    assert!(app.docs[0].text.contains("'Auto'"));
    assert!(
        app.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Card(c) if c.auto && c.state == CardState::Applied))
    );
    assert!(
        bodies.lock().unwrap()[1]
            .to_string()
            .contains("approved the change")
    );
    assert_eq!(
        app.assistant.key,
        KeyState::Unchecked,
        "the Anthropic key was never looked up"
    );
}

#[test]
fn claude_code_in_auto_mode_reads_no_keychain_and_no_settings() {
    let r = cli_start("assist-cli-nokeys", None, false, true);
    assert_eq!(r.app.assistant.key, KeyState::Unchecked);
    assert_eq!(r.app.assistant.openai_key_state, KeyState::Unchecked);
    assert!(r.app.assistant.provider_file.is_none() && r.app.assistant.openai_file.is_none());
    // and the module source names no credential/settings file besides what it always had
    let src = include_str!("mod.rs");
    for bad in [
        ".claude.json",
        "credentials.json",
        "settings.json",
        ".credentials",
    ] {
        assert!(!src.contains(bad), "{bad}");
    }
}

#[test]
fn the_cursor_follows_the_edit() {
    assert_eq!(map_cursor("abc def ghi", "abc XYZ ghi", 2), 2);
    assert_eq!(
        map_cursor("abc def ghi", "abc XYZW ghi", 10),
        11,
        "after: shifted"
    );
    assert_eq!(
        map_cursor("abc def ghi", "abc XYZW ghi", 5),
        4,
        "inside: edit start"
    );
    assert_eq!(map_cursor("abc", "", 3), 0);
    assert_eq!(map_cursor("", "abc", 0), 0);
    assert_eq!(
        map_cursor("h\u{e9}llo", "h\u{e9}llo!", 5),
        5,
        "chars, not bytes"
    );
    assert!(map_cursor("a long text", "a", 11) <= 1);
}

// ---- project awareness -------------------------------------------------------------------------

fn notes_app(name: &str) -> (App, PathBuf) {
    let dir = overview::tests::notes_copy(name);
    let app = App::bare(Some(dir.join("mainunit.pas")));
    app.assistant
        .project
        .set_index(overview::Index::scan(&app.root));
    (app, dir)
}

#[test]
fn the_overview_uses_unsaved_text_and_names_the_chips_in_the_bubble() {
    let (mut app, dir) = notes_app("assist-awareness");
    let ctx = egui::Context::default();
    let first = app.assistant_snapshot(&ctx).overview.unwrap();
    assert!(first.contains("Project: assist-awareness") && first.contains("notes.lpr: program"));
    assert!(!first.contains("Zed_Unsaved"));
    // An unsaved edit of the open unit shows up; the file on disk is untouched.
    let before = std::fs::read_to_string(dir.join("mainunit.pas")).unwrap();
    app.docs[0].text =
        "unit mainunit;\ninterface\nuses Zed_Unsaved;\nimplementation\nend.\n".into();
    let snap = app.assistant_snapshot(&ctx);
    assert!(
        snap.overview
            .as_deref()
            .unwrap()
            .contains("mainunit.pas: unit mainunit; uses Zed_Unsaved")
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("mainunit.pas")).unwrap(),
        before
    );
    let (sent, labels) = compose(app.assistant.chips, &snap, "q");
    assert!(sent.contains("Project overview") && sent.contains("Zed_Unsaved"));
    assert_eq!(labels, vec!["mainunit.pas", "project"]);
    // The chip off: no overview computed or sent.
    app.assistant.chips.project = false;
    let snap = app.assistant_snapshot(&ctx);
    assert_eq!(snap.overview, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_snapshot_lists_other_tabs_with_unsaved_text_only_for_dirty_ones() {
    let (mut app, dir) = notes_app("assist-tabs");
    app.open_path(dir.join("settingsunit.pas"));
    app.open_path(dir.join("aboutunit.pas"));
    assert_eq!(app.docs.len(), 3);
    app.active = 0;
    let i = app
        .docs
        .iter()
        .position(|d| d.path.ends_with("aboutunit.pas"))
        .unwrap();
    app.docs[i].text.push_str("// edited, not saved\n");
    let ctx = egui::Context::default();
    let snap = app.assistant_snapshot(&ctx);
    assert_eq!(snap.tabs.len(), 2);
    assert!(snap.tabs.iter().all(|t| t.name != "mainunit.pas"));
    let about = snap
        .tabs
        .iter()
        .find(|t| t.name == "aboutunit.pas")
        .unwrap();
    assert!(
        about
            .unsaved
            .as_deref()
            .unwrap()
            .ends_with("// edited, not saved\n")
    );
    assert!(
        snap.tabs
            .iter()
            .find(|t| t.name == "settingsunit.pas")
            .unwrap()
            .unsaved
            .is_none()
    );
    app.assistant.chips.tabs = true;
    let (sent, labels) = compose(app.assistant.chips, &snap, "q");
    assert!(sent.contains("Other open tabs:") && sent.contains("aboutunit.pas (unsaved)"));
    assert!(sent.contains("// edited, not saved"));
    assert!(labels.contains(&"2 other tabs".to_string()), "{labels:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_prompts_tell_the_model_about_the_overview_and_to_read_first() {
    for p in [pascoda_ai::SYSTEM_PROMPT, pascoda_ai::CLAUDE_CODE_PROMPT] {
        assert!(p.contains("project overview") && p.contains("do not guess"));
    }
    assert!(pascoda_ai::SYSTEM_PROMPT.contains("read_file"));
    assert!(
        pascoda_ai::CLAUDE_CODE_PROMPT.contains("(Read)")
            && pascoda_ai::CLAUDE_CODE_PROMPT.contains("Grep")
    );
}

#[test]
fn hand_typed_claude_models_are_remembered_saved_and_reloaded() {
    let dir = crate::target_tmp().join("claude-models");
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("claude-models");
    let mut a = Assistant::default();
    a.model_file = Some(file.clone());
    // built-in models are not added to the list
    a.model = pascoda_ai::MODEL_OPUS.into();
    a.remember_model();
    assert!(a.extra_models.is_empty() && !file.exists());
    // a new one is, newest first, without duplicates
    a.model = "claude-future-6".into();
    a.remember_model();
    a.model = "claude-other-1".into();
    a.remember_model();
    a.model = "claude-future-6".into();
    a.remember_model();
    assert_eq!(a.extra_models, ["claude-future-6", "claude-other-1"]);
    // a new session finds the chosen model and the list
    let mut b = Assistant::default();
    b.load_claude_models(&file);
    assert_eq!(b.model, "claude-future-6");
    assert_eq!(b.extra_models, ["claude-future-6", "claude-other-1"]);
    // the list is capped and blank input is ignored
    for i in 0..15 {
        b.model = format!("m{i}");
        b.model_file = Some(file.clone());
        b.remember_model();
    }
    assert_eq!(b.extra_models.len(), 10);
    assert_eq!(b.extra_models[0], "m14");
    b.model = "  ".into();
    b.remember_model();
    assert_eq!(b.extra_models.len(), 10);
}

// ---- the Codex CLI provider ----------------------------------------------------------------------

fn codex_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../pascoda-ai/tests/fixtures/codex/fake_codex.py")
}

/// A `codex` stand-in playing `turns` (see pascoda-ai's fixture folder).
fn fake_codex(name: &str, turns: Value) -> PathBuf {
    let base = crate::target_tmp().join(format!("{name}-codex"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("script.json"), turns.to_string()).unwrap();
    let wrapper = base.join("codex");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport FAKE_CODEX_SCRIPT='{}' FAKE_CODEX_LOG='{}'\nexec '{}' \"$@\"\n",
            base.join("script.json").display(),
            base.join("log.jsonl").display(),
            codex_fixture().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

fn codex_found(logged_in: Option<bool>) -> CliState {
    CliState::Found(CliStatus {
        binary: "/usr/local/bin/codex".into(),
        version: Some("0.165.0".into()),
        logged_in,
    })
}

/// The turn of a Codex edit of the greeting in unit1.pas: ask, write on accept, finish.
fn codex_edit_turn(unit: &Path) -> Value {
    let text = std::fs::read_to_string(unit).unwrap();
    let (n, line) = text
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(GREETING))
        .unwrap();
    let new_line = line.replace(GREETING, "'Fixed'");
    let diff = format!("@@ -{0},1 +{0},1 @@\n-{line}\n+{new_line}\n", n + 1);
    let after = text.replace(GREETING, "'Fixed'");
    let changes = json!([{"path": unit.to_str().unwrap(), "kind": {"type":"update","move_path":null}, "diff": diff}]);
    let item =
        |status: &str| json!({"type":"fileChange","id":"fc1","changes":changes,"status":status});
    json!([[
        {"notify":{"method":"item/started","params":{"item":item("inProgress"),"threadId":"{THREAD}","turnId":"{TURN}"}}},
        {"notify":{"method":"item/agentMessage/delta","params":{"threadId":"{THREAD}","turnId":"{TURN}","itemId":"m1","delta":"Changing it."}}},
        {"request":{"id":100,"method":"item/fileChange/requestApproval","params":{"threadId":"{THREAD}","turnId":"{TURN}","itemId":"fc1","startedAtMs":1}}},
        {"write":{"path":unit.to_str().unwrap(),"text":after}},
        {"notify":{"method":"item/completed","params":{"item":item("completed"),"threadId":"{THREAD}","turnId":"{TURN}"}}},
        {"notify":{"method":"turn/completed","params":{"threadId":"{THREAD}","turn":{"id":"{TURN}","status":"completed","items":[],"error":null}}}}
    ]])
}

fn codex_send(app: &mut App, ctx: &egui::Context, text: &str) {
    app.assistant.input = text.into();
    assert!(app.assistant_send(ctx));
}

#[test]
fn the_chooser_groups_claude_and_openai_agents_and_remembers_codex() {
    use std::collections::BTreeSet;
    let order: Vec<_> = Provider::ALL
        .iter()
        .map(|p| (p.vendor(), p.label()))
        .collect();
    assert_eq!(
        order,
        [
            ("Claude", "Claude Console API"),
            ("Claude", "Claude Code CLI"),
            ("OpenAI", "OpenAI API"),
            ("OpenAI", "Codex CLI"),
            ("Other servers", "oMLX"),
            ("Other servers", "Ollama"),
            ("Other servers", "Open WebUI"),
            ("Other servers", "OpenAI Compatible"),
        ]
    );
    // each vendor's providers sit together
    let mut seen = BTreeSet::new();
    let mut last = "";
    for p in Provider::ALL {
        if p.vendor() != last {
            assert!(seen.insert(p.vendor()), "{} appears twice", p.vendor());
            last = p.vendor();
        }
    }
    let file = crate::target_tmp().join("codex-provider").join("provider");
    let mut a = Assistant::default();
    a.provider_file = Some(file.clone());
    a.set_provider(Provider::Codex);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "codex");
    assert_eq!(Provider::load(&file), Provider::Codex);
    assert_eq!(Provider::Codex.short_name(), "Codex CLI");
}

#[test]
fn codex_is_ready_only_when_found_and_logged_in_and_the_model_is_remembered() {
    let mut a = Assistant::default();
    a.provider = Provider::Codex;
    assert!(!a.ready() && !a.settled(), "not looked for yet");
    a.codex = CliState::Missing;
    assert!(a.settled() && !a.ready());
    a.codex = codex_found(Some(false));
    assert!(a.settled() && !a.ready(), "found but not logged in");
    a.codex = codex_found(None);
    assert!(
        a.ready(),
        "unknown login state: let the first message find out"
    );
    a.codex = codex_found(Some(true));
    assert!(a.ready());
    // the other CLI's state does not count
    a.provider = Provider::ClaudeCode;
    assert!(!a.ready());
    // an empty model is fine (Codex's own default); a typed one is saved
    a.provider = Provider::Codex;
    assert_eq!(a.current_model(), "");
    let file = crate::target_tmp()
        .join("codex-model-test")
        .join("codex-model");
    a.codex_model_file = Some(file.clone());
    a.codex_model = " gpt-test ".into();
    a.persist_codex_model();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "gpt-test");
}

#[test]
fn a_codex_turn_with_an_approved_edit_is_written_by_codex_and_reloaded() {
    let dir = scratch("assist-codex-ok");
    let unit = dir.join("unit1.pas").canonicalize().unwrap();
    let mut app = App::bare(Some(unit.clone()));
    app.assistant.provider = Provider::Codex;
    app.assistant.codex_bin = Some(fake_codex("assist-codex-ok", codex_edit_turn(&unit)));
    app.assistant.codex = codex_found(Some(true));
    let ctx = egui::Context::default();
    codex_send(&mut app, &ctx, "Please change the greeting");
    wait(&mut app, &ctx, "a proposal", |a| {
        a.assistant.items.iter().any(|i| matches!(i, Item::Card(_)))
    });
    let idx = app
        .assistant
        .items
        .iter()
        .position(|i| matches!(i, Item::Card(_)))
        .unwrap();
    let before = app.docs[0].text.clone();
    assert!(app.assistant.backend_writes, "Codex writes the file itself");
    app.assistant_decide(idx, true);
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Writing);
    assert_eq!(app.docs[0].text, before, "the app does not touch the text");
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    let disk = std::fs::read_to_string(&unit).unwrap();
    assert!(disk.contains("'Fixed'") && !disk.contains(GREETING));
    assert_eq!(app.docs[0].text, disk, "the tab shows what Codex wrote");
    assert!(!app.docs[0].dirty());
    assert!(matches!(
        app.assistant.items[idx].card_state(),
        CardState::Applied
    ));
}

#[test]
fn a_rejected_codex_edit_leaves_the_file_alone() {
    let dir = scratch("assist-codex-no");
    let unit = dir.join("unit1.pas").canonicalize().unwrap();
    let original = std::fs::read_to_string(&unit).unwrap();
    let mut app = App::bare(Some(unit.clone()));
    app.assistant.provider = Provider::Codex;
    app.assistant.codex_bin = Some(fake_codex("assist-codex-no", codex_edit_turn(&unit)));
    app.assistant.codex = codex_found(Some(true));
    let ctx = egui::Context::default();
    codex_send(&mut app, &ctx, "change it");
    wait(&mut app, &ctx, "a proposal", |a| {
        a.assistant.items.iter().any(|i| matches!(i, Item::Card(_)))
    });
    let idx = app
        .assistant
        .items
        .iter()
        .position(|i| matches!(i, Item::Card(_)))
        .unwrap();
    app.assistant_decide(idx, false);
    wait(&mut app, &ctx, "the end of the turn", |a| {
        !a.assistant.streaming()
    });
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), original);
    assert_eq!(app.assistant.items[idx].card_state(), &CardState::Rejected);
}

#[test]
fn codex_not_logged_in_shows_the_login_steps() {
    let dir = scratch("assist-codex-login");
    let unit = dir.join("unit1.pas");
    let turn = json!([[{"notify":{"method":"turn/completed","params":{"threadId":"{THREAD}","turn":{
        "id":"{TURN}","status":"failed","items":[],
        "error":{"message":"401 Unauthorized","codexErrorInfo":"unauthorized"}}}}}]]);
    let mut app = App::bare(Some(unit));
    app.assistant.provider = Provider::Codex;
    app.assistant.codex_bin = Some(fake_codex("assist-codex-login", turn));
    app.assistant.codex = codex_found(Some(true));
    let ctx = egui::Context::default();
    codex_send(&mut app, &ctx, "hi");
    wait(&mut app, &ctx, "the error", |a| {
        a.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Error(_)))
    });
    assert!(matches!(&app.assistant.codex, CliState::Found(s) if s.logged_in == Some(false)));
    assert!(!app.assistant.ready());
    assert!(
        app.assistant.show_settings,
        "the card with the login steps is shown"
    );
    let msg = app
        .assistant
        .items
        .iter()
        .find_map(|i| match i {
            Item::Error(m) => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    assert!(msg.contains("codex login"), "{msg}");
}

fn compat_assistant(_tag: &str) -> Assistant {
    Assistant {
        provider: Provider::OpenAiCompatible,
        ..Default::default()
    }
}

#[test]
fn the_compatible_server_is_ready_once_it_has_an_address() {
    let mut a = compat_assistant("ready");
    assert!(!a.ready(), "no server named yet");
    a.compat_edit.base_url = "  http://127.0.0.1:11434/v1 ".into();
    a.commit_compat();
    assert!(a.ready(), "an address is enough: no key is asked for");
    assert_eq!(a.compat.base_url, "http://127.0.0.1:11434/v1", "trimmed");
    assert!(a.settled(), "nothing is looked up for this provider");
    a.compat_edit.base_url = "   ".into();
    a.commit_compat();
    assert!(!a.ready(), "a blank address is no server");
}

#[test]
fn the_compatible_server_is_saved_without_a_key_or_default_address() {
    let dir = scratch("assist-compat-settings");
    let file = dir.join("openai-compatible-settings");
    assert!(
        read_compat_file(&file).is_empty(),
        "a missing file names no server (the OpenAI default would be wrong here)"
    );
    let mut a = compat_assistant("settings");
    a.compat_file = Some(file.clone());
    a.compat_edit = OpenAiSettings {
        base_url: "http://localhost:1234/v1".into(),
        model: String::new(),
    };
    a.commit_compat();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.to_lowercase().contains("key"), "no key in the file: {text}");
    assert_eq!(
        read_compat_file(&file)["openai-compatible"].base_url,
        "http://localhost:1234/v1"
    );
}

#[test]
fn a_file_of_one_server_before_the_presets_is_the_generic_server() {
    let dir = scratch("assist-compat-old-file");
    let file = dir.join("openai-compatible-settings");
    std::fs::write(&file, r#"{"base_url":"http://10.0.0.5:9000/v1","model":"m"}"#).unwrap();
    let saved = read_compat_file(&file);
    assert_eq!(saved["openai-compatible"].base_url, "http://10.0.0.5:9000/v1");
    assert_eq!(saved["openai-compatible"].model, "m");
}

#[test]
fn each_compatible_server_keeps_its_own_address_model_and_key() {
    let _keychain = lock(&KEYCHAIN);
    let dir = scratch("assist-compat-servers");
    let file = dir.join("openai-compatible-settings");
    let mut a = Assistant {
        provider: Provider::OpenAi,
        compat_file: Some(file.clone()),
        compat_service: format!("PascodaTest-compat-{}", std::process::id()),
        ..Default::default()
    };
    // Ollama comes with its usual address and needs no key.
    a.set_provider(Provider::Ollama);
    assert_eq!(a.compat.base_url, "http://127.0.0.1:11434/v1");
    assert!(a.ready() && a.compat_key_state == KeyState::Missing);
    a.compat.model = "qwen3:4b".into();
    a.persist_compat();

    // Open WebUI: its own address, and a key kept under its own account.
    a.set_provider(Provider::OpenWebUi);
    assert_eq!(a.compat.base_url, "http://127.0.0.1:8080/api/v1");
    a.key_input = "sk-webui".into();
    a.save_compat_key();
    assert_eq!(a.compat_key_state, KeyState::Keychain);
    assert!(a.key_input.is_empty(), "the key field is cleared once saved");
    a.compat.model = "gemma".into();
    a.persist_compat();

    // oMLX does not see Open WebUI's key.
    a.set_provider(Provider::OMlx);
    assert_eq!(a.compat.base_url, "http://127.0.0.1:8000/v1");
    assert_eq!(a.compat_key_state, KeyState::Missing);

    // Back to Ollama: its model is still there, and Open WebUI's key is still stored for it.
    a.set_provider(Provider::Ollama);
    assert_eq!(a.compat.model, "qwen3:4b");
    a.set_provider(Provider::OpenWebUi);
    assert_eq!(a.compat.model, "gemma");
    assert_eq!(a.compat_key_state, KeyState::Keychain);
    a.remove_compat_key();
    assert_eq!(a.compat_key_state, KeyState::Missing);
    assert!(a.compat_keystore().load().unwrap().is_none());

    // Every server's address and model are in the file, with no key.
    let saved = read_compat_file(&file);
    assert_eq!(saved["ollama"].model, "qwen3:4b");
    assert_eq!(saved["open-webui"].base_url, "http://127.0.0.1:8080/api/v1");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("sk-webui"));
}

#[test]
fn a_turn_goes_to_the_compatible_server_with_its_model() {
    let dir = scratch("assist-compat-turn");
    let (url, bodies) = mock(vec![oa_text("Hello from the server.")]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.provider = Provider::OpenAiCompatible;
    app.assistant.compat_edit = OpenAiSettings {
        base_url: format!("{url}/v1"),
        model: String::new(),
    };
    app.assistant.commit_compat();
    app.assistant.compat.model = "local-model".into();
    let ctx = egui::Context::default();
    app.assistant.input = "hi".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the end of the turn", |a| !a.assistant.streaming());
    let seen = bodies.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["model"], "local-model");
    assert!(
        app.assistant
            .items
            .iter()
            .any(|i| matches!(i, Item::Assistant(t) if t.contains("Hello from the server."))),
        "the server's answer is shown"
    );
}

#[test]
fn switching_the_agent_empties_the_chat() {
    let mut a = openai_assistant("switch-empties");
    a.items.push(Item::Assistant("an old answer".into()));
    a.usage.input_tokens = 99;
    a.set_provider(Provider::Codex);
    assert!(a.items.is_empty(), "the chat of the previous agent is gone");
    assert_eq!(a.usage.input_tokens, 0);
    assert_eq!(a.provider, Provider::Codex);
}

/// Open WebUI wants `Authorization: Bearer <key>` on every request: the key saved for it is sent.
#[test]
fn open_webui_gets_the_saved_key_as_a_bearer_token() {
    let _keychain = lock(&KEYCHAIN);
    let dir = scratch("assist-compat-webui-key");
    let (url, bodies, auths) = mock_auth(vec![oa_text("hello")]);
    let mut app = App::bare(Some(dir.join("unit1.pas")));
    app.assistant.compat_service = format!("PascodaTest-webui-{}", std::process::id());
    app.assistant.set_provider(Provider::OpenWebUi);
    app.assistant.compat = OpenAiSettings {
        base_url: format!("{url}/api/v1"),
        model: "gemma".into(),
    };
    app.assistant.key_input = "sk-webui-test".into();
    app.assistant.save_compat_key();
    assert_eq!(app.assistant.compat_key_state, KeyState::Keychain);
    let ctx = egui::Context::default();
    app.assistant.input = "hi".into();
    assert!(app.assistant_send(&ctx));
    wait(&mut app, &ctx, "the end of the turn", |a| !a.assistant.streaming());
    assert_eq!(bodies.lock().unwrap().len(), 1);
    assert_eq!(auths.lock().unwrap()[0], "Bearer sk-webui-test");
    let _ = app.assistant.compat_keystore().delete();
}
