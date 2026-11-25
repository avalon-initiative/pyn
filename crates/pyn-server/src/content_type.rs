//! The media type repository content is served with: an allowlist by file name, never an active type.

use axum::http::{HeaderMap, HeaderValue, header};

pub(crate) const TEXT: &str = "text/plain; charset=utf-8";
pub(crate) const BINARY: &str = "application/octet-stream";
/// Keeps a browser that opens repository content directly from running or loading anything with it.
const SANDBOX: &str =
    "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; sandbox";

const MEDIA: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("ico", "image/vnd.microsoft.icon"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/opus"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mov", "video/quicktime"),
    ("pdf", "application/pdf"),
];

/// Served as plain text, markup and scripts included: the browser shows their source.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "text",
    "md",
    "markdown",
    "mdown",
    "rst",
    "adoc",
    "org",
    "tex",
    "bib",
    "log",
    "csv",
    "tsv",
    "json",
    "jsonc",
    "json5",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "env",
    "xml",
    "xsd",
    "xsl",
    "xslt",
    "plist",
    "html",
    "htm",
    "xhtml",
    "svg",
    "css",
    "scss",
    "sass",
    "less",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "mts",
    "cts",
    "tsx",
    "vue",
    "svelte",
    "rs",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hh",
    "hpp",
    "hxx",
    "inl",
    "ipp",
    "m",
    "mm",
    "cs",
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "gradle",
    "go",
    "py",
    "pyi",
    "rb",
    "php",
    "pl",
    "pm",
    "lua",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "sql",
    "graphql",
    "gql",
    "proto",
    "swift",
    "dart",
    "r",
    "jl",
    "ex",
    "exs",
    "erl",
    "hrl",
    "hs",
    "ml",
    "mli",
    "fs",
    "fsx",
    "clj",
    "cljs",
    "elm",
    "zig",
    "nim",
    "v",
    "sv",
    "vhd",
    "asm",
    "s",
    "cmake",
    "mk",
    "mak",
    "make",
    "dockerfile",
    "tf",
    "tfvars",
    "hcl",
    "nix",
    "lock",
    "diff",
    "patch",
    "gitignore",
    "gitattributes",
    "gitmodules",
    "editorconfig",
    "npmrc",
    "nvmrc",
    "hlsl",
    "glsl",
    "vert",
    "frag",
    "comp",
    "usf",
    "ush",
    "shader",
    "cginc",
    "uproject",
    "uplugin",
    "gd",
    "tscn",
    "tres",
    "godot",
    "csproj",
    "sln",
    "props",
    "targets",
    "vcxproj",
];

/// Common file names without an extension that hold text.
const TEXT_NAMES: &[&str] = &[
    "readme",
    "license",
    "licence",
    "copying",
    "notice",
    "authors",
    "contributors",
    "changelog",
    "changes",
    "codeowners",
    "makefile",
    "gnumakefile",
    "dockerfile",
    "containerfile",
    "jenkinsfile",
    "vagrantfile",
    "gemfile",
    "rakefile",
    "procfile",
    "justfile",
];

/// The media type to serve `path` with; anything not on the lists is opaque bytes.
pub(crate) fn for_path(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    match name.rsplit_once('.') {
        Some((_, ext)) => {
            if let Some((_, media)) = MEDIA.iter().find(|(e, _)| *e == ext) {
                media
            } else if TEXT_EXTENSIONS.contains(&ext) {
                TEXT
            } else {
                BINARY
            }
        }
        None if TEXT_NAMES.contains(&name.as_str()) => TEXT,
        None => BINARY,
    }
}

/// Headers every response carrying repository bytes gets, whatever its type.
pub(crate) fn harden(h: &mut HeaderMap) {
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(SANDBOX),
    );
}

/// `attachment` with the file's name, plain where it is safe ASCII and RFC 5987 encoded for the rest.
pub(crate) fn attachment(path: &str) -> HeaderValue {
    let name = path.rsplit('/').next().unwrap_or(path);
    let plain: String = name
        .chars()
        .map(|c| match c {
            ' '..='~' if c != '"' && c != '\\' => c,
            _ => '_',
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("only visible ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_markup_are_plain_text() {
        for p in [
            "README.md",
            "src/main.rs",
            "Source/Game/Actor.CPP",
            "web/index.html",
            "art/logo.svg",
            "data/config.json",
            "page.xhtml",
            "feed.xml",
            "Makefile",
            "docs/LICENSE",
            ".gitignore",
        ] {
            assert_eq!(for_path(p), TEXT, "{p}");
        }
    }

    #[test]
    fn media_gets_its_type() {
        assert_eq!(for_path("Content/T_Rock.PNG"), "image/png");
        assert_eq!(for_path("a/b.jpeg"), "image/jpeg");
        assert_eq!(for_path("sfx/hit.wav"), "audio/wav");
        assert_eq!(for_path("intro.webm"), "video/webm");
        assert_eq!(for_path("docs/spec.pdf"), "application/pdf");
    }

    #[test]
    fn anything_else_is_opaque() {
        for p in [
            "Content/Map.umap",
            "a.uasset",
            "tool.exe",
            "noext",
            "trailing.",
            "x.svgz",
            "x.htm.gz",
        ] {
            assert_eq!(for_path(p), BINARY, "{p}");
        }
    }

    #[test]
    fn no_path_is_ever_served_with_an_active_type() {
        let mut every = TEXT_EXTENSIONS.to_vec();
        every.extend(MEDIA.iter().map(|(e, _)| *e));
        for ext in every {
            let t = for_path(&format!("f.{ext}"));
            assert!(
                t == TEXT
                    || t == BINARY
                    || t == "application/pdf"
                    || ["image/", "audio/", "video/"]
                        .iter()
                        .any(|p| t.starts_with(p)),
                "{ext}: {t}"
            );
            assert!(
                !t.contains("html") && !t.contains("xml") && !t.contains("svg"),
                "{ext}: {t}"
            );
            assert!(!t.contains("javascript"), "{ext}: {t}");
        }
    }

    #[test]
    fn an_attachment_name_cannot_break_the_header() {
        assert_eq!(
            attachment("dir/a b.txt").to_str().unwrap(),
            "attachment; filename=\"a b.txt\"; filename*=UTF-8''a%20b.txt"
        );
        assert_eq!(
            attachment("q\"x\\y.txt").to_str().unwrap(),
            "attachment; filename=\"q_x_y.txt\"; filename*=UTF-8''q%22x%5Cy.txt"
        );
        assert_eq!(
            attachment("é.md").to_str().unwrap(),
            "attachment; filename=\"_.md\"; filename*=UTF-8''%C3%A9.md"
        );
    }
}
