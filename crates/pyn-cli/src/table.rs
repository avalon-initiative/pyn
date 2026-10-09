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

/// A timestamp to the second, in UTC.
pub fn when(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
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
    fn no_rows_renders_the_header_only() {
        assert_eq!(render(&["A", "B"], &[]), "A  B");
    }
}
