//! The instruction sheet of an interactive rebase: commands, parsing, the sheet the editor sees, `--autosquash`
//! and `--exec`. The text is git's own, so either tool can read what the other wrote.

use std::collections::HashMap;

use anyhow::Result;
use pyn_object::ObjectId;

use super::GitHome;
use super::pick::subject_of;
use crate::commit_format::{Context, DateStyle, Decorations, Pretty, render};

/// What a sheet needs from the repository it is about: the commit a name means and a commit's subject.
pub(crate) trait Commits {
    fn find(&self, spec: &str) -> Option<ObjectId>;
    /// Server local-store sheets retain their existing width.
    fn abbreviate(&self, id: &ObjectId) -> Result<String> {
        Ok(id.to_hex()[..7].to_string())
    }
    fn subject(&self, id: &ObjectId) -> Result<String>;
    /// The text after the id on a sheet line: the subject, or `rebase.instructionFormat` applied.
    fn label(&self, id: &ObjectId) -> Result<String>;
    /// Whether the commit changes nothing against its first parent.
    fn is_empty(&self, id: &ObjectId) -> Result<bool>;
}

impl Commits for GitHome {
    fn abbreviate(&self, id: &ObjectId) -> Result<String> {
        Ok(self.repo.abbreviate(id)?)
    }
    fn find(&self, spec: &str) -> Option<ObjectId> {
        self.repo
            .resolve(spec)
            .ok()
            .filter(|id| self.repo.commit(id).is_ok())
    }

    fn subject(&self, id: &ObjectId) -> Result<String> {
        Ok(subject_of(&self.repo.commit(id)?.message))
    }

    fn label(&self, id: &ObjectId) -> Result<String> {
        let Some(format) = self.repo.config().get("rebase.instructionformat") else {
            return self.subject(id);
        };
        let pretty = Pretty::parse(&format!("format:{format}"))?;
        let decorations = Decorations::new();
        let mailmap = self.mailmap()?;
        let abbreviations = self.repo.abbreviations(None)?;
        let ctx = Context {
            style: DateStyle::parse("default")?,
            now: chrono::Utc::now().timestamp(),
            decorations: &decorations,
            commit_color: None,
            tabs: 8,
            abbrev_commit: false,
            mailmap: Some(&mailmap),
            map_headers: false,
            abbreviations: &crate::commit_format::IdFormat::Git(abbreviations),
        };
        let text = render(&pretty, &ctx, id, &self.repo.commit(id)?);
        Ok(text.replace('\n', " "))
    }

    fn is_empty(&self, id: &ObjectId) -> Result<bool> {
        let commit = self.repo.commit(id)?;
        let Some(parent) = commit.parents.first() else {
            return Ok(false);
        };
        Ok(self.repo.commit(parent)?.tree == commit.tree)
    }
}

/// How a `fixup` treats the messages: `-C` takes this commit's, `-c` also opens the editor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Flag {
    #[default]
    None,
    Replace,
    Edit,
}

impl Flag {
    fn word(self) -> &'static str {
        match self {
            Flag::None => "",
            Flag::Replace => " -C",
            Flag::Edit => " -c",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cmd {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
    Exec,
    Break,
    Noop,
    Label,
    Reset,
    Merge,
    UpdateRef,
    /// A blank or `#` line; it is kept so the file reads as git wrote it.
    Comment,
}

impl Cmd {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Cmd::Pick => "pick",
            Cmd::Reword => "reword",
            Cmd::Edit => "edit",
            Cmd::Squash => "squash",
            Cmd::Fixup => "fixup",
            Cmd::Drop => "drop",
            Cmd::Exec => "exec",
            Cmd::Break => "break",
            Cmd::Noop => "noop",
            Cmd::Label => "label",
            Cmd::Reset => "reset",
            Cmd::Merge => "merge",
            Cmd::UpdateRef => "update-ref",
            Cmd::Comment => "",
        }
    }

    pub(crate) fn from_word(word: &str) -> Option<Cmd> {
        Some(match word {
            "pick" | "p" => Cmd::Pick,
            "reword" | "r" => Cmd::Reword,
            "edit" | "e" => Cmd::Edit,
            "squash" | "s" => Cmd::Squash,
            "fixup" | "f" => Cmd::Fixup,
            "drop" | "d" => Cmd::Drop,
            "exec" | "x" => Cmd::Exec,
            "break" | "b" => Cmd::Break,
            "noop" => Cmd::Noop,
            "label" | "l" => Cmd::Label,
            "reset" | "t" => Cmd::Reset,
            "merge" | "m" => Cmd::Merge,
            "update-ref" | "u" => Cmd::UpdateRef,
            _ => return None,
        })
    }

    pub(crate) fn melds(self) -> bool {
        matches!(self, Cmd::Squash | Cmd::Fixup)
    }

    /// Whether the command makes a commit that stays in the history.
    fn makes_commit(self) -> bool {
        matches!(
            self,
            Cmd::Pick | Cmd::Reword | Cmd::Edit | Cmd::Squash | Cmd::Fixup | Cmd::Merge
        )
    }
}

/// One line of the sheet: the commit it names, or the rest of the line for `exec`, `label`, `reset`, `merge` (the
/// labels and the `#` oneline) and `update-ref`; a comment keeps its whole line.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub cmd: Cmd,
    pub id: Option<ObjectId>,
    pub arg: String,
    pub flag: Flag,
    /// The text after the commit was written by hand or by git, and is kept as it is.
    pub given: bool,
}

impl Item {
    pub(crate) fn pick(id: ObjectId) -> Self {
        Self {
            cmd: Cmd::Pick,
            id: Some(id),
            arg: String::new(),
            flag: Flag::None,
            given: false,
        }
    }

    pub(crate) fn noop() -> Self {
        Self::plain(Cmd::Noop)
    }

    fn plain(cmd: Cmd) -> Self {
        Self {
            cmd,
            id: None,
            arg: String::new(),
            flag: Flag::None,
            given: false,
        }
    }

    pub(crate) fn exec(line: &str) -> Self {
        Self::with_arg(Cmd::Exec, line)
    }

    pub(crate) fn with_arg(cmd: Cmd, arg: &str) -> Self {
        Self {
            cmd,
            id: None,
            arg: arg.to_string(),
            flag: Flag::None,
            given: false,
        }
    }

    pub(crate) fn counts(&self) -> bool {
        self.cmd != Cmd::Comment
    }

    /// The sheet line; `abbrev` uses the repository's unique prefix for the editor.
    pub(crate) fn line(&self, git: &dyn Commits, abbrev: bool) -> Result<String> {
        let word = self.cmd.word();
        Ok(match (self.cmd, &self.id) {
            (Cmd::Comment, _) => format!("{}\n", self.arg),
            (Cmd::Exec | Cmd::Label | Cmd::Reset | Cmd::UpdateRef, _) => {
                format!("{word} {}\n", self.arg)
            }
            (Cmd::Merge, id) => {
                let mut line = word.to_string();
                if let Some(id) = id {
                    let shown = if abbrev {
                        git.abbreviate(id)?
                    } else {
                        id.to_hex()
                    };
                    line.push_str(&format!("{} {shown}", self.flag.word()));
                }
                if !self.arg.is_empty() {
                    line.push(' ');
                    line.push_str(&self.arg);
                }
                line.push('\n');
                line
            }
            (_, Some(id)) => {
                let shown = if abbrev {
                    git.abbreviate(id)?
                } else {
                    id.to_hex()
                };
                if self.given {
                    let sep = if self.arg.is_empty() { "" } else { " " };
                    return Ok(format!(
                        "{word}{} {shown}{sep}{}\n",
                        self.flag.word(),
                        self.arg
                    ));
                }
                let empty = if git.is_empty(id)? { " # empty" } else { "" };
                format!(
                    "{word}{} {shown} {}{empty}\n",
                    self.flag.word(),
                    git.label(id)?
                )
            }
            _ => format!("{word}\n"),
        })
    }
}

pub(crate) fn sheet(git: &dyn Commits, items: &[Item], abbrev: bool) -> Result<String> {
    items.iter().map(|i| i.line(git, abbrev)).collect()
}

/// The items of a sheet; a bad line collects git's error lines instead. `prior` says commits were already done,
/// so a sheet may start with `squash`.
pub(crate) fn parse(git: &dyn Commits, text: &str, prior: bool) -> Result<Vec<Item>, Vec<String>> {
    let mut items: Vec<Item> = Vec::new();
    let mut errors = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            items.push(Item::with_arg(Cmd::Comment, line.trim_end()));
            continue;
        }
        let (word, rest) = trimmed
            .split_once(char::is_whitespace)
            .map_or((trimmed, ""), |(w, r)| (w, r.trim_start()));
        let bad = |errors: &mut Vec<String>, why: String| {
            errors.push(format!("error: {why}"));
            errors.push(format!("error: invalid line {}: {line}", n + 1));
        };
        let Some(cmd) = Cmd::from_word(word) else {
            bad(&mut errors, format!("invalid command '{word}'"));
            continue;
        };
        match cmd {
            Cmd::Exec if rest.is_empty() => {
                bad(&mut errors, "missing arguments for exec".into());
            }
            Cmd::Exec => items.push(Item::exec(rest)),
            Cmd::Break | Cmd::Noop => items.push(Item::plain(cmd)),
            Cmd::Label | Cmd::Reset | Cmd::UpdateRef if rest.is_empty() => {
                bad(&mut errors, format!("missing arguments for {}", cmd.word()));
            }
            Cmd::Label if !super::rebase_merges::valid_label(rest) => {
                bad(&mut errors, format!("'{rest}' is not a valid label"));
            }
            Cmd::Label | Cmd::Reset | Cmd::UpdateRef => items.push(Item::with_arg(cmd, rest)),
            Cmd::Merge => match merge_item(git, rest) {
                Ok(item) => items.push(item),
                Err(why) => bad(&mut errors, why),
            },
            _ => {
                let mut words = rest.split_whitespace();
                let mut flag = Flag::None;
                let mut rev = words.next();
                if cmd == Cmd::Fixup
                    && let Some(f @ ("-C" | "-c")) = rev
                {
                    flag = if f == "-C" { Flag::Replace } else { Flag::Edit };
                    rev = words.next();
                }
                let Some(rev) = rev else {
                    bad(&mut errors, format!("missing arguments for {}", cmd.word()));
                    continue;
                };
                if rev.starts_with('-') {
                    bad(
                        &mut errors,
                        format!(
                            "'{} {rev}' is not built; pyn takes a plain {}",
                            cmd.word(),
                            cmd.word()
                        ),
                    );
                    continue;
                }
                let Some(id) = git.find(rev) else {
                    bad(&mut errors, format!("could not parse '{rev}'"));
                    continue;
                };
                if cmd.melds()
                    && !prior
                    && !items.iter().any(|i| i.cmd.makes_commit() && !i.cmd.melds())
                {
                    bad(
                        &mut errors,
                        format!("cannot '{}' without a previous commit", cmd.word()),
                    );
                    continue;
                }
                let text = rest.split_once(rev).map_or("", |(_, after)| after.trim());
                items.push(Item {
                    cmd,
                    id: Some(id),
                    arg: text.to_string(),
                    flag,
                    given: true,
                });
            }
        }
    }
    if errors.is_empty() {
        Ok(items)
    } else {
        Err(errors)
    }
}

/// A `merge` line: `[-C <commit> | -c <commit>] <label>... [# <oneline>]`; without a commit the editor opens.
fn merge_item(git: &dyn Commits, rest: &str) -> Result<Item, String> {
    let mut item = Item::with_arg(Cmd::Merge, rest);
    item.flag = Flag::Edit;
    let Some(tail) = rest
        .strip_prefix("-C")
        .map(|t| (Flag::Replace, t))
        .or_else(|| rest.strip_prefix("-c").map(|t| (Flag::Edit, t)))
    else {
        if rest.is_empty() {
            return Err("missing arguments for merge".into());
        }
        return Ok(item);
    };
    let tail = (tail.0, tail.1.trim_start());
    let (rev, arg) = tail
        .1
        .split_once(char::is_whitespace)
        .map_or((tail.1, ""), |(r, a)| (r, a.trim_start()));
    let Some(id) = git.find(rev) else {
        return Err(format!("could not parse '{rev}'"));
    };
    item.flag = tail.0;
    item.id = Some(id);
    item.arg = arg.to_string();
    Ok(item)
}

/// What the editor leaves, cleaned as `git stripspace --strip-comments` does: no comment lines, trailing blanks or
/// doubled blank lines.
pub(crate) fn stripspace(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.iter().map(|l| format!("{l}\n")).collect()
}

/// The text of the sheet as the editor opens it: the commands, then the summary and the help.
pub(crate) fn editor_text(
    git: &dyn Commits,
    items: &[Item],
    range: &str,
    onto: &ObjectId,
) -> Result<String> {
    let count = items
        .iter()
        .filter(|i| i.counts() && i.cmd != Cmd::Noop)
        .count();
    let mut text = sheet(git, items, true)?;
    text.push_str(&format!(
        "\n# Rebase {range} onto {} ({count} command{})\n#\n",
        git.abbreviate(onto)?,
        if count == 1 { "" } else { "s" }
    ));
    text.push_str(&format!(
        "{HELP_COMMANDS}#\n{HELP_ORDER}#\n# If you remove a line here THAT COMMIT WILL BE LOST.\n#\n{HELP_ABORT}#\n"
    ));
    Ok(text)
}

/// The help under the sheet `--edit-todo` opens.
pub(crate) fn edit_help(strict: bool) -> String {
    let lost = if strict {
        "# Do not remove any line. Use 'drop' explicitly to remove a commit.\n"
    } else {
        "# If you remove a line here THAT COMMIT WILL BE LOST.\n"
    };
    format!(
        "#\n{HELP_COMMANDS}#\n{HELP_ORDER}#\n{lost}#\n# You are editing the todo file of an ongoing interactive rebase.\n# To continue rebase after editing, run:\n#     pyn rebase --continue\n#\n"
    )
}

const HELP_COMMANDS: &str = "\
# Commands:
# p, pick <commit> = use commit
# r, reword <commit> = use commit, but edit the commit message
# e, edit <commit> = use commit, but stop for amending
# s, squash <commit> = use commit, but meld into previous commit
# f, fixup [-C | -c] <commit> = like \"squash\" but keep only the previous
#                    commit's log message, unless -C is used, in which case
#                    keep only this commit's message; -c is same as -C but
#                    opens the editor
# x, exec <command> = run command (the rest of the line) using shell
# b, break = stop here (continue rebase later with 'pyn rebase --continue')
# d, drop <commit> = remove commit
# l, label <label> = label current HEAD with a name
# t, reset <label> = reset HEAD to a label
# m, merge [-C <commit> | -c <commit>] <label> [# <oneline>]
#         create a merge commit using the original merge commit's
#         message (or the oneline, if no original merge commit was
#         specified); use -c <commit> to reword the commit message
# u, update-ref <ref> = track a placeholder for the <ref> to be updated
#                       to this position in the new commits. The <ref> is
#                       updated at the end of the rebase
";
const HELP_ORDER: &str = "# These lines can be re-ordered; they are executed from top to bottom.\n";
const HELP_ABORT: &str = "# However, if you remove everything, the rebase will be aborted.\n";

/// The command a `fixup!`, `amend!` (when `amend` is set) or `squash!` subject asks for, and the subject it names.
fn fixup_target(subject: &str, amend: bool) -> Option<(Cmd, Flag, &str)> {
    let strip = |s: &str| -> Option<(Cmd, Flag, usize)> {
        for (prefix, cmd, flag) in [
            ("fixup! ", Cmd::Fixup, Flag::None),
            ("amend! ", Cmd::Fixup, Flag::Replace),
            ("squash! ", Cmd::Squash, Flag::None),
        ] {
            if s.starts_with(prefix) && (amend || flag != Flag::Replace) {
                return Some((cmd, flag, prefix.len()));
            }
        }
        None
    };
    let (cmd, flag, len) = strip(subject)?;
    let mut rest = subject[len..].trim_start();
    while let Some((_, _, len)) = strip(rest) {
        rest = rest[len..].trim_start();
    }
    Some((cmd, flag, rest))
}

/// Moves each `fixup! <subject>` and `squash! <subject>` commit under the commit it names, as `--autosquash` does.
pub(crate) fn autosquash(git: &dyn Commits, items: Vec<Item>, amend: bool) -> Result<Vec<Item>> {
    let subjects: Vec<String> = items
        .iter()
        .map(|i| match &i.id {
            Some(id) => git.subject(id),
            None => Ok(String::new()),
        })
        .collect::<Result<_>>()?;
    let position: HashMap<ObjectId, usize> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| it.id.map(|id| (id, i)))
        .collect();
    let mut by_subject: HashMap<&str, usize> = HashMap::new();
    let mut next: Vec<Option<usize>> = vec![None; items.len()];
    let mut tail: Vec<Option<usize>> = vec![None; items.len()];
    let mut moved = vec![false; items.len()];
    let mut cmds: Vec<(Cmd, Flag)> = items.iter().map(|i| (i.cmd, i.flag)).collect();
    for i in 0..items.len() {
        let target = fixup_target(&subjects[i], amend).and_then(|(cmd, flag, name)| {
            let found = by_subject.get(name).copied().or_else(|| {
                let by_id = (!name.contains(' '))
                    .then(|| git.find(name))
                    .flatten()
                    .and_then(|id| position.get(&id).copied())
                    .filter(|p| *p < i);
                by_id.or_else(|| (0..i).find(|j| subjects[*j].starts_with(name)))
            });
            found.map(|j| (cmd, flag, j))
        });
        match target {
            Some((cmd, flag, j)) => {
                cmds[i] = (cmd, flag);
                moved[i] = true;
                let last = tail[j].unwrap_or(j);
                next[i] = next[last];
                next[last] = Some(i);
                tail[j] = Some(i);
            }
            None => {
                by_subject.entry(subjects[i].as_str()).or_insert(i);
            }
        }
    }
    let mut out = Vec::new();
    for i in (0..items.len()).filter(|i| !moved[*i]) {
        let mut at = Some(i);
        while let Some(c) = at {
            let mut item = items[c].clone();
            (item.cmd, item.flag) = cmds[c];
            out.push(item);
            at = next[c];
        }
    }
    Ok(out)
}

/// `exec` lines after every commit (and the `squash`/`fixup` lines that join it), as `--exec` adds them.
pub(crate) fn with_exec(items: Vec<Item>, commands: &[String]) -> Vec<Item> {
    let mut out = Vec::new();
    let mut open = false;
    let flush = |out: &mut Vec<Item>| {
        out.extend(commands.iter().map(|c| Item::exec(c)));
    };
    for item in items {
        if open && !item.cmd.melds() {
            flush(&mut out);
            open = false;
        }
        open = open || item.cmd.makes_commit();
        out.push(item);
    }
    if open {
        flush(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_prefixes_are_stripped_to_the_target_subject() {
        assert_eq!(
            fixup_target("fixup! fixup! squash! three", true),
            Some((Cmd::Fixup, Flag::None, "three"))
        );
        assert_eq!(
            fixup_target("squash! two", true),
            Some((Cmd::Squash, Flag::None, "two"))
        );
        assert_eq!(
            fixup_target("amend! two", true),
            Some((Cmd::Fixup, Flag::Replace, "two"))
        );
        assert_eq!(fixup_target("fixup!x", true), None);
        assert_eq!(fixup_target("amend! two", false), None);
    }

    struct Names;

    impl Commits for Names {
        fn find(&self, spec: &str) -> Option<ObjectId> {
            ObjectId::from_hex(pyn_object::Algorithm::Sha1, spec).ok()
        }
        fn subject(&self, _: &ObjectId) -> Result<String> {
            Ok("subject".into())
        }
        fn label(&self, _: &ObjectId) -> Result<String> {
            Ok("subject".into())
        }
        fn is_empty(&self, _: &ObjectId) -> Result<bool> {
            Ok(false)
        }
    }

    #[test]
    fn fixup_flags_are_parsed_and_written_back() {
        let id = "1".repeat(40);
        let text = format!("pick {id} a\nfixup -C {id} b\nfixup -c {id}\nfixup {id} d\n");
        let items = parse(&Names, &text, false).unwrap();
        let flags: Vec<Flag> = items.iter().map(|i| i.flag).collect();
        assert_eq!(flags, [Flag::None, Flag::Replace, Flag::Edit, Flag::None]);
        assert_eq!(
            sheet(&Names, &items, true).unwrap(),
            "pick 1111111 a\nfixup -C 1111111 b\nfixup -c 1111111\nfixup 1111111 d\n"
        );
        let errors = parse(&Names, &format!("pick {id}\nfixup -C\n"), false).unwrap_err();
        assert_eq!(errors[0], "error: missing arguments for fixup");
    }

    #[test]
    fn merge_label_reset_and_update_ref_lines_are_kept_as_written() {
        let id = "1".repeat(40);
        let text = format!(
            "label onto\n\nreset onto # x\nmerge -C {id} topic other # Merge branches\nmerge topic\nupdate-ref refs/heads/a\n"
        );
        let items = parse(&Names, &text, false).unwrap();
        let words: Vec<&str> = items.iter().map(|i| i.cmd.word()).collect();
        assert_eq!(
            words,
            ["label", "", "reset", "merge", "merge", "update-ref"]
        );
        assert_eq!(items[3].flag, Flag::Replace);
        assert_eq!(items[3].arg, "topic other # Merge branches");
        assert_eq!((items[4].flag, items[4].id), (Flag::Edit, None));
        assert_eq!(
            sheet(&Names, &items, true).unwrap(),
            "label onto\n\nreset onto # x\nmerge -C 1111111 topic other # Merge branches\nmerge topic\nupdate-ref refs/heads/a\n"
        );
        let errors = parse(&Names, "label a..b\nreset\nmerge -C zzz t\n", false).unwrap_err();
        assert_eq!(errors[0], "error: 'a..b' is not a valid label");
        assert_eq!(errors[2], "error: missing arguments for reset");
        assert_eq!(errors[4], "error: could not parse 'zzz'");
    }

    #[test]
    fn stripspace_drops_comments_and_collapses_blank_lines() {
        assert_eq!(
            stripspace("\n\nlabel onto  \n\n\n# note\nreset onto\n\n#c\n\n"),
            "label onto\n\nreset onto\n"
        );
        assert_eq!(stripspace("# only\n\n"), "");
    }

    #[test]
    fn exec_lines_follow_each_commit_group() {
        let id = ObjectId::from_hex(pyn_object::Algorithm::Sha1, &"1".repeat(40)).unwrap();
        let items = vec![
            Item::pick(id),
            Item {
                cmd: Cmd::Squash,
                id: Some(id),
                arg: String::new(),
                flag: Flag::None,
                given: false,
            },
            Item::pick(id),
        ];
        let out = with_exec(items, &["make".to_string()]);
        let words: Vec<&str> = out.iter().map(|i| i.cmd.word()).collect();
        assert_eq!(words, ["pick", "squash", "exec", "pick", "exec"]);
    }
}
