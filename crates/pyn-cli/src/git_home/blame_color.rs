//! `--color-lines`, `--color-by-age`, `blame.coloring` and the `color.blame.*` settings.

use anyhow::{Result, bail};
use chrono::Local;

use super::GitHome;
use super::log_match::parse_date;
use crate::diff_color::parse_color;

const DEFAULT_AGE: &str = "blue,12 month ago,white,1 month ago,red";

/// The settings read before the arguments, as git reads its configuration first.
pub(super) struct ColorConfig {
    lines: bool,
    age: bool,
    repeated: String,
    recent: Vec<(i64, String)>,
}

/// What the layout paints: the repeated-line color and the age colors with the author time each reaches.
#[derive(Default)]
pub(super) struct Coloring {
    pub repeated: Option<String>,
    pub age: Option<Vec<(i64, String)>>,
}

fn color_of(text: &str) -> Option<String> {
    let color = parse_color(text);
    if color.is_none() {
        eprintln!("error: invalid color value: {text}");
    }
    color
}

fn age_fields(spec: &str) -> Result<Vec<(i64, String)>> {
    let now = Local::now();
    let fields: Vec<&str> = spec.split(',').collect();
    let mut out = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        if i % 2 == 0 {
            let Some(color) = color_of(field) else {
                bail!("expecting a color: {field}");
            };
            let hop = match fields.get(i + 1) {
                Some(date) => parse_date(date, now).unwrap_or(now.timestamp()),
                None => i64::MAX,
            };
            out.push((hop, color));
        }
    }
    if fields.len().is_multiple_of(2) {
        bail!("must end with a color");
    }
    Ok(out)
}

impl ColorConfig {
    pub fn read(git: &GitHome) -> Result<Self> {
        let config = git.config();
        let (mut lines, mut age) = (false, false);
        match config.get("blame.coloring") {
            Some("repeatedLines") => lines = true,
            Some("highlightRecent") => age = true,
            Some("none") | None => {}
            Some(other) => eprintln!("warning: invalid value for 'blame.coloring': '{other}'"),
        }
        let mut repeated = parse_color("cyan").unwrap_or_default();
        if let Some(v) = config
            .get("color.blame.repeatedLines")
            .filter(|v| !v.is_empty())
        {
            match color_of(v) {
                Some(c) => repeated = c,
                None => eprintln!("warning: invalid value for 'color.blame.repeatedLines': '{v}'"),
            }
        }
        let spec = config
            .get("color.blame.highlightRecent")
            .unwrap_or(DEFAULT_AGE);
        Ok(Self {
            lines,
            age,
            repeated,
            recent: age_fields(spec)?,
        })
    }

    /// The flags decide when any is on, else the configuration does.
    pub fn resolve(self, lines_flag: bool, age_flag: bool) -> Coloring {
        let (lines, age) = if lines_flag || age_flag {
            (lines_flag, age_flag)
        } else {
            (self.lines, self.age)
        };
        Coloring {
            repeated: lines.then_some(self.repeated),
            age: age.then_some(self.recent),
        }
    }
}

impl Coloring {
    pub fn heat(&self, author_time: i64) -> Option<&str> {
        let age = self.age.as_ref()?;
        age.iter()
            .find(|(hop, _)| author_time <= *hop)
            .map(|(_, c)| c.as_str())
    }
}
