//! Keeps a program file (`.lpr`) in step with its forms: a new form's unit joins the `uses` clause
//! and gets an `Application.CreateForm` call before `Application.Run`, and a deleted unit is taken
//! out of both again. The edits are text patches, so the rest of the file stays as it was.

use std::path::{Path, PathBuf};

/// The form a unit declares: the `object Name: TClass` at the top of its `.lfm` or `.dfm`.
pub fn form_of_lfm(text: &str) -> Option<(String, String)> {
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim_start();
    let rest = ["object ", "inherited ", "inline "]
        .iter()
        .find_map(|k| line.strip_prefix(k))?;
    let (var, class) = rest.split_once(':')?;
    let class = class.split_whitespace().next()?;
    let var = var.trim();
    (!var.is_empty() && !class.is_empty()).then(|| (class.to_string(), var.to_string()))
}

/// The program file for a unit in `dir`: the first `.lpr` in `dir`, else in its parent folders up
/// to `root`. `None` when there is none.
pub fn program_for(dir: &Path, root: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if !d.starts_with(root) {
            break;
        }
        if let Some(p) = lpr_in(d) {
            return Some(p);
        }
        if d == root {
            break;
        }
        cur = d.parent();
    }
    None
}

fn lpr_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("lpr"))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Adds `unit` to the `uses` clause and `Application.CreateForm(class, var)` before
/// `Application.Run`. A unit or call that is already there is not added twice. Refuses a program
/// with no `Application.Run`, which has no place for the form.
pub fn add_form(src: &str, unit: &str, class: &str, var: &str) -> Result<String, String> {
    let nl = if src.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = src.to_string();
    if let Some((clause_start, clause_end)) = uses_clause(src) {
        let mask = masked(src);
        let names = idents(src, clause_start, clause_end, &mask);
        let listed = names.iter().any(|&(s, e)| src[s..e].eq_ignore_ascii_case(unit));
        if !listed {
            // Right after the last listed name, so a comment after it stays after the new unit.
            let last = names.last().map(|&(_, e)| e);
            let at = last.ok_or("the uses clause lists nothing")?;
            out.replace_range(at..at, &format!(", {unit}"));
        }
    } else {
        return Err("the program has no uses clause".into());
    }
    let call = format!("Application.CreateForm({class}, {var});");
    let wanted: String = call.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let has_call = out.lines().any(|l| {
        let t: String = l.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        t == wanted
    });
    if !has_call {
        let run = find_word(&out.to_ascii_lowercase(), "application.run")
            .ok_or("the program has no Application.Run, so the form has no place there")?;
        let line_start = out[..run].rfind('\n').map_or(0, |i| i + 1);
        let indent: String = out[line_start..run].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        out.insert_str(line_start, &format!("{indent}{call}{nl}"));
    }
    Ok(out)
}

/// Takes `unit` out of every `uses` clause (with its ` in '...'` part) and removes the
/// `Application.CreateForm` call of `form` (its class and variable). Returns the text unchanged
/// when neither is there.
pub fn remove_unit(src: &str, unit: &str, form: Option<(&str, &str)>) -> String {
    let mut out = src.to_string();
    // Last clause first, so the offsets of the clauses before it stay valid.
    for (cs, ce) in uses_clauses(src).into_iter().rev() {
        out = remove_in_clause(&out, cs, ce, unit);
    }
    if let Some((class, var)) = form {
        let wanted: String = format!("application.createform({class},{var});")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        let mut kept = String::new();
        for line in out.split_inclusive('\n') {
            let t: String = line.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
            if t != wanted {
                kept.push_str(line);
            }
        }
        out = kept;
    }
    out
}

/// Points the `uses` entry of `old` in every `uses` clause at `new` and at `rel_in` (the unit's path
/// from the file's folder, written as ` in '...'`; `None` drops the path). A unit that is not listed
/// is left alone, so the text is returned unchanged.
pub fn set_unit(src: &str, old: &str, new: &str, rel_in: Option<&str>) -> String {
    let mut out = src.to_string();
    for (cs, ce) in uses_clauses(src).into_iter().rev() {
        out = set_in_clause(&out, cs, ce, old, new, rel_in);
    }
    out
}

/// A path from the folder `from_dir` to `to`, with `../` where it leaves that folder. Used for the
/// `in '...'` paths of units in other folders.
pub fn relative_path(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to_parts: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to_parts).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(to_parts[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

/// The byte range of every `uses` clause in `src`, from after `uses` to its `;`, in file order.
fn uses_clauses(src: &str) -> Vec<(usize, usize)> {
    let mask = masked(src);
    let lower = src.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = find_word_masked(&lower[from..], "uses", &mask[from..]).map(|p| p + from) {
        let start = p + "uses".len();
        if let Some(end) = (start..src.len()).find(|&i| src.as_bytes()[i] == b';' && !mask[i]) {
            out.push((start, end));
            from = end;
        } else {
            break;
        }
    }
    out
}

fn set_in_clause(src: &str, cs: usize, ce: usize, old: &str, new: &str, rel_in: Option<&str>) -> String {
    let mask = masked(src);
    let names = idents(src, cs, ce, &mask);
    let Some(&(s, e)) = names.iter().find(|&&(s, e)| src[s..e].eq_ignore_ascii_case(old)) else {
        return src.to_string();
    };
    let end = in_clause_end(src, e, ce);
    let entry = match rel_in {
        Some(rel) => format!("{new} in '{rel}'"),
        None => new.to_string(),
    };
    format!("{}{entry}{}", &src[..s], &src[end..])
}

fn remove_in_clause(src: &str, clause_start: usize, clause_end: usize, unit: &str) -> String {
    let mask = masked(src);
    let names = idents(src, clause_start, clause_end, &mask);
    let Some(i) = names.iter().position(|&(s, e)| src[s..e].eq_ignore_ascii_case(unit)) else {
        return src.to_string();
    };
    let (s, e) = names[i];
    let end = in_clause_end(src, e, clause_end);
    let before = prev_significant(src, s, &mask, clause_start);
    let after = next_significant(src, end, &mask, clause_end);
    let (from, to) = match (before, after) {
        (Some(c), _) if src.as_bytes()[c] == b',' => (c, end),
        (_, Some(c)) if src.as_bytes()[c] == b',' => (s, c + 1),
        _ => (s, end),
    };
    // Also takes the spaces after a removed first name, so `Unit1, Forms` becomes `Forms`.
    let to = if from == s {
        to + src[to..].chars().take_while(|c| c.is_whitespace() && *c != '\n').map(char::len_utf8).sum::<usize>()
    } else {
        to
    };
    let mut out = src.to_string();
    out.replace_range(from..to, "");
    out
}

/// Changes a unit's `unit Old;` line to `unit New;`, keeping its layout. Returns the text unchanged
/// when the line names another unit.
pub fn rename_unit_line(src: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut done = false;
    for line in src.split_inclusive('\n') {
        let body = line.trim_start();
        let rest = body.get(4..).filter(|_| body.len() > 4 && body[..4].eq_ignore_ascii_case("unit"));
        let matches = !done
            && rest.is_some_and(|r| {
                r.starts_with(|c: char| c.is_whitespace())
                    && r.trim().strip_suffix(';').is_some_and(|n| n.trim().eq_ignore_ascii_case(old))
            });
        if matches {
            let indent = &line[..line.len() - body.len()];
            let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
            out.push_str(&format!("{indent}unit {new};{ending}"));
            done = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The byte range of the main `uses` clause's names (from after `uses` to before its `;`).
fn uses_clause(src: &str) -> Option<(usize, usize)> {
    let mask = masked(src);
    let start = find_word_masked(&src.to_ascii_lowercase(), "uses", &mask)? + "uses".len();
    let end = (start..src.len()).find(|&i| src.as_bytes()[i] == b';' && !mask[i])?;
    Some((start, end))
}

/// Positions of the identifiers in `src[from..to]` that are not comments or strings.
fn idents(src: &str, from: usize, to: usize, mask: &[bool]) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        if !mask[i] && (b[i].is_ascii_alphabetic() || b[i] == b'_') {
            let s = i;
            while i < to && !mask[i] && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push((s, i));
        } else {
            i += 1;
        }
    }
    out
}

/// Marks comments (`//`, `{}`, `(* *)`) and strings, which are not code.
fn masked(src: &str) -> Vec<bool> {
    let b = src.as_bytes();
    let mut mask = vec![false; b.len()];
    let mut i = 0;
    while i < b.len() {
        let end = if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            Some(b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p))
        } else if b[i] == b'{' {
            Some(b[i..].iter().position(|&c| c == b'}').map_or(b.len(), |p| i + p + 1))
        } else if b[i] == b'(' && b.get(i + 1) == Some(&b'*') {
            Some(find_bytes(b, i + 2, b"*)").map_or(b.len(), |p| p + 2))
        } else if b[i] == b'\'' {
            Some(b[i + 1..].iter().position(|&c| c == b'\'').map_or(b.len(), |p| i + 1 + p + 1))
        } else {
            None
        };
        match end {
            Some(e) => {
                for m in &mut mask[i..e] {
                    *m = true;
                }
                i = e;
            }
            None => i += 1,
        }
    }
    mask
}

fn find_bytes(b: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    (from..b.len().saturating_sub(pat.len() - 1)).find(|&i| &b[i..i + pat.len()] == pat)
}

/// The first whole-word match of `word` (already lower case) in `hay` (lower case), outside
/// comments and strings of the masked source.
fn find_word_masked(hay: &str, word: &str, mask: &[bool]) -> Option<usize> {
    let b = hay.as_bytes();
    let mut from = 0;
    while let Some(p) = hay[from..].find(word).map(|p| p + from) {
        let before_ok = p == 0 || !(b[p - 1].is_ascii_alphanumeric() || b[p - 1] == b'_');
        let after = p + word.len();
        let after_ok = after >= b.len() || !(b[after].is_ascii_alphanumeric() || b[after] == b'_');
        if before_ok && after_ok && !mask[p] {
            return Some(p);
        }
        from = p + 1;
    }
    None
}

fn find_word(hay: &str, word: &str) -> Option<usize> {
    find_word_masked(hay, word, &vec![false; hay.len()])
}

/// The end of ` in '...'` after a name, or the name's own end.
fn in_clause_end(src: &str, name_end: usize, limit: usize) -> usize {
    let rest = &src[name_end..limit];
    let trimmed = rest.trim_start();
    let Some(after_in) = trimmed.strip_prefix("in") else {
        return name_end;
    };
    if !after_in.starts_with(|c: char| c.is_whitespace() || c == '\'') {
        return name_end;
    }
    let offset = rest.len() - trimmed.len() + 2; // past "in"
    let quote_start = match rest[offset..].find('\'') {
        Some(q) => name_end + offset + q,
        None => return name_end,
    };
    match src[quote_start + 1..limit].find('\'') {
        Some(q) => quote_start + 1 + q + 1,
        None => name_end,
    }
}

fn prev_significant(src: &str, at: usize, mask: &[bool], limit: usize) -> Option<usize> {
    (limit..at)
        .rev()
        .find(|&i| !mask[i] && !src.as_bytes()[i].is_ascii_whitespace())
}

fn next_significant(src: &str, at: usize, mask: &[bool], limit: usize) -> Option<usize> {
    (at..limit).find(|&i| !mask[i] && !src.as_bytes()[i].is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = "program GuiApp1;\n\n{$mode objfpc}{$H+}\n\nuses\n  Interfaces, // this includes the LCL widgetset\n  Forms, Unit1;\n\nbegin\n  RequireDerivedFormResource := True;\n  Application.Scaled := True;\n  Application.Initialize;\n  Application.CreateForm(TForm1, Form1);\n  Application.Run;\nend.\n";

    #[test]
    fn a_new_data_module_is_created_after_the_forms_just_before_run() {
        let out = add_form(TEMPLATE, "Unit2", "TDataModule2", "DataModule2").unwrap();
        let form = out.find("Application.CreateForm(TForm1, Form1);").unwrap();
        let data = out.find("Application.CreateForm(TDataModule2, DataModule2);").expect("the data module is created");
        let run = out.find("Application.Run;").unwrap();
        assert!(form < data && data < run, "the data module is created last, before Run:\n{out}");
        assert!(out.contains("Forms, Unit1, Unit2;"), "{out}");
    }

    #[test]
    fn a_new_form_joins_the_uses_clause_and_the_create_calls() {
        let out = add_form(TEMPLATE, "Unit2", "TForm2", "Form2").unwrap();
        assert_eq!(
            out,
            TEMPLATE
                .replace("Forms, Unit1;", "Forms, Unit1, Unit2;")
                .replace(
                    "  Application.Run;",
                    "  Application.CreateForm(TForm2, Form2);\n  Application.Run;"
                )
        );
        // Adding it again changes nothing.
        assert_eq!(add_form(&out, "Unit2", "TForm2", "Form2").unwrap(), out);
    }

    #[test]
    fn removing_the_added_form_restores_the_template_exactly() {
        let added = add_form(TEMPLATE, "Unit2", "TForm2", "Form2").unwrap();
        assert_eq!(remove_unit(&added, "Unit2", Some(("TForm2", "Form2"))), TEMPLATE);
    }

    #[test]
    fn removing_the_first_unit_and_a_unit_with_a_file_name_keep_the_rest() {
        assert_eq!(
            remove_unit(TEMPLATE, "Unit1", Some(("TForm1", "Form1"))),
            TEMPLATE
                .replace("Forms, Unit1;", "Forms;")
                .replace("  Application.CreateForm(TForm1, Form1);\n", "")
        );
        let with_path = "program p;\nuses\n  Forms, Unit3 in 'sub/unit3.pas' {Form3}, Unit4;\nbegin\nend.\n";
        assert_eq!(
            remove_unit(with_path, "Unit3", None),
            "program p;\nuses\n  Forms {Form3}, Unit4;\nbegin\nend.\n",
            "a comment after the removed name stays"
        );
        // A name inside a string or a comment is not the unit.
        let tricky = "program p;\nuses\n  Forms, Unit1; // Unit2\nbegin\nend.\n";
        assert_eq!(remove_unit(tricky, "Unit2", None), tricky);
    }

    #[test]
    fn a_program_without_run_refuses_a_form_and_crlf_is_kept() {
        assert!(add_form("program p;\nuses\n  Forms;\nbegin\nend.\n", "Unit2", "TForm2", "Form2").is_err());
        let crlf = TEMPLATE.replace('\n', "\r\n");
        let out = add_form(&crlf, "Unit2", "TForm2", "Form2").unwrap();
        assert!(out.contains("  Application.CreateForm(TForm2, Form2);\r\n  Application.Run;"));
        assert!(!out.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn a_unit_moved_into_a_folder_gets_its_path_and_back_to_the_folder_drops_it() {
        let moved = set_unit(TEMPLATE, "Unit1", "Unit1", Some("sub/unit1.pas"));
        assert!(moved.contains("  Forms, Unit1 in 'sub/unit1.pas';"), "{moved}");
        assert_eq!(set_unit(&moved, "Unit1", "Unit1", None), TEMPLATE);
        // A unit the program does not list is left alone.
        assert_eq!(set_unit(TEMPLATE, "Unit9", "Unit9", Some("x.pas")), TEMPLATE);
    }

    #[test]
    fn a_renamed_unit_gets_its_new_name_in_the_uses_clause_and_its_unit_line() {
        let renamed = set_unit(TEMPLATE, "Unit1", "Main", None);
        assert!(renamed.contains("Forms, Main;"));
        let unit = "unit Unit1;\n\ninterface\n";
        assert_eq!(rename_unit_line(unit, "Unit1", "Main"), "unit Main;\n\ninterface\n");
        assert_eq!(rename_unit_line("unit Other;\n", "Unit1", "Main"), "unit Other;\n");
        assert_eq!(rename_unit_line("unit unit1;\r\n", "Unit1", "Main"), "unit Main;\r\n");
    }

    #[test]
    fn every_uses_clause_is_updated_and_relative_paths_leave_their_folder() {
        let unit = "unit Main;\n\ninterface\n\nuses\n  Classes, Unit2;\n\nimplementation\n\nuses\n  Dialogs, Unit2 in 'lib/unit2.pas';\n\nend.\n";
        assert_eq!(
            set_unit(unit, "Unit2", "Dialog", None),
            "unit Main;\n\ninterface\n\nuses\n  Classes, Dialog;\n\nimplementation\n\nuses\n  Dialogs, Dialog;\n\nend.\n"
        );
        assert_eq!(
            remove_unit(unit, "Unit2", None),
            "unit Main;\n\ninterface\n\nuses\n  Classes;\n\nimplementation\n\nuses\n  Dialogs;\n\nend.\n"
        );
        assert_eq!(relative_path(Path::new("/p/a"), Path::new("/p/a/sub/x.pas")), "sub/x.pas");
        assert_eq!(relative_path(Path::new("/p/a/b"), Path::new("/p/a/x.pas")), "../x.pas");
        assert_eq!(relative_path(Path::new("/p/a"), Path::new("/p/c/x.pas")), "../c/x.pas");
    }

    #[test]
    fn form_names_come_from_the_first_object_line() {
        assert_eq!(
            form_of_lfm("object Form2: TForm2\n  Left = 300\nend\n"),
            Some(("TForm2".to_string(), "Form2".to_string()))
        );
        assert_eq!(form_of_lfm("not a form"), None);
    }
}
