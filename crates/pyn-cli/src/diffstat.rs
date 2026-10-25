//! Git's diffstat: the ` name | N ++--` lines scaled to a width, and the totals line.

use std::fmt::Write;

#[derive(Clone, Copy)]
pub enum Counts {
    Lines(usize, usize),
    /// Old and new size in bytes.
    Binary(usize, usize),
}

pub struct Row {
    pub name: String,
    pub counts: Counts,
}

/// Git's default width when neither a terminal nor `COLUMNS` says otherwise.
pub const DEFAULT_WIDTH: usize = 80;

/// The width git uses for a merge's diffstat: `COLUMNS` when set, else 80.
pub fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(DEFAULT_WIDTH)
}

fn digits(n: usize) -> usize {
    n.to_string().len()
}

fn scale(n: usize, width: usize, max: usize) -> usize {
    if n == 0 { 0 } else { 1 + n * (width - 1) / max }
}

/// How a diffstat is laid out: git's `--stat=<width>[,<name-width>[,<count>]]` and `--stat-graph-width`.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub width: usize,
    pub name_width: Option<usize>,
    pub graph_width: Option<usize>,
    pub count: Option<usize>,
    /// The escape sequences for added and deleted counts, when output is colored.
    pub colors: Option<(String, String)>,
}

/// The stat lines and totals for `rows`, fitted to `width` columns (`graph` caps the graph part).
pub fn render(rows: &[Row], width: usize, graph: Option<usize>) -> String {
    render_with(
        rows,
        &Layout {
            width,
            graph_width: graph,
            ..Layout::default()
        },
    )
}

pub fn render_with(all: &[Row], layout: &Layout) -> String {
    let mut out = String::new();
    if all.is_empty() {
        return out;
    }
    let shown = layout
        .count
        .filter(|c| *c > 0)
        .map_or(all.len(), |c| c.min(all.len()));
    let rows = &all[..shown];
    let max_len = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut max_change = 0;
    let mut number_width = 0;
    let mut bin_width = 0;
    for row in rows {
        match row.counts {
            Counts::Lines(a, d) => max_change = max_change.max(a + d),
            Counts::Binary(old, new) => {
                bin_width = bin_width.max(14 + digits(old) + digits(new));
                number_width = 3;
            }
        }
    }
    number_width = number_width.max(digits(max_change));
    let width = layout.width.max(16 + 6 + number_width);
    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    let graph = layout.graph_width.filter(|g| *g > 0);
    if let Some(cap) = graph.filter(|g| *g < graph_width) {
        graph_width = cap;
    }
    let mut name_width = layout
        .name_width
        .filter(|n| *n > 0 && *n < max_len)
        .unwrap_or(max_len);
    if name_width + number_width + 6 + graph_width > width {
        let limit = (width * 3 / 8).saturating_sub(number_width + 6);
        if graph_width > limit {
            graph_width = limit.max(6);
        }
        if let Some(cap) = graph.filter(|g| graph_width > *g) {
            graph_width = cap;
        }
        let room = width.saturating_sub(number_width + 6 + graph_width);
        if name_width > room {
            name_width = room;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    let (add_c, del_c, reset) = match &layout.colors {
        Some((a, d)) => (a.as_str(), d.as_str(), "\x1b[m"),
        None => ("", "", ""),
    };
    let (mut added, mut deleted) = (0, 0);
    for row in rows {
        let name = shorten(&row.name, name_width);
        match row.counts {
            Counts::Lines(a, d) => {
                added += a;
                deleted += d;
                let (mut a, mut d) = (a, d);
                if graph_width <= max_change {
                    let mut total = scale(a + d, graph_width, max_change);
                    if total < 2 && a > 0 && d > 0 {
                        total = 2;
                    }
                    if a < d {
                        a = scale(a, graph_width, max_change);
                        d = total - a;
                    } else {
                        d = scale(d, graph_width, max_change);
                        a = total - d;
                    }
                }
                let n = match row.counts {
                    Counts::Lines(x, y) => x + y,
                    Counts::Binary(..) => 0,
                };
                let bar = |c: &str, ch: &str, n: usize| {
                    if n == 0 {
                        String::new()
                    } else {
                        format!("{c}{}{reset}", ch.repeat(n))
                    }
                };
                let _ = writeln!(
                    out,
                    " {name} | {n:>number_width$}{}{}{}",
                    if n > 0 { " " } else { "" },
                    bar(add_c, "+", a),
                    bar(del_c, "-", d),
                );
            }
            Counts::Binary(old, new) => {
                let sizes = if old + new == 0 {
                    String::new()
                } else {
                    format!(" {del_c}{old}{reset} -> {add_c}{new}{reset} bytes")
                };
                let _ = writeln!(out, " {name} | {:>number_width$}{sizes}", "Bin");
            }
        }
    }
    if shown < all.len() {
        out.push_str(" ...\n");
        for row in &all[shown..] {
            if let Counts::Lines(a, d) = row.counts {
                added += a;
                deleted += d;
            }
        }
    }
    out.push_str(&summary(all.len(), added, deleted));
    out
}

/// The name padded to `width`, with a leading `...` and a cut at a slash when it does not fit.
fn shorten(name: &str, width: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= width {
        return format!("{name:<width$}");
    }
    let keep = width.saturating_sub(3);
    let tail: String = chars[chars.len() - keep..].iter().collect();
    let tail = match tail.find('/') {
        Some(i) => tail[i..].to_string(),
        None => tail,
    };
    format!("...{tail:<keep$}")
}

pub fn summary(files: usize, added: usize, deleted: usize) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut out = format!(" {} changed", plural(files, "file", "files"));
    if added > 0 || deleted == 0 {
        let _ = write!(out, ", {}(+)", plural(added, "insertion", "insertions"));
    }
    if deleted > 0 || added == 0 {
        let _ = write!(out, ", {}(-)", plural(deleted, "deletion", "deletions"));
    }
    out.push('\n');
    out
}

/// `old => new` with the shared leading and trailing directories factored out as git prints a rename: `d/{a => b}/f`.
pub fn rename_name(a: &str, b: &str) -> String {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let mut pfx = 0;
    for (i, (p, q)) in x.iter().zip(y).enumerate() {
        if p != q {
            break;
        }
        if *p == b'/' {
            pfx = i + 1;
        }
    }
    let adjust = usize::from(pfx > 0);
    let floor = (pfx - adjust) as isize;
    let (mut i, mut j) = (x.len() as isize, y.len() as isize);
    let mut sfx = 0;
    while i >= floor && j >= floor {
        let (p, q) = (x.get(i as usize), y.get(j as usize));
        if p != q {
            break;
        }
        if p == Some(&b'/') {
            sfx = x.len() - i as usize;
        }
        i -= 1;
        j -= 1;
    }
    let mid = |s: &str| {
        s.get(pfx..s.len().saturating_sub(sfx).max(pfx))
            .unwrap_or("")
            .to_string()
    };
    if pfx + sfx == 0 {
        return format!("{a} => {b}");
    }
    format!(
        "{}{{{} => {}}}{}",
        &a[..pfx],
        mid(a),
        mid(b),
        &a[a.len() - sfx..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_factors_shared_directories() {
        assert_eq!(rename_name("a", "b"), "a => b");
        assert_eq!(rename_name("d/a", "d/b"), "d/{a => b}");
        assert_eq!(rename_name("x/f", "y/f"), "{x => y}/f");
        assert_eq!(rename_name("d/x/f", "d/y/f"), "d/{x => y}/f");
        assert_eq!(rename_name("f", "d/f"), "f => d/f");
    }

    #[test]
    fn summary_pluralises_and_omits_zero_parts() {
        assert_eq!(summary(1, 1, 0), " 1 file changed, 1 insertion(+)\n");
        assert_eq!(
            summary(2, 3, 1),
            " 2 files changed, 3 insertions(+), 1 deletion(-)\n"
        );
        assert_eq!(
            summary(1, 0, 0),
            " 1 file changed, 0 insertions(+), 0 deletions(-)\n"
        );
    }

    #[test]
    fn graph_scales_to_the_width() {
        let rows = [Row {
            name: "f".into(),
            counts: Counts::Lines(100, 0),
        }];
        let out = render(&rows, 80, None);
        assert!(out.starts_with(" f | 100 "), "{out}");
        assert_eq!(out.lines().next().unwrap().matches('+').count(), 70);
    }
}
