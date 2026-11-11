//! The `funcname` patterns of git's built-in language diff drivers (`diff=cpp` and the rest), as published in git's
//! `userdiff.c`. Patterns are extended expressions, one per line; `\t` stands for a tab.

/// `(patterns, case-insensitive)` of the built-in driver `name`.
pub fn funcname(name: &str) -> Option<(String, bool)> {
    let (patterns, icase) = match name {
        "ada" => (ADA, true),
        "bash" => (BASH, false),
        "bibtex" => (BIBTEX, false),
        "cpp" => (CPP, false),
        "csharp" => (CSHARP, false),
        "css" => (CSS, false),
        "dts" => (DTS, false),
        "elixir" => (ELIXIR, false),
        "fortran" => (FORTRAN, true),
        "fountain" => (FOUNTAIN, true),
        "golang" => (GOLANG, false),
        "html" => (HTML, false),
        "java" => (JAVA, false),
        "kotlin" => (KOTLIN, false),
        "markdown" => (MARKDOWN, false),
        "matlab" => (MATLAB, false),
        "objc" => (OBJC, false),
        "pascal" => (PASCAL, false),
        "perl" => (PERL, false),
        "php" => (PHP, false),
        "python" => (PYTHON, false),
        "ruby" => (RUBY, false),
        "rust" => (RUST, false),
        "scheme" => (SCHEME, false),
        "tex" => (TEX, false),
        _ => return None,
    };
    Some((patterns.replace("\\t", "\t"), icase))
}

/// Built-in drivers git has that are not ported.
pub fn is_unported(name: &str) -> bool {
    name == "ini"
}

const ADA: &str = r"!^(.*[ \t])?(is[ \t]+new|renames|is[ \t]+separate)([ \t].*)?$
!^[ \t]*with[ \t].*$
^[ \t]*((procedure|function)[ \t]+.*)$
^[ \t]*((package|protected|task)[ \t]+.*)$";

const BASH: &str = r"^[ \t]*(([a-zA-Z_][a-zA-Z0-9_]*[ \t]*\([ \t]*\))|(function[ \t]+[a-zA-Z_][a-zA-Z0-9_]*(([ \t]*\([ \t]*\))|([ \t]+)))[ \t]*(\{|\(\(?|\[\[))";

const BIBTEX: &str = r#"(@[a-zA-Z]{1,}[ \t]*\{{0,1}[ \t]*[^ \t"@',\#}{~%]*).*$"#;

const CPP: &str = r"!^[ \t]*[A-Za-z_][A-Za-z_0-9]*:[[:space:]]*($|/[/*])
^((::[[:space:]]*)?[A-Za-z_].*)$";

const CSHARP: &str = r"!^[ \t]*(do|while|for|if|else|instanceof|new|return|switch|case|throw|catch|using)
^[ \t]*(((static|public|internal|private|protected|new|virtual|sealed|override|unsafe|async)[ \t]+)*[][<>@.~_[:alnum:]]+[ \t]+[<>@._[:alnum:]]+[ \t]*\(.*\))[ \t]*$
^[ \t]*(((static|public|internal|private|protected|new|virtual|sealed|override|unsafe)[ \t]+)*[][<>@.~_[:alnum:]]+[ \t]+[@._[:alnum:]]+)[ \t]*$
^[ \t]*(((static|public|internal|private|protected|new|unsafe|sealed|abstract|partial)[ \t]+)*(class|enum|interface|struct|record)[ \t]+.*)$
^[ \t]*(namespace[ \t]+.*)$";

const CSS: &str = r"![:;][[:space:]]*$
^[:[@.#]?[_a-z0-9].*$";

const DTS: &str = r"!;
!=
^[ \t]*((/[ \t]*\{|&?[a-zA-Z_]).*)";

const ELIXIR: &str = r"^[ \t]*((def(macro|module|impl|protocol|p)?|test)[ \t].*)$";

const FORTRAN: &str = r#"!^([C*]|[ \t]*!)
!^[ \t]*MODULE[ \t]+PROCEDURE[ \t]
^[ \t]*((END[ \t]+)?(PROGRAM|MODULE|BLOCK[ \t]+DATA|([^!'" \t]+[ \t]+)*(SUBROUTINE|FUNCTION))[ \t]+[A-Z].*)$"#;

const FOUNTAIN: &str = r"^((\.[^.]|(int|ext|est|int\.?/ext|i/e)[. ]).*)$";

const GOLANG: &str = r"^[ \t]*(func[ \t]*.*(\{[ \t]*)?)
^[ \t]*(type[ \t].*(struct|interface)[ \t]*(\{[ \t]*)?)";

const HTML: &str = r"^[ \t]*(<[Hh][1-6]([ \t].*)?>.*)$";

const JAVA: &str = r"!^[ \t]*(catch|do|for|if|instanceof|new|return|switch|throw|while)
^[ \t]*(([a-z-]+[ \t]+)*(class|enum|interface|record)[ \t]+.*)$
^[ \t]*(([A-Za-z_<>&][][?&<>.,A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\([^;]*)$";

const KOTLIN: &str = r"^[ \t]*(([a-z]+[ \t]+)*(fun|class|interface)[ \t]+.*)$";

const MARKDOWN: &str = r"^ {0,3}#{1,6}[ \t].*";

const MATLAB: &str = r"^[[:space:]]*((classdef|function)[[:space:]].*)$|^(%%%?|##)[[:space:]].*$";

const OBJC: &str = r"!^[ \t]*(do|for|if|else|return|switch|while)
^[ \t]*([-+][ \t]*\([ \t]*[A-Za-z_][A-Za-z_0-9* \t]*\)[ \t]*[A-Za-z_].*)$
^[ \t]*(([A-Za-z_][A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\([^;]*)$
^(@(implementation|interface|protocol)[ \t].*)$";

const PASCAL: &str = r"^(((class[ \t]+)?(procedure|function)|constructor|destructor|interface|implementation|initialization|finalization)[ \t]*.*)$
^(.*=[ \t]*(class|record).*)$";

const PERL: &str = r"^package .*
^sub [[:alnum:]_':]+[ \t]*(\([^)]*\)[ \t]*)?(:[^;#]*)?(\{[ \t]*)?(#.*)?$
^(BEGIN|END|INIT|CHECK|UNITCHECK|AUTOLOAD|DESTROY)[ \t]*(\{[ \t]*)?(#.*)?$
^=head[0-9] .*";

const PHP: &str = r"^[\t ]*(((public|protected|private|static|abstract|final)[\t ]+)*function.*)$
^[\t ]*((((final|abstract)[\t ]+)?class|enum|interface|trait).*)$";

const PYTHON: &str = r"^[ \t]*((class|(async[ \t]+)?def)[ \t].*)$";

const RUBY: &str = r"^[ \t]*((class|module|def)[ \t].*)$";

const RUST: &str = r#"^[\t ]*((pub(\([^\)]+\))?[\t ]+)?((async|const|unsafe|extern([\t ]+"[^"]+"))[\t ]+)?(struct|enum|union|mod|trait|fn|impl|macro_rules!)[< \t]+[^;]*)$"#;

const SCHEME: &str = r"^[\t ]*(\(((define|def(struct|syntax|class|method|rules|record|proto|alias)?)[-*/ \t]|(library|module|struct|class)[*+ \t]).*)$";

const TEX: &str = r"^(\\((sub)*section|chapter|part)\*{0,1}\{.*)$";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_driver::FuncMatcher;

    #[test]
    fn every_pattern_compiles() {
        for name in [
            "ada", "bash", "bibtex", "cpp", "csharp", "css", "dts", "elixir", "fortran",
            "fountain", "golang", "html", "java", "kotlin", "markdown", "matlab", "objc", "pascal",
            "perl", "php", "python", "ruby", "rust", "scheme", "tex",
        ] {
            let (patterns, icase) = funcname(name).unwrap();
            FuncMatcher::parse(&patterns, true, icase).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
