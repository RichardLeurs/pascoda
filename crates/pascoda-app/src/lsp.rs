//! Runs the language server on its own thread and talks to the UI over channels.

use eframe::egui;
use pascoda_lsp::{Client, CompletionItem, Config, FileEdit, Location, Symbol};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

pub enum Req {
    Complete {
        path: PathBuf,
        text: String,
        line: u32,
        col: u32,
    },
    Definition {
        path: PathBuf,
        text: String,
        line: u32,
        col: u32,
    },
    Hover {
        path: PathBuf,
        text: String,
        line: u32,
        col: u32,
    },
    References {
        path: PathBuf,
        text: String,
        line: u32,
        col: u32,
    },
    /// The signature of the call at the caret: `at` is the caret's char index and `key` the text's,
    /// both echoed back so a late answer can be recognised.
    Signature {
        path: PathBuf,
        text: String,
        line: u32,
        col: u32,
        at: usize,
        key: u64,
    },
    /// The declarations of a file. `key` identifies the text asked about, echoed back.
    Symbols {
        path: PathBuf,
        text: String,
        key: u64,
    },
    Rename {
        path: PathBuf,
        text: String,
        line: u32,
        col: u32,
        new_name: String,
    },
}

pub enum Resp {
    Ready,
    Completion(Vec<CompletionItem>),
    Definition(Option<Location>),
    /// The position the answer is for (0-based line, column), so late answers can be ignored.
    Hover(u32, u32, Option<String>),
    References(Vec<Location>),
    Signature {
        at: usize,
        key: u64,
        sig: Option<pascoda_lsp::Signature>,
    },
    Symbols {
        path: PathBuf,
        key: u64,
        symbols: Vec<Symbol>,
    },
    /// The replacements of a rename, per file.
    Rename(Vec<FileEdit>),
    Error(String),
}

pub struct Handle {
    pub requests: Sender<Req>,
    pub responses: Receiver<Resp>,
}

/// Starts the server for `root`; `None` when `pasls` has not been built.
pub fn spawn(root: &Path, ctx: egui::Context) -> Option<Handle> {
    let config = Config::find(root)?;
    let root = root.to_path_buf();
    let (requests, req_rx) = channel::<Req>();
    let (resp_tx, responses) = channel();
    std::thread::spawn(move || {
        let reply = |r: Resp| {
            let _ = resp_tx.send(r);
            ctx.request_repaint();
        };
        let mut client = match Client::start(&config, &root) {
            Ok(c) => c,
            Err(e) => return reply(Resp::Error(e)),
        };
        reply(Resp::Ready);
        while let Ok(req) = req_rx.recv() {
            let result = match req {
                Req::Complete {
                    path,
                    text,
                    line,
                    col,
                } => client
                    .sync(&path, &text)
                    .and_then(|_| client.completion(&path, line, col))
                    .map(Resp::Completion),
                Req::Definition {
                    path,
                    text,
                    line,
                    col,
                } => client
                    .sync(&path, &text)
                    .and_then(|_| client.definition(&path, line, col))
                    .map(Resp::Definition),
                Req::Hover {
                    path,
                    text,
                    line,
                    col,
                } => client
                    .sync(&path, &text)
                    .and_then(|_| client.hover(&path, line, col))
                    .map(|h| Resp::Hover(line, col, h.filter(|t| !t.is_empty()))),
                Req::References {
                    path,
                    text,
                    line,
                    col,
                } => client
                    .sync(&path, &text)
                    .and_then(|_| client.references(&path, line, col))
                    .map(Resp::References),
                Req::Signature {
                    path,
                    text,
                    line,
                    col,
                    at,
                    key,
                } => client
                    .sync(&path, &text)
                    .and_then(|_| client.signature_help(&path, line, col))
                    .map(|sig| Resp::Signature { at, key, sig }),
                Req::Symbols { path, text, key } => client
                    .sync(&path, &text)
                    .and_then(|_| client.document_symbols(&path))
                    .map(|symbols| Resp::Symbols { path, key, symbols }),
                Req::Rename {
                    path,
                    text,
                    line,
                    col,
                    new_name,
                } => client
                    .sync(&path, &text)
                    .and_then(|_| client.rename(&path, line, col, &new_name))
                    .map(Resp::Rename),
            };
            reply(result.unwrap_or_else(Resp::Error));
        }
        client.stop();
    });
    Some(Handle {
        requests,
        responses,
    })
}
