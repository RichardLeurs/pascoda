//! The tabbed pane under the editor: Problems, Output, Debug and Terminal. This file holds the
//! state and the rules for switching tabs by themselves; `shell.rs` draws the tabs and
//! `terminal_view.rs` draws the terminal.

use crate::terminal::Terminal;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum BottomTab {
    #[default]
    Problems,
    Output,
    Debug,
    Terminal,
}

impl BottomTab {
    pub const ALL: [BottomTab; 4] = [
        BottomTab::Problems,
        BottomTab::Output,
        BottomTab::Debug,
        BottomTab::Terminal,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// How long a click on a tab protects the choice from automatic switching.
pub(crate) const USER_GRACE_SECS: f64 = 2.5;

/// Selected tab, the terminal and what the automatic switching needs to know.
#[derive(Default)]
pub(crate) struct BottomPane {
    pub tab: BottomTab,
    /// When (egui time, seconds) the user last clicked a tab.
    last_click: Option<f64>,
    /// The terminal view had keyboard focus in the latest frame.
    pub terminal_focused: bool,
    pub terminal: Option<Terminal>,
    /// The folder the shell was started in (to notice when the project folder moves on).
    pub terminal_root: Option<std::path::PathBuf>,
    /// The shell could not be started.
    pub terminal_error: Option<String>,
    pub view: crate::terminal_view::ViewState,
    /// The terminal was drawn in the previous frame (so a newly shown one asks for focus).
    pub view_shown: bool,
}

impl BottomPane {
    /// The user picked a tab.
    pub fn choose(&mut self, tab: BottomTab, now: f64) {
        self.tab = tab;
        self.last_click = Some(now);
    }

    /// The program wants to show `tab` (a build started, a breakpoint was hit...). Returns
    /// whether it switched. It does not when the user chose a tab a moment ago, nor away from the
    /// terminal while the user types in it.
    pub fn auto(&mut self, tab: BottomTab, now: f64) -> bool {
        if self.tab == tab {
            return false;
        }
        if self.last_click.is_some_and(|t| now - t < USER_GRACE_SECS) {
            return false;
        }
        if self.tab == BottomTab::Terminal && self.terminal_focused {
            return false;
        }
        self.tab = tab;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_by_itself_when_nobody_touched_the_tabs() {
        let mut b = BottomPane::default();
        assert!(b.auto(BottomTab::Output, 10.0));
        assert_eq!(b.tab, BottomTab::Output);
        assert!(b.auto(BottomTab::Debug, 10.1));
        assert_eq!(b.tab, BottomTab::Debug);
        // Already there: nothing to do.
        assert!(!b.auto(BottomTab::Debug, 10.2));
    }

    #[test]
    fn a_recent_click_wins_over_automatic_switching() {
        let mut b = BottomPane::default();
        b.choose(BottomTab::Output, 100.0);
        assert!(!b.auto(BottomTab::Problems, 100.5));
        assert!(!b.auto(BottomTab::Debug, 102.4));
        assert_eq!(b.tab, BottomTab::Output);
        // After the grace period it may switch again.
        assert!(b.auto(BottomTab::Problems, 102.6));
        assert_eq!(b.tab, BottomTab::Problems);
    }

    #[test]
    fn typing_in_the_terminal_is_never_interrupted() {
        let mut b = BottomPane::default();
        b.choose(BottomTab::Terminal, 0.0);
        b.terminal_focused = true;
        assert!(!b.auto(BottomTab::Debug, 1000.0));
        assert_eq!(b.tab, BottomTab::Terminal);
        // Without focus (the user clicked into the editor) the terminal tab can be left.
        b.terminal_focused = false;
        assert!(b.auto(BottomTab::Debug, 1000.0));
        assert_eq!(b.tab, BottomTab::Debug);
    }

    #[test]
    fn tab_order_is_stable() {
        let names: Vec<_> = BottomTab::ALL.iter().map(|t| t.index()).collect();
        assert_eq!(names, [0, 1, 2, 3]);
    }
}
