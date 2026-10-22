//! The interactive hunk prompt shared by `add -p`, `restore -p` and `stash push -p`: git's answers and their texts.

use std::fmt::Write as _;

use pyn_diff::{Hunk, Misfit, Op, apply_hunks};
use regex::bytes::Regex;

/// Hunks listed per page by `g`.
const LIST_LINES: usize = 20;
const SUMMARY_HEADER: usize = 20;
const SUMMARY_LINE: usize = 80;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Use {
    Undecided,
    Yes,
    No,
}

pub(super) enum Item {
    /// A change of file mode, kept as the text of its two lines.
    Mode {
        old: String,
        new: String,
    },
    Hunk(Hunk),
}

impl Item {
    pub(super) fn hunk(&self) -> Option<&Hunk> {
        match self {
            Item::Hunk(h) => Some(h),
            Item::Mode { .. } => None,
        }
    }
}

/// The words a mode of the prompt uses: `stage` / ` ` for `add`, `discard` / ` from worktree` for `restore`.
pub(super) struct Wording {
    pub verb: &'static str,
    pub tail: &'static str,
    /// What an applied edit is for ("staging").
    pub gerund: &'static str,
}

impl Wording {
    fn capital(&self) -> String {
        let mut chars = self.verb.chars();
        chars
            .next()
            .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
            .unwrap_or_default()
    }

    fn prompt(&self, what: &str) -> String {
        format!("{} {what}{}", self.capital(), self.tail)
    }

    fn help(&self) -> String {
        let (v, t) = (self.verb, self.tail);
        format!(
            "y - {v} this hunk{t}\n\
             n - do not {v} this hunk{t}\n\
             q - quit; do not {v} this hunk or any of the remaining ones\n\
             a - {v} this hunk and all later hunks in the file\n\
             d - do not {v} this hunk or any of the later hunks in the file\n"
        )
    }
}

/// One file's changes with the choice made for each.
pub(super) struct FileDiff {
    pub path: String,
    pub header: String,
    pub items: Vec<Item>,
    pub uses: Vec<Use>,
    pub deleted: bool,
    pub added: bool,
    /// What the hunks' old and new lines refer to, and whether choosing applies them backwards.
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    pub reverse: bool,
}

impl FileDiff {
    fn text(&self, at: usize) -> String {
        match &self.items[at] {
            Item::Mode { old, new } => format!("old mode {old}\nnew mode {new}\n"),
            Item::Hunk(h) => h.render(),
        }
    }

    fn what(&self, at: usize) -> &'static str {
        match &self.items[at] {
            Item::Mode { .. } => "mode change",
            _ if self.deleted => "deletion",
            _ if self.added => "addition",
            _ => "this hunk",
        }
    }

    fn can_edit(&self, at: usize) -> bool {
        self.items[at].hunk().is_some() && !self.deleted
    }

    fn fits(&self, hunk: &Hunk) -> Result<(), Misfit> {
        if self.reverse {
            apply_hunks(&self.new, &[&hunk.reversed()]).map(drop)
        } else {
            apply_hunks(&self.old, &[hunk]).map(drop)
        }
    }

    fn summary(&self, at: usize) -> String {
        let Some(h) = self.items[at].hunk() else {
            return String::new();
        };
        let mut out = format!(
            " -{},{} +{},{} ",
            h.old_start, h.old_count, h.new_start, h.new_count
        );
        while out.len() < SUMMARY_HEADER {
            out.push(' ');
        }
        if let Some((op, line)) = h.lines.iter().find(|(op, _)| *op != Op::Equal) {
            out.push(if *op == Op::Delete { '-' } else { '+' });
            out.push_str(String::from_utf8_lossy(line).trim_end_matches('\n'));
        }
        let mut end = out.len().min(SUMMARY_LINE);
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out
    }
}

/// How the prompt talks to the person: lines in, text out, and an editor for a hunk.
pub(super) trait Io {
    fn read_line(&mut self) -> Option<String>;
    fn out(&mut self, text: &str);
    fn err(&mut self, text: &str);
    /// The text left in the editor after it was started on `text`.
    fn edit(&mut self, text: &str) -> Result<Vec<u8>, String>;
}

#[derive(PartialEq, Eq)]
enum Flow {
    Next,
    Quit,
}

/// Asks about every hunk of every file, recording the choices in `uses`; stops at `q`. `after` runs when a file's
/// questions are over, before the blank line that separates files.
pub(super) fn pick(
    files: &mut [FileDiff],
    wording: &Wording,
    io: &mut dyn Io,
    after: &mut dyn FnMut(&FileDiff, &mut dyn Io),
) {
    for file in files {
        if update_file(file, wording, io, after) == Flow::Quit {
            break;
        }
    }
}

fn update_file(
    file: &mut FileDiff,
    w: &Wording,
    io: &mut dyn Io,
    after: &mut dyn FnMut(&FileDiff, &mut dyn Io),
) -> Flow {
    io.out(&file.header);
    let mut at = 0;
    let mut shown = None;
    let mut flow = Flow::Next;
    loop {
        let n = file.items.len();
        if at >= n {
            at = 0;
        }
        let undecided = |i: &usize| file.uses[*i] == Use::Undecided;
        let next = (at + 1..n).chain(0..at).find(undecided);
        let prev = (0..at).rev().chain((at + 1..n).rev()).find(undecided);
        if next.is_none() && file.uses[at] != Use::Undecided {
            break;
        }
        if shown != Some(at) {
            io.out(&file.text(at));
            shown = Some(at);
        }
        let splittable = file.items[at]
            .hunk()
            .is_some_and(|h| h.splittable_into() > 1);
        let mut keys = String::from("y,n,q,a,d");
        for (allowed, key) in [
            (next.is_some(), ",k"),
            (n > 1, ",K"),
            (next.is_some(), ",j"),
            (n > 1, ",J"),
            (n > 1, ",g,/"),
            (splittable, ",s"),
            (file.can_edit(at), ",e"),
        ] {
            if allowed {
                keys.push_str(key);
            }
        }
        io.out(&format!(
            "({}/{n}) {}{} [{keys},p,P,?]? ",
            at + 1,
            w.prompt(file.what(at)),
            match file.uses[at] {
                Use::Yes => " (was: y)",
                Use::No => " (was: n)",
                Use::Undecided => "",
            }
        ));
        let Some(answer) = io.read_line() else {
            flow = Flow::Quit;
            break;
        };
        let mut letters = answer.chars();
        let Some(first) = letters.next() else {
            continue;
        };
        let rest = letters.as_str().to_string();
        if letters.next().is_some() && !matches!(first, 'g' | '/') {
            say(io, &format!("Only one letter is expected, got '{answer}'"));
            continue;
        }
        match first {
            'y' | 'Y' | 'n' | 'N' => {
                file.uses[at] = if first.eq_ignore_ascii_case(&'y') {
                    Use::Yes
                } else {
                    Use::No
                };
                at = next.unwrap_or(at);
            }
            'a' | 'A' | 'd' | 'D' => {
                let choice = if first.eq_ignore_ascii_case(&'a') {
                    Use::Yes
                } else {
                    Use::No
                };
                for u in &mut file.uses[at..] {
                    if *u == Use::Undecided {
                        *u = choice;
                    }
                }
                at = (0..n)
                    .find(|i| file.uses[*i] == Use::Undecided)
                    .unwrap_or(at);
            }
            'q' | 'Q' => {
                for u in &mut file.uses[at..] {
                    if *u == Use::Undecided {
                        *u = Use::No;
                    }
                }
                flow = Flow::Quit;
                break;
            }
            'j' => match next {
                Some(i) => at = i,
                None => say(io, "No other undecided hunk"),
            },
            'J' if n > 1 => at = (at + 1) % n,
            'J' => say(io, "No other hunk"),
            'k' => match prev {
                Some(i) => at = i,
                None => say(io, "No other undecided hunk"),
            },
            'K' if n > 1 => at = (at + n - 1) % n,
            'K' => say(io, "No other hunk"),
            'g' if n > 1 => match goto(file, at, rest.trim(), io) {
                Goto::To(i) => at = i,
                Goto::Stay => {}
                Goto::End => {
                    flow = Flow::Quit;
                    break;
                }
            },
            'g' => say(io, "No other hunks to goto"),
            '/' if n > 1 => match search(file, at, &rest, io) {
                Goto::To(i) => at = i,
                Goto::Stay => {}
                Goto::End => {
                    flow = Flow::Quit;
                    break;
                }
            },
            '/' => say(io, "No other hunks to search"),
            's' => {
                if split(file, at, io) {
                    shown = None;
                }
            }
            'e' if file.can_edit(at) => {
                if edit(file, at, w, io) {
                    file.uses[at] = Use::Yes;
                    at = next.unwrap_or(at);
                    shown = None;
                }
            }
            'e' => say(io, "Sorry, cannot edit this hunk"),
            'p' | 'P' => io.out(&file.text(at)),
            '?' => {
                io.out(&w.help());
                for (allowed, line) in [
                    (
                        next.is_some(),
                        "j - go to the next undecided hunk, roll over at the bottom\n",
                    ),
                    (n > 1, "J - go to the next hunk, roll over at the bottom\n"),
                    (
                        next.is_some(),
                        "k - go to the previous undecided hunk, roll over at the top\n",
                    ),
                    (n > 1, "K - go to the previous hunk, roll over at the top\n"),
                    (
                        n > 1,
                        "g - select a hunk to go to\n/ - search for a hunk matching the given regex\n",
                    ),
                    (
                        splittable,
                        "s - split the current hunk into smaller hunks\n",
                    ),
                    (file.can_edit(at), "e - manually edit the current hunk\n"),
                    (
                        true,
                        "p - print the current hunk\nP - print the current hunk using the pager\n? - print help\n",
                    ),
                ] {
                    if allowed {
                        io.out(line);
                    }
                }
            }
            _ => say(io, &format!("Unknown command '{first}' (use '?' for help)")),
        }
    }
    after(file, io);
    io.out("\n");
    flow
}

fn say(io: &mut dyn Io, text: &str) {
    io.out(&format!("{text}\n"));
}

enum Goto {
    To(usize),
    Stay,
    /// The input ended while asking.
    End,
}

fn goto(file: &FileDiff, at: usize, given: &str, io: &mut dyn Io) -> Goto {
    let n = file.items.len();
    let first_listed = usize::from(matches!(file.items[0], Item::Mode { .. }));
    let mut answer = given.to_string();
    let mut from = at.saturating_sub(LIST_LINES / 2).max(first_listed);
    while answer.is_empty() {
        let end = (from + LIST_LINES).min(n);
        for i in from..end {
            let mark = match file.uses[i] {
                Use::Yes => '+',
                Use::No => '-',
                Use::Undecided => ' ',
            };
            io.out(&format!("{mark}{:>2}: {}\n", i + 1, file.summary(i)));
        }
        from = end;
        io.out(if from < n {
            "go to which hunk (<ret> to see more)? "
        } else {
            "go to which hunk? "
        });
        match io.read_line() {
            Some(line) => answer = line.trim().to_string(),
            None => break,
        }
    }
    match answer.parse::<usize>() {
        Ok(i) if (1..=n).contains(&i) => Goto::To(i - 1),
        Ok(_) => {
            let s = if n == 1 { "" } else { "s" };
            say(io, &format!("Sorry, only {n} hunk{s} available."));
            Goto::Stay
        }
        Err(_) => {
            say(io, &format!("Invalid number: '{answer}'"));
            Goto::Stay
        }
    }
}

fn search(file: &FileDiff, at: usize, given: &str, io: &mut dyn Io) -> Goto {
    let mut pattern = given.to_string();
    if pattern.is_empty() {
        io.out("search for regex? ");
        match io.read_line() {
            Some(line) if line.trim().is_empty() => return Goto::Stay,
            Some(line) => pattern = line,
            None => return Goto::End,
        }
    }
    let regex = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => {
            say(io, &format!("Malformed search regexp {pattern}: {e}"));
            return Goto::Stay;
        }
    };
    let n = file.items.len();
    match (0..n)
        .map(|d| (at + d) % n)
        .find(|i| regex.is_match(file.text(*i).as_bytes()))
    {
        Some(i) => Goto::To(i),
        None => {
            say(io, "No hunk matches the given pattern");
            Goto::Stay
        }
    }
}

fn split(file: &mut FileDiff, at: usize, io: &mut dyn Io) -> bool {
    let parts = match &file.items[at] {
        Item::Hunk(h) if h.splittable_into() > 1 => h.split(),
        _ => {
            say(io, "Sorry, cannot split this hunk");
            return false;
        }
    };
    let count = parts.len();
    let used = file.uses[at];
    file.items
        .splice(at..=at, parts.into_iter().map(Item::Hunk));
    file.uses.splice(at..=at, vec![used; count]);
    io.out(&format!("Split into {count} hunks.\n"));
    true
}

const EDIT_GUIDE: &str = "# ---\n\
# To remove '-' lines, make them ' ' lines (context).\n\
# To remove '+' lines, delete them.\n\
# Lines starting with # will be removed.\n";

/// Lets the person edit the hunk; true when the edited hunk replaced it.
fn edit(file: &mut FileDiff, at: usize, w: &Wording, io: &mut dyn Io) -> bool {
    let Some(original) = file.items[at].hunk().cloned() else {
        return false;
    };
    let mut text = String::new();
    let _ = write!(
        text,
        "# Manual hunk edit mode -- see bottom for a quick guide.\n{}{EDIT_GUIDE}\
         # If the patch applies cleanly, the edited hunk will immediately be marked for {}.\n\
         # If it does not apply cleanly, you will be given an opportunity to\n\
         # edit again.  If all lines of the hunk are removed, then the edit is\n\
         # aborted and the hunk is left unchanged.\n",
        original.render(),
        w.gerund
    );
    loop {
        match io.edit(&text) {
            Ok(bytes) => {
                match original.from_edit(&bytes) {
                    Ok(None) => return false,
                    Ok(Some(hunk)) => match file.fits(&hunk) {
                        Ok(()) => {
                            file.items[at] = Item::Hunk(hunk);
                            return true;
                        }
                        Err(Misfit(line)) => {
                            io.err(&format!("error: patch failed: {}:{line}\n", file.path));
                            io.err(&format!("error: {}: patch does not apply\n", file.path));
                            io.err("error: 'git apply --cached' failed\n");
                        }
                    },
                    Err(e) => io.err(&format!("error: {e}\n")),
                }
                text = String::from_utf8_lossy(&bytes).into_owned();
            }
            Err(e) => io.err(&e),
        }
        loop {
            io.out("Your edited hunk does not apply. Edit again (saying \"no\" discards!) [y/n]? ");
            match io.read_line().and_then(|l| l.chars().next()) {
                Some('y' | 'Y') => break,
                Some('n' | 'N') | None => return false,
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Script {
        input: std::vec::IntoIter<String>,
        out: String,
        err: String,
        edits: std::vec::IntoIter<Vec<u8>>,
    }

    impl Script {
        fn new(input: &[&str]) -> Self {
            Script {
                input: input
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .into_iter(),
                out: String::new(),
                err: String::new(),
                edits: Vec::new().into_iter(),
            }
        }
    }

    impl Io for Script {
        fn read_line(&mut self) -> Option<String> {
            self.input.next()
        }
        fn out(&mut self, text: &str) {
            self.out.push_str(text);
        }
        fn err(&mut self, text: &str) {
            self.err.push_str(text);
        }
        fn edit(&mut self, _: &str) -> Result<Vec<u8>, String> {
            self.edits.next().ok_or_else(|| "no editor".to_string())
        }
    }

    const WORDS: Wording = Wording {
        verb: "stage",
        tail: "",
        gerund: "staging",
    };

    fn file(edits: &[(usize, &str)]) -> FileDiff {
        let old: Vec<String> = (1..=40).map(|i| format!("{i}\n")).collect();
        let mut new = old.clone();
        for (i, text) in edits {
            new[*i - 1] = format!("{text}\n");
        }
        let (old, new) = (old.concat().into_bytes(), new.concat().into_bytes());
        let hunks = pyn_diff::hunks(&old, &new, 3, false);
        FileDiff {
            path: "f".into(),
            header: "diff --git a/f b/f\n".into(),
            uses: vec![Use::Undecided; hunks.len()],
            items: hunks.into_iter().map(Item::Hunk).collect(),
            deleted: false,
            added: false,
            old,
            new,
            reverse: false,
        }
    }

    fn run(f: &mut FileDiff, input: &[&str]) -> Script {
        let mut io = Script::new(input);
        update_file(f, &WORDS, &mut io, &mut |_, _| {});
        io
    }

    fn three() -> FileDiff {
        file(&[(3, "three"), (20, "twenty"), (38, "t38")])
    }

    #[test]
    fn keys_offered_follow_the_position_and_the_decisions() {
        let io = run(&mut three(), &["n", "n"]);
        assert!(
            io.out
                .contains("(1/3) Stage this hunk [y,n,q,a,d,k,K,j,J,g,/,e,p,P,?]? ")
        );
        assert!(
            io.out
                .contains("(3/3) Stage this hunk [y,n,q,a,d,K,J,g,/,e,p,P,?]? ")
        );
        let io = run(&mut file(&[(3, "three")]), &["q"]);
        assert!(io.out.contains("[y,n,q,a,d,e,p,P,?]? "));
        assert!(io.out.ends_with("]? \n"));
    }

    #[test]
    fn y_n_a_d_decide_and_roll_over_to_what_is_left() {
        let mut f = three();
        run(&mut f, &["j", "y", "y", "n"]);
        assert_eq!(f.uses, [Use::No, Use::Yes, Use::Yes]);
        let mut f = three();
        run(&mut f, &["J", "J", "y", "d"]);
        assert_eq!(f.uses, [Use::No, Use::No, Use::Yes]);
        let mut f = three();
        run(&mut f, &["n", "A"]);
        assert_eq!(f.uses, [Use::No, Use::Yes, Use::Yes]);
    }

    #[test]
    fn q_leaves_earlier_choices_and_skips_the_rest() {
        let mut f = file(&[(3, "three"), (20, "twenty")]);
        let io = run(&mut f, &["y", "q"]);
        assert_eq!(f.uses, [Use::Yes, Use::No]);
        assert!(io.out.ends_with("]? \n"));
    }

    #[test]
    fn one_letter_answers_only_and_the_hunk_is_shown_when_it_changes() {
        let mut f = three();
        let io = run(&mut f, &["", "yes", " Y", "x", "?", "Y"]);
        assert_eq!(f.uses, [Use::Yes, Use::Undecided, Use::Undecided]);
        assert!(io.out.contains("Only one letter is expected, got 'yes'\n"));
        assert!(io.out.contains("Only one letter is expected, got ' Y'\n"));
        assert!(io.out.contains("Unknown command 'x' (use '?' for help)\n"));
        assert!(
            io.out
                .contains("y - stage this hunk\nn - do not stage this hunk\n")
        );
        assert!(
            io.out
                .contains("j - go to the next undecided hunk, roll over at the bottom\nJ - ")
        );
        assert_eq!(io.out.matches("@@ -1,6 +1,6 @@").count(), 1);
    }

    #[test]
    fn p_prints_the_hunk_again() {
        let io = run(&mut three(), &["p", "P"]);
        assert_eq!(io.out.matches("@@ -1,6 +1,6 @@").count(), 3);
    }

    #[test]
    fn j_and_k_roll_over_and_refuse_when_nothing_else_is_undecided() {
        let mut f = three();
        let io = run(&mut f, &["k", "K", "J", "j"]);
        assert!(io.out.contains("(3/3) "));
        assert!(io.out.contains("(2/3) "));
        let io = run(&mut file(&[(3, "three")]), &["j", "K", "g", "/x"]);
        assert!(io.out.contains("No other undecided hunk\n"));
        assert!(io.out.contains("No other hunk\n"));
        assert!(io.out.contains("No other hunks to goto\n"));
        assert!(io.out.contains("No other hunks to search\n"));
    }

    #[test]
    fn g_lists_hunks_and_jumps() {
        let io = run(&mut three(), &["y", "g", "3", "q"]);
        assert!(io.out.contains("+ 1:  -1,6 +1,6          -3\n"));
        assert!(io.out.contains("  3:  -35,6 +35,6        -38\n"));
        assert!(io.out.contains("go to which hunk? "));
        assert!(io.out.contains("(3/3) "));
        let io = run(&mut file(&[(3, "x"), (20, "y")]), &["g9", "gz", "g 2"]);
        assert!(io.out.contains("Sorry, only 2 hunks available.\n"));
        assert!(io.out.contains("Invalid number: 'z'\n"));
        assert!(io.out.contains("(2/2) "));
    }

    #[test]
    fn slash_searches_from_the_current_hunk() {
        let io = run(&mut three(), &["/twen", "/nomatch", "/(", "/", ""]);
        assert!(io.out.contains("(2/3) "));
        assert!(io.out.contains("No hunk matches the given pattern\n"));
        assert!(io.out.contains("Malformed search regexp ("));
        assert!(io.out.contains("search for regex? "));
    }

    #[test]
    fn s_splits_a_hunk_and_says_how_many() {
        let mut f = file(&[(5, "five"), (7, "seven"), (11, "eleven")]);
        let io = run(&mut f, &["s"]);
        assert!(io.out.contains("Split into 3 hunks.\n"));
        assert_eq!(f.items.len(), 3);
        let io = run(&mut file(&[(5, "five")]), &["s"]);
        assert!(io.out.contains("Sorry, cannot split this hunk\n"));
    }

    #[test]
    fn the_end_of_input_stops_all_files_and_a_decided_hunk_shows_its_answer() {
        let mut f = three();
        let mut io = Script::new(&["y", "K"]);
        let flow = update_file(&mut f, &WORDS, &mut io, &mut |_, _| {});
        assert!(flow == Flow::Quit);
        assert!(io.out.contains("Stage this hunk (was: y) ["));
    }

    #[test]
    fn an_edited_hunk_that_fits_is_chosen() {
        let mut f = file(&[(5, "five")]);
        let mut io = Script::new(&["e"]);
        io.edits =
            vec![b"@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+FIVE\n 6\n 7\n 8\n".to_vec()].into_iter();
        update_file(&mut f, &WORDS, &mut io, &mut |_, _| {});
        assert_eq!(f.uses, [Use::Yes]);
        let Item::Hunk(h) = &f.items[0] else { panic!() };
        assert!(h.render().contains("+FIVE"));
    }

    #[test]
    fn an_edit_that_does_not_fit_asks_again_and_no_discards() {
        let mut f = file(&[(5, "five")]);
        let mut io = Script::new(&["e", "x", "y", "n", "q"]);
        io.edits = vec![b" 99\n-5\n+x\n".to_vec(); 2].into_iter();
        update_file(&mut f, &WORDS, &mut io, &mut |_, _| {});
        assert_eq!(f.uses, [Use::No]);
        assert!(io.err.contains("error: patch failed: f:"));
        assert_eq!(
            io.out
                .matches("Your edited hunk does not apply. Edit again")
                .count(),
            3
        );
    }
}
