//! The menu of `pyn clean -i`: git's commands, prompts, column layout and answer handling, over any reader and writer.

use std::io::{BufRead, Write};

use pyn_gitwork::{Ignore, Level, parse_rules};

const COMMANDS: [&str; 6] = [
    "clean",
    "filter by pattern",
    "select by numbers",
    "ask each",
    "quit",
    "help",
];

const MENU_HELP: &str = concat!(
    "clean               - start cleaning\n",
    "filter by pattern   - exclude items from deletion\n",
    "select by numbers   - select items to be deleted by numbers\n",
    "ask each            - confirm each deletion (like \"rm -i\")\n",
    "quit                - stop cleaning\n",
    "help                - this screen\n",
    "?                   - help for prompt selection\n",
);

const SINGLE_HELP: &str = concat!(
    "Prompt help:\n",
    "1          - select a numbered item\n",
    "foo        - select item based on unique prefix\n",
    "           - (empty) select nothing\n",
);

const MULTI_HELP: &str = concat!(
    "Prompt help:\n",
    "1          - select a single item\n",
    "3-5        - select a range of items\n",
    "2-3,6-9    - select multiple ranges\n",
    "foo        - select item based on unique prefix\n",
    "-...       - unselect specified items\n",
    "*          - choose all items\n",
    "           - (empty) finish selecting\n",
);

const NO_MORE: &str = "No more files to clean, exiting.\n";
const SCREEN_WIDTH: usize = 80;

pub(super) enum Outcome {
    /// Indexes of the entries to remove, in order.
    Run(Vec<usize>),
    /// Nothing is removed.
    Quit,
}

/// Lines in, text out; a line that ends the input without a newline makes the next read fail but keep the old text.
pub(super) struct Console<R, W> {
    input: R,
    out: W,
    eof: bool,
    width: usize,
}

impl<R: BufRead, W: Write> Console<R, W> {
    pub(super) fn new(input: R, out: W, width: usize) -> Self {
        Self {
            input,
            out,
            eof: false,
            width,
        }
    }

    fn say(&mut self, text: &str) {
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    /// Reads a line without its newline into `buf`; false at the end of the input, which leaves `buf` as it was
    /// when an unterminated line came before.
    fn line(&mut self, buf: &mut String) -> bool {
        if self.eof {
            return false;
        }
        buf.clear();
        let mut bytes = Vec::new();
        match self.input.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => {
                self.eof = true;
                false
            }
            Ok(_) => {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                } else {
                    self.eof = true;
                }
                buf.push_str(&String::from_utf8_lossy(&bytes));
                true
            }
        }
    }

    fn read(&mut self) -> Option<String> {
        let mut buf = String::new();
        self.line(&mut buf).then_some(buf)
    }

    fn table(&mut self, cells: &[String], by_rows: bool) {
        let text = layout(cells, by_rows, self.width);
        self.say(&text);
    }
}

/// Cells in `cols` columns, filled down the columns or along the rows, indented by two and padded by two.
fn layout(cells: &[String], by_rows: bool, term: usize) -> String {
    let widest = cells.iter().map(|c| c.chars().count()).max().unwrap_or(0) + 2;
    let cols = (term.saturating_sub(3) / widest).max(1);
    let rows = cells.len().div_ceil(cols);
    let mut out = String::new();
    for row in 0..rows {
        out.push_str("  ");
        for col in 0..cols {
            let at = if by_rows {
                row * cols + col
            } else {
                col * rows + row
            };
            let Some(cell) = cells.get(at) else { break };
            out.push_str(cell);
            let step = if by_rows { 1 } else { rows };
            if col + 1 == cols || at + step >= cells.len() {
                break;
            }
            for _ in cell.chars().count()..widest {
                out.push(' ');
            }
        }
        out.push('\n');
    }
    out
}

fn numbered(names: &[&str], marked: &[bool]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mark = if marked.get(i) == Some(&true) {
                '*'
            } else {
                ' '
            };
            format!("{mark}{:>2}: {name}", i + 1)
        })
        .collect()
}

/// What one answer asked for: `None` for a word that names nothing, which the caller reports.
enum Pick {
    Items(Vec<usize>),
    Huh,
}

fn number(text: &str) -> Option<usize> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().unwrap_or(usize::MAX))
}

fn by_prefix(names: &[&str], word: &str) -> Pick {
    if word.is_empty() {
        return Pick::Huh;
    }
    let word = word.to_lowercase();
    let mut hits = (0..names.len()).filter(|i| names[*i].to_lowercase().starts_with(&word));
    match (hits.next(), hits.next()) {
        (Some(i), None) => Pick::Items(vec![i]),
        _ => Pick::Huh,
    }
}

fn pick(names: &[&str], word: &str, ranges: bool) -> Pick {
    let count = names.len();
    if let Some(n) = number(word) {
        return if (1..=count).contains(&n) {
            Pick::Items(vec![n - 1])
        } else {
            Pick::Huh
        };
    }
    if !ranges {
        return by_prefix(names, word);
    }
    if word == "*" {
        return Pick::Items((0..count).collect());
    }
    if let Some((low, high)) = word.split_once('-') {
        let low_n = number(low);
        let high_n = if high.is_empty() {
            Some(count)
        } else {
            number(high)
        };
        if let (Some(low), Some(high)) = (low_n, high_n) {
            return if low >= 1 && low <= high && high <= count {
                Pick::Items((low - 1..high).collect())
            } else {
                Pick::Huh
            };
        }
    }
    by_prefix(names, word)
}

/// The command chosen from the main menu: its number from zero, or nothing yet.
fn command<R: BufRead, W: Write>(io: &mut Console<R, W>) -> Option<usize> {
    let names: Vec<&str> = COMMANDS.to_vec();
    loop {
        io.say("*** Commands ***\n");
        let cells = numbered(&names, &[]);
        io.table(&cells, true);
        io.say("What now> ");
        let Some(answer) = io.read() else {
            io.say("Bye.\n");
            return Some(4);
        };
        let answer = answer.trim();
        if answer.is_empty() {
            continue;
        }
        if answer == "?" {
            io.say(SINGLE_HELP);
            continue;
        }
        let (unselect, word) = match answer.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, answer),
        };
        match pick(&names, word, false) {
            Pick::Huh => io.say(&format!("Huh ({word})?\n")),
            Pick::Items(found) if !unselect => {
                if found[0] == 4 {
                    io.say("Bye.\n");
                }
                return found.first().copied();
            }
            Pick::Items(_) => {}
        }
    }
}

fn print_names<R: BufRead, W: Write>(io: &mut Console<R, W>, names: &[&str], live: &[usize]) {
    let cells: Vec<String> = live.iter().map(|i| names[*i].to_string()).collect();
    io.table(&cells, false);
}

/// Whether the rules hit the entry or a folder above it; a folder entry is also tried with its trailing slash.
fn excluded(ignore: &Ignore, levels: &[Level], name: &str) -> bool {
    let is_dir = name.ends_with('/');
    let bare = name.strip_suffix('/').unwrap_or(name);
    let above = bare
        .match_indices('/')
        .any(|(at, _)| ignore.is_ignored(levels, &bare.as_bytes()[..at], true));
    above
        || ignore.is_ignored(levels, bare.as_bytes(), is_dir)
        || (is_dir && ignore.is_ignored(levels, name.as_bytes(), true))
}

fn filter<R: BufRead, W: Write>(io: &mut Console<R, W>, names: &[&str], live: &mut Vec<usize>) {
    print_names(io, names, live);
    loop {
        io.say("Input ignore patterns>> ");
        let Some(line) = io.read() else {
            io.say("\n");
            return;
        };
        if line.is_empty() {
            return;
        }
        let patterns: Vec<&str> = line.split(' ').filter(|p| !p.is_empty()).collect();
        let levels = [Level::new(b"", parse_rules(patterns.join("\n").as_bytes()))];
        let ignore = Ignore::default();
        let before = live.len();
        live.retain(|i| !excluded(&ignore, &levels, names[*i]));
        if live.len() == before {
            io.say(&format!("WARNING: Cannot find items matched by: {line}\n"));
        } else if live.is_empty() {
            return;
        } else {
            print_names(io, names, live);
        }
    }
}

fn select<R: BufRead, W: Write>(io: &mut Console<R, W>, names: &[&str], live: &mut Vec<usize>) {
    let shown: Vec<&str> = live.iter().map(|i| names[*i]).collect();
    let mut marked = vec![false; shown.len()];
    loop {
        let cells = numbered(&shown, &marked);
        io.table(&cells, true);
        io.say("Select items to delete>> ");
        let Some(answer) = io.read() else {
            live.clear();
            return;
        };
        let answer = answer.trim();
        if answer.is_empty() {
            break;
        }
        if answer == "?" {
            io.say(MULTI_HELP);
            continue;
        }
        for word in answer.split([' ', ',']).filter(|w| !w.is_empty()) {
            let (on, word) = match word.strip_prefix('-') {
                Some(rest) => (false, rest),
                None => (true, word),
            };
            match pick(&shown, word, true) {
                Pick::Items(found) => found.into_iter().for_each(|i| marked[i] = on),
                Pick::Huh => io.say(&format!("Huh ({word})?\n")),
            }
        }
    }
    let kept: Vec<usize> = live
        .iter()
        .zip(&marked)
        .filter_map(|(i, on)| on.then_some(*i))
        .collect();
    *live = kept;
}

fn ask_each<R: BufRead, W: Write>(
    io: &mut Console<R, W>,
    names: &[&str],
    live: &[usize],
) -> Vec<usize> {
    let mut confirm = String::new();
    let mut ended = false;
    let mut keep = Vec::new();
    for i in live {
        if !ended {
            io.say(&format!("Remove {} [y/N]? ", names[*i]));
            if !io.line(&mut confirm) {
                io.say("\n");
                ended = true;
            }
        }
        if !confirm.is_empty() && "yes".starts_with(&confirm.to_lowercase()) {
            keep.push(*i);
        }
    }
    keep
}

/// Runs the menu over the entry names until something is chosen to remove or the person quits.
pub(super) fn run<R: BufRead, W: Write>(io: &mut Console<R, W>, names: &[&str]) -> Outcome {
    let mut live: Vec<usize> = (0..names.len()).collect();
    loop {
        if live.is_empty() {
            io.say(NO_MORE);
            return Outcome::Quit;
        }
        let one = live.len() == 1;
        io.say(if one {
            "Would remove the following item:\n"
        } else {
            "Would remove the following items:\n"
        });
        print_names(io, names, &live);
        match command(io) {
            Some(0) => return Outcome::Run(live),
            Some(1) => filter(io, names, &mut live),
            Some(2) => select(io, names, &mut live),
            Some(3) => return Outcome::Run(ask_each(io, names, &live)),
            Some(5) => io.say(MENU_HELP),
            _ => return Outcome::Quit,
        }
    }
}

/// Columns of the terminal as git reads them: `COLUMNS`, else 80.
pub(super) fn screen_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|c| *c > 0)
        .unwrap_or(SCREEN_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(input: &str, names: &[&str]) -> (String, Option<Vec<usize>>) {
        let mut out = Vec::new();
        let mut io = Console::new(input.as_bytes(), &mut out, 80);
        let outcome = run(&mut io, names);
        let ran = match outcome {
            Outcome::Run(v) => Some(v),
            Outcome::Quit => None,
        };
        (String::from_utf8(out).unwrap(), ran)
    }

    const MENU: &str = "*** Commands ***\n    1: clean                2: filter by pattern    3: select by numbers\n    4: ask each             5: quit                 6: help\nWhat now> ";

    #[test]
    fn clean_runs_everything_and_prints_git_s_transcript() {
        let (out, ran) = session("1\n", &["a.txt", "b.log", "d/"]);
        assert_eq!(
            out,
            format!("Would remove the following items:\n  a.txt  b.log  d/\n{MENU}")
        );
        assert_eq!(ran, Some(vec![0, 1, 2]));
    }

    #[test]
    fn answers_are_prefixes_numbers_or_huh() {
        let (out, ran) = session("xyz\n\nQ\n", &["a"]);
        assert!(out.contains("What now> Huh (xyz)?\n*** Commands ***"));
        assert!(out.ends_with("What now> Bye.\n"));
        assert_eq!(ran, None);
        let (_, ran) = session("ASK\ny\n", &["a", "b"]);
        assert_eq!(ran, Some(vec![0]));
    }

    #[test]
    fn select_by_numbers_marks_ranges_and_unselects() {
        let names = ["a", "b", "c", "d"];
        let (out, ran) = session("3\n1-3 -2\n\n1\n", &names);
        assert!(out.contains("Select items to delete>>   * 1: a    2: b  * 3: c    4: d\n"));
        assert_eq!(ran, Some(vec![0, 2]));
        let (out, ran) = session("3\n9 x\n", &names);
        assert!(out.contains(">> Huh (9)?\nHuh (x)?\n"));
        assert!(out.ends_with("Select items to delete>> No more files to clean, exiting.\n"));
        assert_eq!(ran, None);
    }

    #[test]
    fn filter_drops_matches_and_warns_when_nothing_matches() {
        let names = ["a.txt", "b.log", "sub/"];
        let (out, ran) = session("2\n*.txt zz\nnope\n\n1\n", &names);
        assert!(out.contains("Input ignore patterns>>   b.log  sub/\nInput ignore patterns>> WARNING: Cannot find items matched by: nope\n"));
        assert_eq!(ran, Some(vec![1, 2]));
        let (out, _) = session("2\nsub\n*\n", &names);
        assert!(out.ends_with("Input ignore patterns>> No more files to clean, exiting.\n"));
    }

    #[test]
    fn ask_each_keeps_the_stale_answer_after_an_unterminated_last_line() {
        let names = ["a", "b", "c"];
        assert_eq!(session("4\ny\n", &names).1, Some(vec![0]));
        assert_eq!(session("4\ny", &names).1, Some(vec![0, 1, 2]));
        assert_eq!(session("4\nyes\nYy\nye\n", &names).1, Some(vec![0, 2]));
    }

    #[test]
    fn columns_fill_down_for_lists_and_along_for_menus() {
        let cells: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        assert_eq!(layout(&cells, false, 9), "  a  d\n  b  e\n  c\n");
        assert_eq!(layout(&cells, true, 9), "  a  b\n  c  d\n  e\n");
        assert_eq!(layout(&cells, true, 80), "  a  b  c  d  e\n");
    }
}
