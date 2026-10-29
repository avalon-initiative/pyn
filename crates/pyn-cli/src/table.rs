//! The one table format every listing command prints: header row, aligned columns, last column unpadded.

/// Renders `header` and `rows`; every column but the last is padded to its widest cell.
pub fn render(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let mut out = String::new();
        let last = widths.len() - 1;
        for (i, cell) in cells.enumerate() {
            out.push_str(cell);
            if i < last {
                let pad = widths[i] - cell.chars().count() + 2;
                out.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.trim_end().to_string()
    };
    let mut lines = vec![line(&mut header.iter().copied())];
    lines.extend(rows.iter().map(|r| line(&mut r.iter().map(String::as_str))));
    lines.join("\n")
}

/// Like `render`, with each row's note (a message body) indented four spaces under it.
/// Notes do not affect column widths.
pub fn render_notes(header: &[&str], rows: &[(Vec<String>, String)]) -> String {
    let cells: Vec<Vec<String>> = rows.iter().map(|(c, _)| c.clone()).collect();
    let table = render(header, &cells);
    let mut lines = table.lines();
    let mut out = vec![lines.next().unwrap_or_default().to_string()];
    for ((_, note), line) in rows.iter().zip(lines) {
        out.push(line.to_string());
        for l in note.trim_end().lines() {
            out.push(if l.trim().is_empty() {
                String::new()
            } else {
                format!("    {l}")
            });
        }
    }
    out.join("\n")
}

/// Prints the table with notes, or `empty` alone when there are no rows.
pub fn show_notes(header: &[&str], rows: &[(Vec<String>, String)], empty: &str) {
    if rows.is_empty() {
        println!("{empty}");
    } else {
        println!("{}", render_notes(header, rows));
    }
}

/// Prints the table, or `empty` alone when there are no rows.
pub fn show(header: &[&str], rows: &[Vec<String>], empty: &str) {
    if rows.is_empty() {
        println!("{empty}");
    } else {
        println!("{}", render(header, rows));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn columns_align_to_the_widest_cell() {
        let out = render(
            &["MODE", "REV", "PATH"],
            &[
                row(&["exclusive", "r1", "a"]),
                row(&["shared", "r12", "b/c"]),
            ],
        );
        assert_eq!(
            out,
            "MODE       REV  PATH\nexclusive  r1   a\nshared     r12  b/c"
        );
    }

    #[test]
    fn the_last_column_is_not_padded() {
        let out = render(
            &["A", "LAST"],
            &[row(&["x", "short"]), row(&["yy", "a longer one"])],
        );
        assert!(out.lines().all(|l| l == l.trim_end()), "{out:?}");
        assert!(out.ends_with("yy  a longer one"));
    }

    #[test]
    fn an_empty_last_cell_leaves_no_trailing_space() {
        assert_eq!(render(&["A", "B"], &[row(&["x", ""])]), "A  B\nx");
    }

    #[test]
    fn wide_characters_count_as_one_column() {
        let out = render(&["WHO", "N"], &[row(&["a\u{b7}b", "1"])]);
        assert_eq!(out, "WHO  N\na\u{b7}b  1");
    }

    #[test]
    fn notes_indent_under_their_row_without_widening_columns() {
        let rows = vec![
            (row(&["a", "one"]), "first\n\nsecond\n".to_string()),
            (row(&["bb", "two"]), String::new()),
        ];
        assert_eq!(
            render_notes(&["A", "B"], &rows),
            "A   B\na   one\n    first\n\n    second\nbb  two"
        );
    }

    #[test]
    fn no_rows_renders_the_header_only() {
        assert_eq!(render(&["A", "B"], &[]), "A  B");
    }
}
