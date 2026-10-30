//! Where each line of the code editor is, in rows. A line takes one row, or several when it wraps.
//! The gutter, the line bands and the clicks in the gutter all read this map, so they stay in step
//! with the text however many rows each line takes.

use eframe::egui::Galley;

pub struct LineMap {
    /// The top of each line, then the bottom of the text: `lines + 1` entries.
    tops: Vec<f32>,
}

impl LineMap {
    /// The map of a laid-out editor text with `lines` logical lines (`text.split('\n').count()`).
    pub fn new(galley: &Galley, lines: usize) -> Self {
        let mut tops = Vec::with_capacity(lines + 1);
        let mut at_line_start = true;
        for row in &galley.rows {
            if at_line_start {
                tops.push(row.pos.y);
            }
            at_line_start = row.ends_with_newline;
        }
        // An empty text still has one (empty) row; keep one top per line in any case.
        let end = galley.size().y;
        let last = tops.last().copied().unwrap_or(0.0);
        tops.resize(lines, last);
        tops.push(end.max(last));
        Self { tops }
    }

    /// The number of logical lines.
    pub fn lines(&self) -> usize {
        self.tops.len() - 1
    }

    /// The height of the whole text.
    pub fn height(&self) -> f32 {
        self.tops[self.lines()]
    }

    /// The top of a line (1-based).
    pub fn top(&self, line: u32) -> f32 {
        self.tops[(line.max(1) as usize - 1).min(self.lines() - 1)]
    }

    /// The bottom of a line (1-based): the top of the next one.
    pub fn bottom(&self, line: u32) -> f32 {
        self.tops[(line.max(1) as usize).min(self.lines())]
    }

    /// The line at a height in the text (1-based). Above the text is line 1; below it, the last.
    pub fn line_at(&self, y: f32) -> u32 {
        let after = self.tops.partition_point(|&t| t <= y);
        (after.max(1).min(self.lines())) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;
    use eframe::egui::{self, text::LayoutJob};

    /// The map of `text` laid out at `width` (infinite for no wrapping), with a real font.
    fn map_of(text: &str, width: f32) -> LineMap {
        let ctx = egui::Context::default();
        let mut result = None;
        let mut out = ctx.run_pass(Default::default(), |ctx| {
            let pal = Palette::of(ctx);
            let mut job: LayoutJob = crate::highlight::layout(text, &pal, crate::editor_font(), crate::prefs::line_height());
            job.wrap.max_width = width;
            let galley = ctx.fonts_mut(|f| f.layout_job(job));
            result = Some(LineMap::new(&galley, text.split('\n').count()));
        });
        out.textures_delta.clear();
        result.unwrap()
    }

    #[test]
    fn without_wrapping_each_line_is_one_line_height() {
        let lh = crate::prefs::line_height();
        let map = map_of("begin\n  x := 1;\n\nend.\n", f32::INFINITY);
        assert_eq!(map.lines(), 5, "a trailing newline makes an empty last line");
        for line in 1..=5u32 {
            assert_eq!(map.top(line), (line - 1) as f32 * lh, "line {line}");
        }
        assert_eq!(map.height(), 5.0 * lh);
        assert_eq!(map.line_at(0.0), 1);
        assert_eq!(map.line_at(lh * 2.5), 3);
        assert_eq!(map.line_at(10_000.0), 5, "below the text is the last line");
        assert_eq!(map.line_at(-5.0), 1, "above the text is the first line");
    }

    #[test]
    fn a_wrapped_line_takes_several_rows_and_the_lines_after_it_move_down() {
        let lh = crate::prefs::line_height();
        let long = "word ".repeat(40);
        let text = format!("first\n{long}\nlast");
        let map = map_of(&text, 200.0);
        assert_eq!(map.lines(), 3);
        assert_eq!(map.top(1), 0.0);
        assert_eq!(map.top(2), lh);
        // The second line wraps onto several rows: the third line starts after all of them.
        let rows = (map.top(3) - map.top(2)) / lh;
        assert!(rows > 3.0, "the long line takes {rows} rows");
        assert_eq!(map.bottom(2), map.top(3), "a line's bottom is the next line's top");
        assert_eq!(map.bottom(3), map.height());
        // A point inside the wrapped line is that line, whichever of its rows it is on.
        assert_eq!(map.line_at(map.top(2) + lh * 2.0), 2);
        assert_eq!(map.line_at(map.top(3) + 1.0), 3);
    }

    #[test]
    fn an_empty_text_is_one_line() {
        let map = map_of("", f32::INFINITY);
        assert_eq!(map.lines(), 1);
        assert_eq!(map.line_at(0.0), 1);
        assert_eq!(map.bottom(1), map.height());
    }
}
