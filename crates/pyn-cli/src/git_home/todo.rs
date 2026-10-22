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
    fn subject(&self, id: &ObjectId) -> Result<String>;
    /// The text after the id on a sheet line: the subject, or `rebase.instructionFormat` applied.
    fn label(&self, id: &ObjectId) -> Result<String>;
    /// Whether the commit changes nothing against its first parent.
    fn is_empty(&self, id: &ObjectId) -> Result<bool>;
}

impl Commits for GitHome {
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
        let ctx = Context {
            style: DateStyle::parse("default")?,
            now: chrono::Utc::now().timestamp(),
            decorations: &decorations,
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
            Cmd::Pick | Cmd::Reword | Cmd::Edit | Cmd::Squash | Cmd::Fixup
        )
    }
}

/// One line of the sheet: the commit it names, or the command line of an `exec`.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub cmd: Cmd,
    pub id: Option<ObjectId>,
    pub arg: String,
    pub flag: Flag,
}

impl Item {
    pub(crate) fn pick(id: ObjectId) -> Self {
        Self {
            cmd: Cmd::Pick,
            id: Some(id),
            arg: String::new(),
            flag: Flag::None,
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
        }
    }

    pub(crate) fn exec(line: &str) -> Self {
        Self {
            cmd: Cmd::Exec,
            id: None,
            arg: line.to_string(),
            flag: Flag::None,
        }
    }

    /// The sheet line; `abbrev` cuts the id to seven digits for the editor.
    pub(crate) fn line(&self, git: &dyn Commits, abbrev: bool) -> Result<String> {
        let word = self.cmd.word();
        Ok(match (self.cmd, &self.id) {
            (Cmd::Exec, _) => format!("exec {}\n", self.arg),
            (_, Some(id)) => {
                let shown = if abbrev {
                    id.to_hex()[..7].to_string()
                } else {
                    id.to_hex()
                };
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
                items.push(Item {
                    cmd,
                    id: Some(id),
                    arg: String::new(),
                    flag,
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

/// The text of the sheet as the editor opens it: the commands, then the summary and the help.
pub(crate) fn editor_text(
    git: &dyn Commits,
    items: &[Item],
    range: &str,
    onto: &ObjectId,
) -> Result<String> {
    let count = items.iter().filter(|i| i.cmd != Cmd::Noop).count();
    let mut text = sheet(git, items, true)?;
    text.push_str(&format!(
        "\n# Rebase {range} onto {} ({count} command{})\n#\n",
        &onto.to_hex()[..7],
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
            "pick 1111111 subject\nfixup -C 1111111 subject\nfixup -c 1111111 subject\nfixup 1111111 subject\n"
        );
        let errors = parse(&Names, &format!("pick {id}\nfixup -C\n"), false).unwrap_err();
        assert_eq!(errors[0], "error: missing arguments for fixup");
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
            },
            Item::pick(id),
        ];
        let out = with_exec(items, &["make".to_string()]);
        let words: Vec<&str> = out.iter().map(|i| i.cmd.word()).collect();
        assert_eq!(words, ["pick", "squash", "exec", "pick", "exec"]);
    }
}
