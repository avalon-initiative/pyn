//! The `diff` and `whitespace` attributes on a git repository (`textconv`, `binary`, `funcname`, `xfuncname`,
//! `whitespace=`), compared with real git on a copy of the same repository: exit status class and the patch text.
//! Real git is the oracle for meaning only.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{FORMATS, copy_tree};

#[path = "common/oracle.rs"]
mod oracle;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl Pair {
    fn new(format: &str, script: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&git)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        let pyn = root.join("pyn");
        copy_tree(&git, &pyn);
        Self {
            _tmp: tmp,
            home,
            git,
            pyn,
        }
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn pyn_cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    /// Like `same`, run from the folder `sub` of the work tree.
    fn same_from(&self, sub: &str, args: &[&str]) {
        let g = self.git_in(&self.git.join(sub), args);
        let p = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(self.pyn.join(sub))
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap();
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{args:?} in {sub}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(text(&g.stdout), text(&p.stdout), "{args:?} in {sub}");
    }

    /// Runs `args` with both tools: the status class and standard output must agree.
    fn same(&self, args: &[&str]) {
        let (g, p) = (self.git_in(&self.git, args), self.pyn_cmd(args));
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{args:?}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(text(&g.stdout), text(&p.stdout), "{args:?}");
    }
}

fn each(script: &str, run: impl Fn(&Pair)) {
    for format in FORMATS {
        run(&Pair::new(format, script));
    }
}

/// `f` is committed, then edited and committed again, and edited once more in the work tree; `config` is shell run
/// before the first commit.
fn edited(attributes: &str, config: &str) -> String {
    format!(
        "printf '{attributes}' >.gitattributes; {config}\n\
         printf 'one\\ntwo\\nthree\\n' >f; git add -A .; git commit -qm base\n\
         printf 'one\\nTWO\\nthree\\nfour\\n' >f; git commit -qam second\n\
         printf 'one\\nTWO\\nthree\\nfour\\nfive\\n' >f; printf 'new\\n' >g; git add -N g\n"
    )
}

fn all_views(p: &Pair) {
    for args in [
        &["diff"][..],
        &["diff", "--no-textconv"],
        &["diff", "--textconv"],
        &["diff", "--stat"],
        &["diff", "--numstat"],
        &["diff", "--check"],
        &["diff", "-a"],
        &["diff", "HEAD~1", "HEAD"],
        &["show", "HEAD"],
        &["log", "-p", "--format=%s"],
    ] {
        p.same(args);
    }
}

#[test]
fn textconv_converts_both_sides_of_a_patch_only() {
    let script = edited(
        "f diff=up\\ng diff=up\\n",
        "git config diff.up.textconv 'tr a-z A-Z <'",
    );
    each(&script, all_views);
}

#[test]
fn textconv_gets_a_file_name_argument_and_may_fail() {
    for command in ["head -n 2", "false", "exit 3 #"] {
        let script = edited(
            "f diff=up\\n",
            &format!("git config diff.up.textconv '{command}'"),
        );
        each(&script, |p| {
            p.same(&["diff"]);
            p.same(&["diff", "--stat"]);
        });
    }
}

#[test]
fn textconv_output_that_is_equal_hides_the_difference() {
    let script = edited("f diff=up\\n", "git config diff.up.textconv 'echo same #'");
    each(&script, |p| {
        p.same(&["diff"]);
        p.same(&["diff", "HEAD~1", "HEAD"]);
    });
}

#[test]
fn a_driver_that_names_no_textconv_changes_nothing() {
    let script = edited(
        "f diff=up\\ng diff=nonesuch\\n",
        "git config diff.up.xfuncname '^x'",
    );
    each(&script, all_views);
}

#[test]
fn the_binary_setting_and_the_attribute_decide_what_is_binary() {
    for (attrs, config) in [
        ("f diff=b\\n", "git config diff.b.binary true"),
        ("f diff=b\\n", "git config diff.b.binary false"),
        ("f -diff\\n", "true"),
        ("f diff\\n", "true"),
        (
            "f diff=b\\n",
            "git config diff.b.binary true; git config diff.b.textconv 'cat <'",
        ),
    ] {
        each(&edited(attrs, config), all_views);
    }
    let nul = "printf 'f diff=b\\n' >.gitattributes; git config diff.b.binary false\n\
        printf 'a\\0\\nb\\n' >f; git add -A .; git commit -qm base; printf 'a\\0\\nB\\n' >f\n";
    each(nul, all_views);
}

/// Twenty lines with section lines in them, changed near the end and in the middle.
fn sections(attributes: &str, config: &str) -> String {
    format!(
        "printf '{attributes}' >.gitattributes; {config}\n\
         printf 'sec alpha\\nx\\nproc one\\ny\\nskip me\\nz\\nsec beta tail\\n1\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9\\n10\\nend\\n' >a.x\n\
         git add -A .; git commit -qm base\n\
         sed -i 's/^10$/ten/;s/^y$/yy/' a.x\n"
    )
}

#[test]
fn xfuncname_and_funcname_pick_the_hunk_heading() {
    for config in [
        "git config diff.fn.xfuncname '^(sec [a-z]+)'",
        "git config diff.fn.xfuncname '^sec'",
        "git config diff.fn.funcname '^\\(proc\\) [a-z]*'",
        "git config diff.fn.funcname '^p.o+c'",
        "git config diff.fn.xfuncname '!^skip\n^[a-z]+ ([a-z]+)$'",
        "git config diff.fn.xfuncname '^(nomatch)'",
    ] {
        let script = sections("*.x diff=fn\\n", config);
        each(&script, |p| {
            p.same(&["diff", "-U1"]);
            p.same(&["diff"]);
            p.same(&["show", "HEAD", "-U1"]);
        });
    }
}

#[test]
fn the_last_of_funcname_and_xfuncname_read_wins() {
    for config in [
        "git config diff.fn.xfuncname '^(sec [a-z]+)'; git config diff.fn.funcname '^\\(proc\\) [a-z]*'",
        "git config diff.fn.funcname '^\\(proc\\) [a-z]*'; git config diff.fn.xfuncname '^(sec [a-z]+)'",
        "git config diff.fn.xfuncname '^(sec [a-z]+)'; git config diff.fn.funcname '^\\(proc\\) [a-z]*'; \
         git config --add diff.fn.xfuncname '^(sec [a-z]+ tail)'",
    ] {
        let script = sections("*.x diff=fn\\n", config);
        each(&script, |p| {
            p.same(&["diff", "-U1"]);
            p.same(&["show", "HEAD", "-U1"]);
        });
    }
}

/// A function line of the language, then a line git's default pattern would take, filler and an edited line.
const LANGUAGES: [(&str, &str); 34] = [
    ("c", "int run(int x)\n{"),
    ("cpp", "Widget::Widget(int x)\n{"),
    ("java", "public class Foo {\n    public void run(int x) {"),
    ("csharp", "namespace Foo\n    public static void Run(int x)"),
    ("objc", "@implementation Foo\n- (void)run:(int)x"),
    ("python", "class Foo:\n    async def run(x):"),
    ("ruby", "module Foo\n  def run"),
    ("rust", "pub(crate) async fn run(x: u32) {"),
    ("golang", "func (s *S) Run(x int) {"),
    ("php", "public static function run($x)"),
    ("perl", "sub run($x) {"),
    ("tex", "\\section{Intro}"),
    ("markdown", "## Heading"),
    ("bibtex", "@article{key,"),
    ("html", "<h2 class=\"t\">Title</h2>"),
    ("css", ".sel > a {"),
    ("fortran", "      SUBROUTINE FOO(X)"),
    ("fortran", "subroutine foo(x)"),
    ("pascal", "procedure Run;"),
    ("pascal", "PROCEDURE Run;"),
    ("fountain", "INT. HOUSE - DAY"),
    ("fountain", "int. house - day"),
    ("matlab", "function y = f(x)"),
    ("scheme", "(define (run x)"),
    ("dts", "&uart0 {"),
    ("elixir", "  defmodule Foo do\n  def run(x) do"),
    ("ada", "procedure Run is"),
    ("ada", "PROCEDURE Run is"),
    ("kotlin", "class Foo {\n    fun run(x: Int) {"),
    ("c", "static int run(int x)"),
    ("java", "class A {\n    void run() throws Exception {"),
    ("rust", "impl Foo for Bar {"),
    ("python", "def run(x):"),
    ("cpp", "namespace foo {"),
];

fn language(name: &str, head: &str, attrs: &str, config: &str) -> String {
    format!(
        "printf '{attrs}' >.gitattributes; {config}\n\
         {{ cat <<'EOF'\n{head}\nzzz decoy\nEOF\n\
         for i in $(seq 1 20); do echo '    12'; done; echo '    12 old'; echo '    12'; }} >f.{name}\n\
         git add -A .; git commit -qm base\n\
         sed -i 's/12 old/12 new/' f.{name}\n"
    )
}

#[test]
fn builtin_language_drivers_pick_the_hunk_heading() {
    for (name, head) in LANGUAGES {
        let script = language(name, head, &format!("f.{name} diff={name}\\n"), "true");
        each(&script, |p| {
            p.same(&["diff"]);
            p.same(&["diff", "-U1"]);
        });
    }
}

#[test]
fn a_configured_pattern_overrides_a_builtin_driver() {
    for config in [
        "git config diff.python.xfuncname '^(zzz.*)'",
        "git config diff.python.funcname '^zzz'",
    ] {
        let script = language("py", "def run(x):", "f.py diff=python\\n", config);
        each(&script, |p| p.same(&["diff"]));
    }
}

#[test]
fn an_invalid_funcname_pattern_is_an_error() {
    let script = sections(
        "*.x diff=fn\\n",
        "git config diff.fn.xfuncname '^(unclosed'",
    );
    each(&script, |p| p.same(&["diff"]));
}

#[test]
fn a_driver_setting_with_no_value_is_a_fatal_config_error() {
    for key in ["funcname", "xfuncname", "textconv", "wordRegex", "command"] {
        let script = format!(
            "printf 'f diff=fn\\n' >.gitattributes; printf 'one\\ntwo\\n' >f; git add -A .; git commit -qm base\n\
             printf 'one\\nTWO\\n' >f; printf '[diff \"fn\"]\\n\\t{key}\\n' >>.git/config\n"
        );
        each(&script, |p| {
            p.same(&["diff"]);
            p.same(&["diff", "--word-diff"]);
        });
    }
}

#[test]
fn textconv_runs_at_the_work_tree_root() {
    let script = "mkdir sub; printf 'f diff=up\\n' >.gitattributes; printf 'ROOT\\n' >marker\n\
        git config diff.up.textconv 'cat marker 2>/dev/null; cat'\n\
        printf 'one\\ntwo\\n' >f; git add -A .; git commit -qm base; printf 'one\\nTWO\\n' >f\n";
    each(script, |p| {
        p.same(&["diff"]);
        p.same_from("sub", &["diff"]);
        p.same_from("sub", &["diff", "HEAD", "--", "../f"]);
    });
}

#[test]
fn a_driver_word_regex_picks_the_words() {
    for (config, args) in [
        (
            "git config diff.w.wordRegex '[a-z]+'",
            &["diff", "--word-diff"][..],
        ),
        (
            "git config diff.w.wordRegex '[a-z]+'",
            &["diff", "--color-words"],
        ),
        (
            "git config diff.w.wordRegex '[a-z]+'",
            &["diff", "--word-diff=porcelain"],
        ),
        (
            "git config diff.w.wordRegex '[a-z]+'; git config diff.wordRegex '[^ ]+'",
            &["diff", "--word-diff"],
        ),
        (
            "git config diff.w.wordRegex '[a-z]+'",
            &["diff", "--word-diff", "--word-diff-regex=[^ ]+"],
        ),
        (
            "git config diff.w.wordRegex '[a-z]+'",
            &["show", "HEAD", "--word-diff"],
        ),
        (
            "git config diff.w.wordRegex '[a-z]+'; git config diff.other.wordRegex '.'",
            &["diff", "--word-diff"],
        ),
    ] {
        let script = format!(
            "printf 'f diff=w\\n' >.gitattributes; {config}\n\
             printf 'foo-bar baz\\nx\\n' >f; git add -A .; git commit -qm base; printf 'foo-qux baz\\nx\\n' >f\n"
        );
        each(&script, |p| p.same(args));
    }
}

const WS_FILES: &str = "printf 'a \\n\\tb \\n        c\\n \\td\\n' >t.plain; for e in ws on off list; do cp t.plain t.$e; done; \
    printf 'a \\r\\n' >t.cr; git add -N t.*\n";

#[test]
fn the_whitespace_attribute_decides_what_diff_check_reports() {
    for attrs in [
        "*.ws whitespace=-trailing-space\\n",
        "*.on whitespace\\n*.off -whitespace\\n",
        "*.list whitespace=indent-with-non-tab,tabwidth=4\\n",
        "*.list whitespace=tab-in-indent,-space-before-tab\\n",
        "*.cr whitespace=cr-at-eol\\n",
        "t.* whitespace=trailing-space\\n*.off -whitespace\\n",
        "*.list whitespace=unknown-rule\\n",
    ] {
        for core in [
            "true",
            "git config core.whitespace -trailing-space,cr-at-eol",
        ] {
            let script = format!(
                "printf '{attrs}' >.gitattributes; {core}\nprintf 'x\\n' >seed; git add seed; git commit -qm base\n{WS_FILES}"
            );
            each(&script, |p| {
                p.same(&["diff", "--check"]);
                p.same(&["diff", "--color=always"]);
            });
        }
    }
}

/// A repository where `f` is committed twice with a textconv that upper-cases, then left clean.
const CLEAN_UPPER: &str = "printf 'f diff=up\\n' >.gitattributes; git config diff.up.textconv 'tr a-z A-Z <'\n\
    printf 'one\\ntwo\\nthree\\n' >f; git add -A .; git commit -qm base\n\
    printf 'one\\ntwo changed\\nthree\\nfour\\n' >f; git commit -qam second\n";

#[test]
fn blame_blames_the_converted_text() {
    each(CLEAN_UPPER, |p| {
        for args in [
            &["blame", "f"][..],
            &["blame", "--no-textconv", "f"],
            &["blame", "--textconv", "f"],
            &["blame", "-L", "2,3", "f"],
            &["blame", "--porcelain", "f"],
            &["blame", "-C", "f"],
        ] {
            p.same(args);
        }
    });
}

#[test]
fn blame_converts_a_work_tree_file_and_follows_a_rename() {
    each(CLEAN_UPPER, |p| {
        for dir in [&p.git, &p.pyn] {
            fs::rename(dir.join("f"), dir.join("g")).unwrap();
            fs::write(dir.join(".gitattributes"), "* diff=up\n").unwrap();
        }
        p.git_in(&p.git, &["add", "-A"]);
        p.git_in(&p.pyn, &["add", "-A"]);
        for dir in [&p.git, &p.pyn] {
            let out = p.git_in(dir, &["commit", "-qm", "rename"]);
            assert!(out.status.success());
        }
        p.same(&["blame", "g"]);
        p.same(&["blame", "--no-textconv", "g"]);
    });
}

#[test]
fn grep_converts_only_when_asked() {
    each(CLEAN_UPPER, |p| {
        for args in [
            &["grep", "-n", "TWO"][..],
            &["grep", "-n", "--textconv", "TWO"],
            &["grep", "-n", "--textconv", "--no-textconv", "TWO"],
            &["grep", "--textconv", "-c", "O"],
            &["grep", "--textconv", "-l", "FOUR", "HEAD", "HEAD~1"],
            &["grep", "--textconv", "--cached", "-n", "ONE"],
            &["grep", "--textconv", "-n", "-i", "four", "HEAD"],
        ] {
            p.same(args);
        }
    });
}

#[test]
fn a_file_replaced_by_a_link_is_split_and_only_the_file_converts() {
    let script = format!("{CLEAN_UPPER}rm f; ln -s elsewhere f\n");
    each(&script, |p| {
        for args in [
            &["diff"][..],
            &["diff", "--stat"],
            &["diff", "--no-textconv"],
            &["diff", "-R"],
            &["diff", "--raw"],
        ] {
            p.same(args);
        }
        p.git_in(&p.git, &["add", "-A"]);
        p.git_in(&p.pyn, &["add", "-A"]);
        p.same(&["diff", "--cached"]);
        p.same(&["diff", "HEAD"]);
    });
}

const SECTIONS: &str = "printf 'sec one\\n a\\n b\\n c\\n d\\n e\\n f\\n g\\n h\\nsec two\\n i\\n j\\n k\\n l\\n m\\n n\\n o\\n p\\nsec three\\n x\\n' >f.x\n\
    git add -A .; git commit -qm base\n\
    printf 'sec one\\n a\\n b\\n c\\n d\\n e\\n f\\n g\\n h\\nsec two\\n i\\n j\\n k\\n l\\n m\\n n\\n O\\n p\\nsec three\\n x\\n' >f.x\n";

#[test]
fn function_context_finds_the_function_start_with_the_driver_pattern() {
    for config in [
        "git config diff.fn.xfuncname '^(sec .*)$'",
        "git config diff.fn.funcname '^sec'",
        "git config diff.fn.xfuncname '^sec t.*'",
        "git config diff.fn.xfuncname '!^sec one\n^sec .*'",
    ] {
        let script = format!("printf '*.x diff=fn\\n' >.gitattributes; {config}\n{SECTIONS}");
        each(&script, |p| {
            for args in [
                &["diff", "-W"][..],
                &["diff", "-W", "-U0"],
                &["diff", "-W", "-U1"],
                &["diff", "--function-context", "--inter-hunk-context=9"],
                &["diff", "-W", "--no-textconv", "-R"],
            ] {
                p.same(args);
            }
        });
    }
    let default = format!("printf '*.x diff=nonesuch\\n' >.gitattributes\n{SECTIONS}");
    each(&default, |p| p.same(&["diff", "-W"]));
}

#[test]
fn no_index_check_reads_the_whitespace_attribute_of_the_global_file() {
    let script = "printf '[core]\\n\\tattributesFile = %s/ga\\n' \"$HOME\" >\"$HOME/.gitconfig\"\n\
        printf '*.t whitespace=-trailing-space\\n*.u whitespace=blank-at-eof\\n' >\"$HOME/ga\"\n\
        printf 'a\\n' >o.t; printf 'a\\nb \\n' >n.t; printf 'a\\n' >o.u; printf 'a\\nb \\n\\n\\n' >n.u\n\
        printf 'a\\n' >o.v; printf 'a\\nb \\n' >n.v; git add -A .; git commit -qm base\n";
    each(script, |p| {
        for (old, new) in [("o.t", "n.t"), ("o.u", "n.u"), ("o.v", "n.v")] {
            p.same(&["diff", "--no-index", "--check", old, new]);
        }
    });
}

/// A shell script that prints what an external diff program gets, with the ids and the temporary names masked.
fn external_program(dir: &Path, name: &str, body: &str) -> String {
    let file = dir.join(name);
    fs::write(&file, body).unwrap();
    format!("sh {}", file.display())
}

const SHOW_ARGS: &str = r#"echo "args=$# path=$1"
mask() { case "$1" in .) echo . ;; *[!0-9a-f]*) echo "$1" ;; *) echo ID ;; esac; }
if [ "$2" = /dev/null ]; then echo "old=null"; else echo "old=$(basename "$2") $(mask "$3") $4"; fi
if [ "$5" = /dev/null ]; then echo "new=null"; else echo "new=$(basename "$5") $(mask "$6") $7"; fi
if [ $# -gt 7 ]; then echo "to=$8"; printf '%s' "$9"; fi
echo "old-content: $(cat "$2" | tr '\n' '|')"
echo "new-content: $(cat "$5" | tr '\n' '|')"
echo "counter=$GIT_DIFF_PATH_COUNTER/$GIT_DIFF_PATH_TOTAL"
"#;

const EXTERNAL_HISTORY: &str = "printf '*.x diff=ex\\n' >.gitattributes\n\
    mkdir d; printf 'a\\nb\\n' >f.x; printf 'plain\\n' >p.txt; printf 'q\\n' >d/h.x; git add -A .; git commit -qm base\n\
    printf 'a\\nc\\n' >f.x; printf 'plain2\\n' >p.txt; printf 'new\\n' >g.x; git add -A .; git commit -qm second\n\
    git mv d/h.x d/i.x; printf 'q\\nr\\n' >d/i.x; printf 'a\\nd\\n' >f.x; printf 'new\\n' >n.x; git add -N n.x\n";

#[test]
fn an_external_diff_program_gets_each_side_of_a_changed_path() {
    let tmp = tempfile::tempdir().unwrap();
    let command = external_program(tmp.path(), "show.sh", SHOW_ARGS);
    let script = format!("git config diff.ex.command '{command}'\n{EXTERNAL_HISTORY}");
    each(&script, |p| {
        for args in [
            &["diff"][..],
            &["diff", "--cached"],
            &["diff", "HEAD"],
            &["diff", "-R"],
            &["diff", "HEAD~1", "HEAD"],
            &["diff", "-M", "HEAD"],
            &["diff", "-C", "HEAD"],
            &["diff", "--stat", "-p"],
            &["diff", "--no-ext-diff"],
            &["diff", "--ext-diff"],
            &["diff", "--exit-code"],
            &["diff", "--quiet"],
            &["diff", "--name-only"],
            &["diff", "--", "f.x"],
            &["log", "-p", "-2"],
            &["log", "-p", "-2", "--ext-diff"],
            &["show", "HEAD"],
            &["show", "--ext-diff", "HEAD"],
        ] {
            p.same(args);
        }
    });
}

#[test]
fn an_external_diff_program_sees_a_binary_file_and_a_type_change() {
    let tmp = tempfile::tempdir().unwrap();
    let command = external_program(tmp.path(), "show.sh", SHOW_ARGS);
    let script = format!(
        "git config diff.ex.command '{command}'; printf '*.x diff=ex\\n' >.gitattributes\n\
         printf 'a\\0b\\n' >bin.x; printf 'text\\n' >t.x; git add -A .; git commit -qm base\n\
         printf 'a\\0c\\n' >bin.x; rm t.x; ln -s target t.x\n"
    );
    each(&script, |p| {
        p.same(&["diff"]);
        p.same(&["diff", "-R"]);
    });
}

#[test]
fn a_failing_external_diff_program_stops_the_diff() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("fail.sh", "echo partial\nexit 3\n"),
        ("quiet.sh", "exit 0\n"),
    ] {
        let command = external_program(tmp.path(), name, body);
        let script = format!("git config diff.ex.command '{command}'\n{EXTERNAL_HISTORY}");
        each(&script, |p| {
            p.same(&["diff"]);
            p.same(&["diff", "--", "p.txt", "g.x"]);
        });
    }
}
