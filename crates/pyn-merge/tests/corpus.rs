use pyn_merge::{Chunk, Conflict, Outcome, merge, merge_text, render_markers, render_two_way};

fn clean(base: &str, ours: &str, theirs: &str) -> String {
    match merge_text(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()) {
        Outcome::Clean(b) => String::from_utf8(b).unwrap(),
        other => panic!("expected clean merge, got {other:?}"),
    }
}

fn conflicts(base: &str, ours: &str, theirs: &str) -> Vec<Conflict<String>> {
    match merge_text(base.as_bytes(), ours.as_bytes(), theirs.as_bytes()) {
        Outcome::Conflicted(chunks) => chunks
            .into_iter()
            .filter_map(|c| match c {
                Chunk::Conflict(c) => Some(Conflict {
                    base: join(&c.base),
                    ours: join(&c.ours),
                    theirs: join(&c.theirs),
                }),
                Chunk::Resolved(_) => None,
            })
            .collect(),
        other => panic!("expected conflict, got {other:?}"),
    }
}

fn join(lines: &[&[u8]]) -> Vec<String> {
    lines
        .iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect()
}

fn one(base: &[&str], ours: &[&str], theirs: &[&str]) -> Conflict<String> {
    let to = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    Conflict {
        base: to(base),
        ours: to(ours),
        theirs: to(theirs),
    }
}

#[test]
fn trivial_cases() {
    assert_eq!(clean("", "", ""), "");
    assert_eq!(clean("a\n", "a\n", "a\n"), "a\n");
    assert_eq!(clean("a\n", "b\n", "a\n"), "b\n");
    assert_eq!(clean("a\n", "a\n", "b\n"), "b\n");
    assert_eq!(clean("a\n", "b\n", "b\n"), "b\n");
}

#[test]
fn distant_edits_combine() {
    let base = "1\n2\n3\n4\n5\n6\n7\n";
    assert_eq!(
        clean(base, "1\nTWO\n3\n4\n5\n6\n7\n", "1\n2\n3\n4\n5\nSIX\n7\n"),
        "1\nTWO\n3\n4\n5\nSIX\n7\n"
    );
}

#[test]
fn separated_by_one_line_is_clean() {
    assert_eq!(
        clean("a\nb\nc\nd\n", "A\nb\nc\nd\n", "a\nb\nC\nd\n"),
        "A\nb\nC\nd\n"
    );
}

#[test]
fn adjacent_edits_conflict() {
    let got = conflicts("a\nb\nc\n", "A\nb\nc\n", "a\nB\nc\n");
    assert_eq!(
        got,
        vec![one(&["a\n", "b\n"], &["A\n", "b\n"], &["a\n", "B\n"])]
    );
}

#[test]
fn identical_edits_merge_without_conflict() {
    assert_eq!(clean("a\nb\nc\n", "a\nX\nc\n", "a\nX\nc\n"), "a\nX\nc\n");
    assert_eq!(clean("a\nb\nc\n", "a\nc\n", "a\nc\n"), "a\nc\n");
}

#[test]
fn identical_edit_plus_distinct_edit() {
    assert_eq!(
        clean("a\nb\nc\nd\ne\n", "a\nX\nc\nd\nE\n", "a\nX\nc\nd\ne\n"),
        "a\nX\nc\nd\nE\n"
    );
}

#[test]
fn conflicting_edits_of_one_line() {
    let got = conflicts("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n");
    assert_eq!(got, vec![one(&["b\n"], &["X\n"], &["Y\n"])]);
}

#[test]
fn one_sided_delete_of_untouched_lines_is_clean() {
    assert_eq!(clean("a\nb\nc\nd\n", "a\nd\n", "a\nb\nc\nd\n"), "a\nd\n");
    assert_eq!(
        clean("a\nb\nc\nd\ne\n", "a\nd\ne\n", "a\nb\nc\nd\nE\n"),
        "a\nd\nE\n"
    );
}

#[test]
fn delete_versus_edit_conflicts() {
    let got = conflicts("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
    assert_eq!(got, vec![one(&["b\n"], &[], &["B\n"])]);
}

#[test]
fn delete_versus_edit_inside_larger_delete() {
    let got = conflicts("a\nb\nc\nd\n", "a\nd\n", "a\nb\nC\nd\n");
    assert_eq!(got, vec![one(&["b\n", "c\n"], &[], &["b\n", "C\n"])]);
}

#[test]
fn both_insert_at_same_point_conflicts_unless_equal() {
    let got = conflicts("a\nb\n", "a\nX\nb\n", "a\nY\nb\n");
    assert_eq!(got, vec![one(&[], &["X\n"], &["Y\n"])]);
    assert_eq!(clean("a\nb\n", "a\nX\nb\n", "a\nX\nb\n"), "a\nX\nb\n");
}

#[test]
fn inserts_at_different_points_combine() {
    assert_eq!(
        clean("a\nb\nc\n", "X\na\nb\nc\n", "a\nb\nc\nY\n"),
        "X\na\nb\nc\nY\n"
    );
}

#[test]
fn insert_into_empty_base() {
    assert_eq!(clean("", "a\n", ""), "a\n");
    let got = conflicts("", "a\n", "b\n");
    assert_eq!(got, vec![one(&[], &["a\n"], &["b\n"])]);
}

#[test]
fn both_delete_everything() {
    assert_eq!(clean("a\nb\n", "", ""), "");
    assert_eq!(clean("a\nb\n", "", "a\nb\n"), "");
}

#[test]
fn missing_final_newline_preserved_when_untouched() {
    assert_eq!(clean("a\nb\nc", "A\nb\nc", "a\nb\nc"), "A\nb\nc");
    assert_eq!(
        clean("a\nb\nc\nd", "A\nb\nc\nd", "a\nb\nc\nD"),
        "A\nb\nc\nD"
    );
}

#[test]
fn adding_a_final_newline_on_one_side() {
    assert_eq!(clean("a\nb\nc", "a\nb\nc\n", "A\nb\nc"), "A\nb\nc\n");
    assert_eq!(conflicts("a\nb", "a\nb\n", "A\nb").len(), 1);
}

#[test]
fn appending_after_unterminated_last_line_conflicts_with_its_edit() {
    let got = conflicts("a\nb", "a\nb\nc\n", "a\nB");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].base, vec!["b".to_string()]);
}

#[test]
fn both_remove_final_newline() {
    assert_eq!(clean("a\nb\n", "a\nb", "a\nb"), "a\nb");
}

#[test]
fn crlf_lines_are_compared_whole() {
    assert_eq!(
        clean(
            "a\r\nb\r\nc\r\nd\r\n",
            "A\r\nb\r\nc\r\nd\r\n",
            "a\r\nb\r\nc\r\nD\r\n"
        ),
        "A\r\nb\r\nc\r\nD\r\n"
    );
}

#[test]
fn several_conflicts_with_clean_text_between() {
    let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let ours = "1\nA\n3\n4\n5\n6\nB\n8\n9\n";
    let theirs = "1\nX\n3\n4\n5\n6\nY\n8\n9\n";
    let got = conflicts(base, ours, theirs);
    assert_eq!(
        got,
        vec![
            one(&["2\n"], &["A\n"], &["X\n"]),
            one(&["7\n"], &["B\n"], &["Y\n"])
        ]
    );
}

#[test]
fn chunks_keep_resolved_runs_between_conflicts() {
    let out = merge_text(b"1\n2\n3\n", b"1\nA\n3\n", b"1\nB\n3\n");
    let Outcome::Conflicted(chunks) = out else {
        panic!("expected conflict")
    };
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0], Chunk::Resolved(vec![&b"1\n"[..]]));
    assert_eq!(chunks[2], Chunk::Resolved(vec![&b"3\n"[..]]));
}

#[test]
fn binaries_conflict_as_a_whole_file() {
    let base = b"\0base";
    assert_eq!(
        merge_text(base, b"\0ours", b"\0theirs"),
        Outcome::BinaryConflict
    );
}

#[test]
fn binaries_with_one_side_unchanged_take_the_other() {
    let base = b"\0base";
    assert_eq!(
        merge_text(base, b"\0ours", base),
        Outcome::Clean(b"\0ours".to_vec())
    );
    assert_eq!(
        merge_text(base, base, b"\0theirs"),
        Outcome::Clean(b"\0theirs".to_vec())
    );
    assert_eq!(
        merge_text(base, b"\0same", b"\0same"),
        Outcome::Clean(b"\0same".to_vec())
    );
}

#[test]
fn a_side_turning_binary_conflicts() {
    assert_eq!(
        merge_text(b"text\n", b"te\0xt\n", b"other\n"),
        Outcome::BinaryConflict
    );
}

#[test]
fn markers_render_diff3_style() {
    let out = merge_text(b"a\nb\nc\n", b"a\nX\nc\n", b"a\nY\nc\n");
    let Outcome::Conflicted(chunks) = out else {
        panic!("expected conflict")
    };
    let got = render_markers(&chunks, "ours", "base", "theirs");
    assert_eq!(
        String::from_utf8(got).unwrap(),
        "a\n<<<<<<< ours\nX\n||||||| base\nb\n=======\nY\n>>>>>>> theirs\nc\n"
    );
}

#[test]
fn two_way_markers_leave_out_the_base() {
    let out = merge_text(b"a\nb\nc", b"a\nX\nc", b"a\nY\nc");
    let Outcome::Conflicted(chunks) = out else {
        panic!("expected conflict")
    };
    assert_eq!(
        String::from_utf8(render_two_way(&chunks, "ours", "theirs")).unwrap(),
        "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\nc"
    );
}

#[test]
fn markers_terminate_an_unterminated_side() {
    let out = merge_text(b"a\nb", b"a\nX", b"a\nY");
    let Outcome::Conflicted(chunks) = out else {
        panic!("expected conflict")
    };
    let got = String::from_utf8(render_markers(&chunks, "o", "b", "t")).unwrap();
    assert_eq!(
        got,
        "a\n<<<<<<< o\nX\n||||||| b\nb\n=======\nY\n>>>>>>> t\n"
    );
}

#[test]
fn generic_merge_works_on_any_equatable_items() {
    let got = merge(&[1, 2, 3, 4, 5], &[1, 20, 3, 4, 5], &[1, 2, 3, 4, 50]);
    assert_eq!(got, vec![Chunk::Resolved(vec![1, 20, 3, 4, 50])]);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self, m: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % m
    }
}

fn resolve(chunks: &[Chunk<u32>], pick: fn(&Conflict<u32>) -> &Vec<u32>) -> Vec<u32> {
    let mut out = Vec::new();
    for c in chunks {
        match c {
            Chunk::Resolved(v) => out.extend(v),
            Chunk::Conflict(c) => out.extend(pick(c)),
        }
    }
    out
}

fn mutate(rng: &mut Rng, base: &[u32], next_id: &mut u32) -> Vec<u32> {
    let mut v = base.to_vec();
    for _ in 0..rng.next(4) {
        let at = rng.next(v.len() as u64 + 1) as usize;
        match rng.next(3) {
            0 => {
                v.insert(at, *next_id);
                *next_id += 1;
            }
            1 if at < v.len() => {
                v.remove(at);
            }
            _ if at < v.len() => {
                v[at] = *next_id;
                *next_id += 1;
            }
            _ => {}
        }
    }
    v
}

#[test]
fn random_merges_satisfy_the_algebraic_laws() {
    let mut rng = Rng(7);
    let mut id = 1000;
    for _ in 0..2000 {
        let len = rng.next(25) as usize;
        let base: Vec<u32> = (0..len as u32).collect();
        let ours = mutate(&mut rng, &base, &mut id);
        let theirs = mutate(&mut rng, &base, &mut id);

        let unchanged = |x: &[u32]| vec![Chunk::Resolved(x.to_vec())];
        let norm = |c: Vec<Chunk<u32>>| {
            if c == vec![Chunk::Resolved(vec![])] {
                vec![]
            } else {
                c
            }
        };
        assert_eq!(norm(merge(&base, &ours, &base)), norm(unchanged(&ours)));
        assert_eq!(norm(merge(&base, &base, &theirs)), norm(unchanged(&theirs)));
        assert_eq!(norm(merge(&base, &ours, &ours)), norm(unchanged(&ours)));

        // Swapping the sides swaps each conflict's sides and nothing else.
        let ab = merge(&base, &ours, &theirs);
        let ba = merge(&base, &theirs, &ours);
        assert_eq!(ab.len(), ba.len());
        for (x, y) in ab.iter().zip(&ba) {
            match (x, y) {
                (Chunk::Resolved(p), Chunk::Resolved(q)) => assert_eq!(p, q),
                (Chunk::Conflict(p), Chunk::Conflict(q)) => {
                    assert_eq!((&p.base, &p.ours, &p.theirs), (&q.base, &q.theirs, &q.ours));
                }
                _ => panic!("side swap changed the chunk shape"),
            }
        }
    }
}

#[test]
fn random_conflicts_carry_real_base_and_side_text() {
    let mut rng = Rng(99);
    let mut id = 1000;
    for _ in 0..2000 {
        let len = rng.next(25) as usize;
        let base: Vec<u32> = (0..len as u32).collect();
        let ours = mutate(&mut rng, &base, &mut id);
        let theirs = mutate(&mut rng, &base, &mut id);
        let chunks = merge(&base, &ours, &theirs);

        // Resolving every conflict to the base text and every resolved run unchanged must not
        // invent lines: all output lines come from one of the three inputs.
        for out in [
            resolve(&chunks, |c| &c.ours),
            resolve(&chunks, |c| &c.theirs),
            resolve(&chunks, |c| &c.base),
        ] {
            for l in out {
                assert!(base.contains(&l) || ours.contains(&l) || theirs.contains(&l));
            }
        }
        for c in &chunks {
            if let Chunk::Conflict(c) = c {
                assert_ne!(c.ours, c.theirs);
            }
        }
        // A clean merge keeps every base line neither side touched.
        if chunks.iter().all(|c| matches!(c, Chunk::Resolved(_))) {
            let merged = resolve(&chunks, |c| &c.ours);
            for l in &base {
                if ours.contains(l) && theirs.contains(l) {
                    assert!(merged.contains(l));
                }
            }
        }
    }
}

#[test]
fn random_spaced_edits_match_the_model() {
    let mut rng = Rng(2024);
    for _ in 0..1000 {
        let len = 20 + rng.next(20) as usize;
        let base: Vec<u32> = (0..len as u32).collect();
        let (mut ours, mut theirs, mut expect) = (base.clone(), base.clone(), base.clone());
        // Edit lines at least two apart so no two changes touch, then each side edits its own set.
        let mut i = 1;
        while i + 1 < len {
            match rng.next(4) {
                0 => {
                    ours[i] = 10_000 + i as u32;
                    expect[i] = ours[i];
                }
                1 => {
                    theirs[i] = 20_000 + i as u32;
                    expect[i] = theirs[i];
                }
                2 => {
                    let v = 30_000 + i as u32;
                    (ours[i], theirs[i], expect[i]) = (v, v, v);
                }
                _ => {}
            }
            i += 2;
        }
        let chunks = merge(&base, &ours, &theirs);
        assert!(chunks.iter().all(|c| matches!(c, Chunk::Resolved(_))));
        assert_eq!(resolve(&chunks, |c| &c.ours), expect);
    }
}

#[test]
fn random_text_merges_never_panic_and_agree_on_trivial_sides() {
    let mut rng = Rng(5);
    let words = ["a\n", "b\n", "c\n", "d", "\n", "e\r\n"];
    let gen_text = |rng: &mut Rng| {
        (0..rng.next(12))
            .map(|_| words[rng.next(6) as usize])
            .collect::<String>()
    };
    for _ in 0..2000 {
        let (base, ours, theirs) = (gen_text(&mut rng), gen_text(&mut rng), gen_text(&mut rng));
        let _ = merge_text(base.as_bytes(), ours.as_bytes(), theirs.as_bytes());
        assert_eq!(clean(&base, &ours, &base), ours);
        assert_eq!(clean(&base, &base, &theirs), theirs);
        assert_eq!(clean(&base, &ours, &ours), ours);
    }
}
