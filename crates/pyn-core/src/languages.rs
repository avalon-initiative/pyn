//! Repository topics and the language breakdown of a snapshot: validation, and detection by file name.

use std::collections::HashMap;

use crate::error::{PynError, Result};

pub const MAX_TOPICS: usize = 20;
pub const MAX_TOPIC_LEN: usize = 50;

/// Lowercases, trims and de-duplicates topics, keeping their order. A topic is 1 to 50 of `a-z`, `0-9` and `-`,
/// starting with a letter or digit; at most `MAX_TOPICS`.
pub fn validate_topics(topics: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in topics {
        let topic = raw.trim().to_ascii_lowercase();
        let ok = (1..=MAX_TOPIC_LEN).contains(&topic.len())
            && topic
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !topic.starts_with('-');
        if !ok {
            return Err(PynError::InvalidRequest(format!(
                "invalid topic {raw:?}: 1 to {MAX_TOPIC_LEN} lowercase letters, digits or '-', starting with a letter or digit"
            )));
        }
        if !out.contains(&topic) {
            out.push(topic);
        }
    }
    if out.len() > MAX_TOPICS {
        return Err(PynError::InvalidRequest(format!(
            "at most {MAX_TOPICS} topics"
        )));
    }
    Ok(out)
}

/// One language's share of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageShare {
    pub name: String,
    pub bytes: u64,
}

const BY_EXTENSION: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("c", "C"),
    ("h", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("cxx", "C++"),
    ("hpp", "C++"),
    ("hh", "C++"),
    ("hxx", "C++"),
    ("inl", "C++"),
    ("cs", "C#"),
    ("go", "Go"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("scala", "Scala"),
    ("swift", "Swift"),
    ("m", "Objective-C"),
    ("mm", "Objective-C++"),
    ("py", "Python"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("pl", "Perl"),
    ("lua", "Lua"),
    ("js", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("jsx", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("mts", "TypeScript"),
    ("vue", "Vue"),
    ("svelte", "Svelte"),
    ("html", "HTML"),
    ("htm", "HTML"),
    ("css", "CSS"),
    ("scss", "SCSS"),
    ("sass", "Sass"),
    ("less", "Less"),
    ("sh", "Shell"),
    ("bash", "Shell"),
    ("zsh", "Shell"),
    ("ps1", "PowerShell"),
    ("bat", "Batchfile"),
    ("cmd", "Batchfile"),
    ("sql", "SQL"),
    ("hs", "Haskell"),
    ("ml", "OCaml"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("clj", "Clojure"),
    ("dart", "Dart"),
    ("zig", "Zig"),
    ("nim", "Nim"),
    ("r", "R"),
    ("jl", "Julia"),
    ("gd", "GDScript"),
    ("hlsl", "HLSL"),
    ("glsl", "GLSL"),
    ("vert", "GLSL"),
    ("frag", "GLSL"),
    ("shader", "ShaderLab"),
    ("usf", "HLSL"),
    ("ush", "HLSL"),
    ("wgsl", "WGSL"),
    ("cmake", "CMake"),
    ("tf", "HCL"),
    ("nix", "Nix"),
    ("proto", "Protocol Buffer"),
];

const BY_NAME: &[(&str, &str)] = &[
    ("Makefile", "Makefile"),
    ("makefile", "Makefile"),
    ("GNUmakefile", "Makefile"),
    ("CMakeLists.txt", "CMake"),
    ("Dockerfile", "Dockerfile"),
];

/// The programming language a path is written in, by its file name; `None` for data, documents and binaries.
pub fn language_of(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    if let Some((_, lang)) = BY_NAME.iter().find(|(n, _)| *n == file) {
        return Some(lang);
    }
    let (_, ext) = file.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    BY_EXTENSION
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, lang)| *lang)
}

/// Whether a path is left out of the breakdown: vendored, generated or pyn's own files.
fn skipped(path: &str) -> bool {
    path.starts_with(".pyn/")
        || path.split('/').any(|part| {
            matches!(
                part,
                "node_modules" | "vendor" | "third_party" | "ThirdParty" | "dist" | "target"
            )
        })
        || path.ends_with(".min.js")
}

/// Bytes per language over `(path, bytes)` pairs, largest first, then by name.
pub fn breakdown<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> Vec<LanguageShare> {
    let mut totals: HashMap<&'static str, u64> = HashMap::new();
    for (path, bytes) in files {
        if skipped(path) {
            continue;
        }
        if let Some(lang) = language_of(path) {
            *totals.entry(lang).or_default() += bytes;
        }
    }
    let mut out: Vec<_> = totals
        .into_iter()
        .filter(|(_, b)| *b > 0)
        .map(|(name, bytes)| LanguageShare {
            name: name.to_string(),
            bytes,
        })
        .collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_lowercased_deduplicated_and_bounded() {
        let got = validate_topics(vec![" Rust ".into(), "game-dev".into(), "rust".into()]).unwrap();
        assert_eq!(got, ["rust", "game-dev"]);
        for bad in ["", "-x", "a b", "c++", &"x".repeat(MAX_TOPIC_LEN + 1)] {
            assert!(validate_topics(vec![bad.into()]).is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..=MAX_TOPICS).map(|n| format!("t{n}")).collect();
        assert!(validate_topics(many).is_err());
    }

    #[test]
    fn languages_come_from_names_and_skip_vendored_files() {
        let files = [
            ("src/main.rs", 300),
            ("src/lib.RS", 100),
            ("web/app.ts", 250),
            ("Makefile", 50),
            ("README.md", 900),
            ("Content/map.umap", 10_000),
            ("web/node_modules/x/index.js", 5_000),
            (".pyn/hooks.sh", 40),
            ("empty.py", 0),
        ];
        assert_eq!(
            breakdown(files),
            [
                LanguageShare { name: "Rust".into(), bytes: 400 },
                LanguageShare { name: "TypeScript".into(), bytes: 250 },
                LanguageShare { name: "Makefile".into(), bytes: 50 },
            ]
        );
    }
}
