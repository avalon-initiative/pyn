#!/usr/bin/env python3
"""Self-test for check_oracle_text.py: each rule fires on a small snippet and clean code passes."""
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from check_oracle_text import check_text  # noqa: E402


def rules(src: str) -> set[str]:
    return {r.split(" ")[0] for _, r in check_text(src)}


class Rules(unittest.TestCase):
    def test_git_stderr_eq(self):
        self.assertEqual(rules("fn t() { assert_eq!(got.stderr, want.stderr); }"), {"git-stderr"})
        self.assertEqual(rules('fn t() { assert_eq!(g.err(), "fatal: x\\n", "{a}"); }'), {"git-stderr"})

    def test_git_stderr_contains(self):
        self.assertEqual(rules('fn t() { assert!(String::from_utf8_lossy(&oracle.stderr).contains("x")); }'), {"git-stderr"})

    def test_git_stderr_multiline(self):
        self.assertEqual(rules("fn t() {\n    assert_eq!(\n        text(&g.stderr),\n        text(&p.stderr),\n    );\n}"), {"git-stderr"})

    def test_git_stdout(self):
        self.assertEqual(rules("fn t() { assert_eq!(got.out(), want.out(), \"{what}\"); }"), {"git-stdout"})

    def test_git_version(self):
        self.assertEqual(rules('let o = Command::new("git").arg("--version").output().unwrap();'), {"git-version"})
        self.assertEqual(rules("let ok = version >= vec![2, 55];"), {"git-version"})
        self.assertEqual(rules("let v = git_version();"), {"git-version"})
        self.assertEqual(rules('let v = std::env::var("GIT_VERSION");'), {"git-version"})

    def test_replace(self):
        self.assertEqual(rules('let t = out.replace("pyn ", "git ");'), {"replace-pyn"})

    def test_clean(self):
        clean = """
// assert_eq!(g.stderr, p.stderr) in a comment, git --version too
fn t() {
    assert_eq!(g.code, p.code, "{}", g.stderr);
    assert_eq!(got.stderr.is_empty(), want.stderr.is_empty());
    assert!(p.stderr.contains("not built"));
    assert_eq!(p.out(), "text");
    let s = "git --version";
    assert_same_behavior("x", Format::Human, Stderr::Ignored, (&a, &b), (&c, &d));
}
"""
        self.assertEqual(check_text(clean), [])

    def test_escape(self):
        ok = "// oracle-text: allowed git prints a fixed line here\nassert_eq!(got.stderr, want.stderr);"
        self.assertEqual(check_text(ok), [])
        bare = "// oracle-text: allowed\nassert_eq!(got.stderr, want.stderr);"
        self.assertEqual(len(check_text(bare)), 1)
        self.assertIn("reason", check_text(bare)[0][1])


if __name__ == "__main__":
    unittest.main()
