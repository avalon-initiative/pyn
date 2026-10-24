//! The commit graph `pyn log --graph` draws in front of each line, row by row as git draws it (without colors).

use std::collections::HashMap;

use pyn_object::ObjectId;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Padding,
    Skip,
    PreCommit,
    Commit,
    PostMerge,
    Collapsing,
}

pub(super) struct Graph {
    commit: Option<ObjectId>,
    parents: Vec<ObjectId>,
    width: usize,
    expansion_row: usize,
    state: State,
    prev_state: State,
    commit_index: usize,
    prev_commit_index: usize,
    merge_layout: i32,
    edges_added: i32,
    prev_edges_added: i32,
    columns: Vec<ObjectId>,
    new_columns: Vec<ObjectId>,
    mapping: Vec<i32>,
    old_mapping: Vec<i32>,
    mapping_size: usize,
}

fn at(map: &[i32], i: usize) -> i32 {
    map.get(i).copied().unwrap_or(-1)
}

impl Graph {
    pub fn new() -> Self {
        Self {
            commit: None,
            parents: Vec::new(),
            width: 0,
            expansion_row: 0,
            state: State::Padding,
            prev_state: State::Padding,
            commit_index: 0,
            prev_commit_index: 0,
            merge_layout: 0,
            edges_added: 0,
            prev_edges_added: 0,
            columns: Vec::new(),
            new_columns: Vec::new(),
            mapping: Vec::new(),
            old_mapping: Vec::new(),
            mapping_size: 0,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.state == State::Padding
    }

    /// Moves to `commit`, whose shown parents are `parents` in order.
    pub fn update(&mut self, commit: ObjectId, parents: Vec<ObjectId>) {
        self.commit = Some(commit);
        self.parents = parents;
        self.prev_commit_index = self.commit_index;
        self.update_columns();
        self.expansion_row = 0;
        self.state = if self.state != State::Padding {
            State::Skip
        } else if self.needs_pre_commit_line() {
            State::PreCommit
        } else {
            State::Commit
        };
    }

    fn set_state(&mut self, s: State) {
        self.prev_state = self.state;
        self.state = s;
    }

    fn dashed_parents(&self) -> i64 {
        self.parents.len() as i64 + i64::from(self.merge_layout) - 3
    }

    fn needs_pre_commit_line(&self) -> bool {
        self.parents.len() >= 3
            && self.commit_index + 1 < self.columns.len()
            && (self.expansion_row as i64) < self.dashed_parents() * 2
    }

    fn find_new_column(&self, commit: &ObjectId) -> Option<usize> {
        self.new_columns.iter().position(|c| c == commit)
    }

    fn insert_into_new_columns(&mut self, commit: ObjectId, idx: Option<usize>) {
        let i = self.find_new_column(&commit).unwrap_or_else(|| {
            self.new_columns.push(commit);
            self.new_columns.len() - 1
        });
        let mapping_idx;
        if self.parents.len() > 1 && idx.is_some() && self.merge_layout == -1 {
            let dist = idx.map_or(0, |x| x as i64 - i as i64);
            let shift = if dist > 1 { 2 * dist - 3 } else { 1 };
            self.merge_layout = i32::from(dist <= 0);
            self.edges_added = self.parents.len() as i32 + self.merge_layout - 2;
            mapping_idx = self.width as i64 + (i64::from(self.merge_layout) - 1) * shift;
            self.width += 2 * self.merge_layout as usize;
        } else if self.edges_added > 0
            && self.width >= 2
            && at(&self.mapping, self.width - 2) == i as i32
        {
            mapping_idx = self.width as i64 - 2;
            self.edges_added = -1;
        } else {
            mapping_idx = self.width as i64;
            self.width += 2;
        }
        self.mapping[mapping_idx as usize] = i as i32;
    }

    fn update_columns(&mut self) {
        let commit = self.commit.expect("update sets the commit");
        std::mem::swap(&mut self.columns, &mut self.new_columns);
        self.new_columns.clear();
        let max_new = self.columns.len() + self.parents.len();
        self.mapping_size = 2 * max_new;
        self.mapping = vec![-1; self.mapping_size + 4];
        self.width = 0;
        self.prev_edges_added = self.edges_added;
        self.edges_added = 0;
        let mut seen_this = false;
        for i in 0..=self.columns.len() {
            let col_commit = if i == self.columns.len() {
                if seen_this {
                    break;
                }
                commit
            } else {
                self.columns[i]
            };
            if col_commit == commit {
                seen_this = true;
                self.commit_index = i;
                self.merge_layout = -1;
                for p in self.parents.clone() {
                    self.insert_into_new_columns(p, Some(i));
                }
                if self.parents.is_empty() {
                    self.width += 2;
                }
            } else {
                self.insert_into_new_columns(col_commit, None);
            }
        }
        while self.mapping_size > 1 && at(&self.mapping, self.mapping_size - 1) < 0 {
            self.mapping_size -= 1;
        }
    }

    fn pad(&self, sb: &mut String) {
        while sb.len() < self.width {
            sb.push(' ');
        }
    }

    fn mapping_correct(&self) -> bool {
        (0..self.mapping_size).all(|i| {
            let t = at(&self.mapping, i);
            t < 0 || t as usize == i / 2
        })
    }

    /// The next row of the graph, and whether it is the row of the commit itself.
    fn next_line(&mut self) -> (String, bool) {
        let mut sb = String::new();
        let mut is_commit = false;
        match self.state {
            State::Padding => self.padding_row(&mut sb),
            State::Skip => self.skip_row(&mut sb),
            State::PreCommit => self.pre_commit_row(&mut sb),
            State::Commit => {
                self.commit_row(&mut sb);
                is_commit = true;
            }
            State::PostMerge => self.post_merge_row(&mut sb),
            State::Collapsing => self.collapsing_row(&mut sb),
        }
        (sb, is_commit)
    }

    fn padding_row(&self, sb: &mut String) {
        for _ in 0..self.new_columns.len() {
            sb.push_str("| ");
        }
        self.pad(sb);
    }

    fn skip_row(&mut self, sb: &mut String) {
        sb.push_str("...");
        self.pad(sb);
        if self.needs_pre_commit_line() {
            self.set_state(State::PreCommit);
        } else {
            self.set_state(State::Commit);
        }
    }

    fn pre_commit_row(&mut self, sb: &mut String) {
        let commit = self.commit.expect("a commit is current");
        let mut seen_this = false;
        for (i, col) in self.columns.iter().enumerate() {
            if *col == commit {
                seen_this = true;
                sb.push('|');
                sb.push_str(&" ".repeat(self.expansion_row));
            } else if seen_this && self.expansion_row == 0 {
                let slash = self.prev_state == State::PostMerge && self.prev_commit_index < i;
                sb.push(if slash { '\\' } else { '|' });
            } else if seen_this && self.expansion_row > 0 {
                sb.push('\\');
            } else {
                sb.push('|');
            }
            sb.push(' ');
        }
        self.pad(sb);
        self.expansion_row += 1;
        if !self.needs_pre_commit_line() {
            self.set_state(State::Commit);
        }
    }

    fn octopus(&self, sb: &mut String) {
        let dashed = self.dashed_parents();
        for i in 0..dashed {
            sb.push('-');
            sb.push(if i == dashed - 1 { '.' } else { '-' });
        }
    }

    fn commit_row(&mut self, sb: &mut String) {
        let commit = self.commit.expect("a commit is current");
        let mut seen_this = false;
        for i in 0..=self.columns.len() {
            let col_commit = if i == self.columns.len() {
                if seen_this {
                    break;
                }
                commit
            } else {
                self.columns[i]
            };
            if col_commit == commit {
                seen_this = true;
                sb.push('*');
                if self.parents.len() > 2 {
                    self.octopus(sb);
                }
            } else if seen_this && self.edges_added > 1 {
                sb.push('\\');
            } else if seen_this && self.edges_added == 1 {
                let slash = self.prev_state == State::PostMerge
                    && self.prev_edges_added > 0
                    && self.prev_commit_index < i;
                sb.push(if slash { '\\' } else { '|' });
            } else if self.prev_state == State::Collapsing
                && at(&self.old_mapping, 2 * i + 1) == i as i32
                && at(&self.mapping, 2 * i) < i as i32
            {
                sb.push('/');
            } else {
                sb.push('|');
            }
            sb.push(' ');
        }
        self.pad(sb);
        if self.parents.len() > 1 {
            self.set_state(State::PostMerge);
        } else if self.mapping_correct() {
            self.set_state(State::Padding);
        } else {
            self.set_state(State::Collapsing);
        }
    }

    fn post_merge_row(&mut self, sb: &mut String) {
        const MERGE_CHARS: [char; 3] = ['/', '|', '\\'];
        let commit = self.commit.expect("a commit is current");
        let mut seen_this = false;
        let mut parent_col = false;
        for i in 0..=self.columns.len() {
            let col_commit = if i == self.columns.len() {
                if seen_this {
                    break;
                }
                commit
            } else {
                self.columns[i]
            };
            if col_commit == commit {
                seen_this = true;
                let mut idx = self.merge_layout as usize;
                let last = self.parents.len() - 1;
                for (n, _) in self.parents.iter().enumerate() {
                    sb.push(MERGE_CHARS[idx]);
                    if idx == 2 {
                        if self.edges_added > 0 || n != last {
                            sb.push(' ');
                        }
                    } else {
                        idx += 1;
                    }
                }
                if self.edges_added == 0 {
                    sb.push(' ');
                }
            } else if seen_this {
                sb.push(if self.edges_added > 0 { '\\' } else { '|' });
                sb.push(' ');
            } else {
                sb.push('|');
                if self.merge_layout != 0 || i + 1 != self.commit_index {
                    sb.push(if parent_col { '_' } else { ' ' });
                }
            }
            if self.parents.first() == Some(&col_commit) {
                parent_col = true;
            }
        }
        self.pad(sb);
        if self.mapping_correct() {
            self.set_state(State::Padding);
        } else {
            self.set_state(State::Collapsing);
        }
    }

    fn collapsing_row(&mut self, sb: &mut String) {
        std::mem::swap(&mut self.mapping, &mut self.old_mapping);
        let size = self.mapping_size;
        self.mapping = vec![-1; size + 4];
        let (mut used_horizontal, mut horizontal_edge, mut horizontal_target) =
            (false, -1i64, -1i64);
        for i in 0..size {
            let target = at(&self.old_mapping, i);
            if target < 0 {
                continue;
            }
            let t = target as usize;
            if t * 2 == i {
                self.mapping[i] = target;
            } else if at(&self.mapping, i - 1) < 0 {
                self.mapping[i - 1] = target;
                if horizontal_edge == -1 {
                    horizontal_edge = i as i64;
                    horizontal_target = i64::from(target);
                    let mut j = t * 2 + 3;
                    while j + 2 < i {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            } else if at(&self.mapping, i - 1) == target {
            } else {
                self.mapping[i - 2] = target;
                if horizontal_edge == -1 {
                    horizontal_target = i64::from(target);
                    horizontal_edge = i as i64 - 1;
                    let mut j = t * 2 + 3;
                    while j + 2 < i {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            }
        }
        self.old_mapping = self.mapping.clone();
        if size > 0 && at(&self.mapping, size - 1) < 0 {
            self.mapping_size -= 1;
        }
        for i in 0..self.mapping_size {
            let target = at(&self.mapping, i);
            if target < 0 {
                sb.push(' ');
            } else if target as usize * 2 == i {
                sb.push('|');
            } else if i64::from(target) == horizontal_target && i as i64 != horizontal_edge - 1 {
                if i != target as usize * 2 + 3 {
                    self.mapping[i] = -1;
                }
                used_horizontal = true;
                sb.push('_');
            } else {
                if used_horizontal && (i as i64) < horizontal_edge {
                    self.mapping[i] = -1;
                }
                sb.push('/');
            }
        }
        self.pad(sb);
        if self.mapping_correct() {
            self.set_state(State::Padding);
        }
    }

    /// A row that leaves every branch line as it is.
    fn padding_line(&mut self) -> String {
        if self.state != State::Commit {
            return self.next_line().0;
        }
        let commit = self.commit.expect("a commit is current");
        let mut sb = String::new();
        for col in &self.columns {
            sb.push('|');
            if *col == commit && self.parents.len() > 2 {
                sb.push_str(&" ".repeat((self.parents.len() - 2) * 2));
            } else {
                sb.push(' ');
            }
        }
        self.pad(&mut sb);
        self.prev_state = State::Padding;
        sb
    }

    /// Rows up to and including the commit's own, each but the last ended by a newline.
    pub fn show_commit(&mut self, out: &mut String) {
        if self.is_finished() {
            let row = self.padding_line();
            out.push_str(&row);
            return;
        }
        let mut shown = false;
        while !shown && !self.is_finished() {
            let (row, is_commit) = self.next_line();
            out.push_str(&row);
            shown = is_commit;
            if !shown {
                out.push('\n');
            }
        }
    }

    /// The next row, without a newline.
    pub fn show_oneline(&mut self, out: &mut String) {
        let (row, _) = self.next_line();
        out.push_str(&row);
    }

    /// The rows still to come, separated by newlines.
    pub fn show_remainder(&mut self, out: &mut String) {
        while !self.is_finished() {
            let (row, _) = self.next_line();
            out.push_str(&row);
            if self.is_finished() {
                break;
            }
            out.push('\n');
        }
    }

    /// A row with only the graph, without a newline.
    pub fn show_padding(&mut self, out: &mut String) {
        let row = self.padding_line();
        out.push_str(&row);
    }
}

/// Git's topological order (`--topo-order` as `--graph` implies it) of commits given in date order.
pub(super) fn topo_order(
    list: &[ObjectId],
    parents: &HashMap<ObjectId, Vec<ObjectId>>,
) -> Vec<ObjectId> {
    let mut indegree: HashMap<ObjectId, u32> = list.iter().map(|id| (*id, 1)).collect();
    for id in list {
        for p in &parents[id] {
            if let Some(n) = indegree.get_mut(p).filter(|n| **n > 0) {
                *n += 1;
            }
        }
    }
    let mut stack: Vec<ObjectId> = list
        .iter()
        .filter(|id| indegree[*id] == 1)
        .copied()
        .collect();
    stack.reverse();
    let mut out = Vec::with_capacity(list.len());
    while let Some(id) = stack.pop() {
        for p in &parents[&id] {
            let Some(n) = indegree.get_mut(p).filter(|n| **n > 0) else {
                continue;
            };
            *n -= 1;
            if *n == 1 {
                stack.push(*p);
            }
        }
        indegree.insert(id, 0);
        out.push(id);
    }
    out
}
