//! progress-gen — validate `progress/progress.toml` and render the public
//! progress matrix (`progress/progress.svg`) plus the README summary table.
//!
//! `progress.toml` is the single source of truth. Every percentage shown in
//! the repository is computed here from enumerated items:
//!
//! ```text
//! completion = (implemented + verified) / total items
//! verified   =  verified               / total items
//! ```
//!
//! Percentages are floored so they never overstate progress.
//!
//! Usage:
//!   cargo run -p progress-gen            # regenerate SVG + README block
//!   cargo run -p progress-gen -- --check # fail if anything is invalid or stale

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;

const README_START: &str = "<!-- progress-table:start -->";
const README_END: &str = "<!-- progress-table:end -->";

#[derive(Parser, Debug)]
#[command(about = "Validate progress.toml and render progress.svg")]
struct Args {
    /// Fail (exit 1) if progress.svg or the README table is stale instead of rewriting them.
    #[arg(long)]
    check: bool,
    /// Repository root (defaults to the workspace this tool was built from).
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    NotStarted,
    Investigating,
    Partial,
    Implemented,
    Verified,
    Blocked,
}

impl Status {
    const ALL: [Status; 6] = [
        Status::Verified,
        Status::Implemented,
        Status::Partial,
        Status::Investigating,
        Status::Blocked,
        Status::NotStarted,
    ];

    fn key(self) -> &'static str {
        match self {
            Status::NotStarted => "not_started",
            Status::Investigating => "investigating",
            Status::Partial => "partial",
            Status::Implemented => "implemented",
            Status::Verified => "verified",
            Status::Blocked => "blocked",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::NotStarted => "Not started",
            Status::Investigating => "Investigating",
            Status::Partial => "Partial",
            Status::Implemented => "Implemented",
            Status::Verified => "Verified",
            Status::Blocked => "Blocked",
        }
    }

    fn is_done(self) -> bool {
        matches!(self, Status::Implemented | Status::Verified)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    schema: u32,
    title: String,
    current_milestone: String,
    #[serde(default, rename = "milestone")]
    milestones: Vec<Milestone>,
    #[serde(default, rename = "category")]
    categories: Vec<Category>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Milestone {
    id: String,
    name: String,
    status: Status,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Category {
    id: String,
    name: String,
    #[serde(default, rename = "item")]
    items: Vec<Item>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    id: String,
    name: String,
    status: Status,
    /// Where the evidence for the status lives (doc, test, tool). Required for
    /// implemented/verified so progress cannot be claimed without backing.
    #[serde(default)]
    evidence: Option<String>,
    /// Required when status = "blocked".
    #[serde(default)]
    blocker: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    counts: [usize; 6],
}

impl Tally {
    fn add(&mut self, s: Status) {
        self.counts[Self::idx(s)] += 1;
    }
    fn idx(s: Status) -> usize {
        Status::ALL.iter().position(|x| *x == s).unwrap_or(0)
    }
    fn get(&self, s: Status) -> usize {
        self.counts[Self::idx(s)]
    }
    fn total(&self) -> usize {
        self.counts.iter().sum()
    }
    fn done(&self) -> usize {
        self.get(Status::Implemented) + self.get(Status::Verified)
    }
    fn merge(&mut self, o: &Tally) {
        for (a, b) in self.counts.iter_mut().zip(o.counts.iter()) {
            *a += *b;
        }
    }
}

/// Floored integer percentage; never overstates.
fn pct(num: usize, den: usize) -> usize {
    (num * 100).checked_div(den).unwrap_or(0)
}

fn validate(p: &Progress) -> Result<()> {
    let mut errors = Vec::new();
    if p.schema != 1 {
        errors.push(format!("unsupported schema {}", p.schema));
    }
    let mut mids = BTreeSet::new();
    for m in &p.milestones {
        if !mids.insert(m.id.as_str()) {
            errors.push(format!("duplicate milestone id {}", m.id));
        }
    }
    if !mids.contains(p.current_milestone.as_str()) {
        errors.push(format!(
            "current_milestone {} is not a declared milestone",
            p.current_milestone
        ));
    }
    if p.categories.is_empty() {
        errors.push("no categories".into());
    }
    let mut cids = BTreeSet::new();
    for c in &p.categories {
        if !cids.insert(c.id.as_str()) {
            errors.push(format!("duplicate category id {}", c.id));
        }
        if c.items.is_empty() {
            errors.push(format!("category {} has no items", c.id));
        }
        let mut iids = BTreeSet::new();
        for i in &c.items {
            let path = format!("{}.{}", c.id, i.id);
            if !iids.insert(i.id.as_str()) {
                errors.push(format!("duplicate item id {path}"));
            }
            let has = |o: &Option<String>| o.as_deref().is_some_and(|s| !s.trim().is_empty());
            if i.status.is_done() && !has(&i.evidence) {
                errors.push(format!(
                    "{path} is {} but has no `evidence` (no fake progress)",
                    i.status.key()
                ));
            }
            if i.status == Status::Blocked && !has(&i.blocker) {
                errors.push(format!("{path} is blocked but has no `blocker`"));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("progress.toml is invalid:\n  - {}", errors.join("\n  - "))
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const W: i64 = 980;
const LABEL_X: i64 = 24;
const CELLS_X: i64 = 206;
const CELL: i64 = 14;
const GAP: i64 = 4;
const PCT_COL_W: i64 = 150;

fn cells_per_row() -> i64 {
    (W - CELLS_X - PCT_COL_W - 24) / (CELL + GAP)
}

fn cell_svg(out: &mut String, x: i64, y: i64, s: Status, tip: &str) {
    let tip = esc(tip);
    let k = s.key();
    match s {
        Status::NotStarted => {
            let _ = writeln!(
                out,
                r#"<rect class="c {k}" x="{}" y="{}" width="{}" height="{}" rx="2"><title>{tip}</title></rect>"#,
                x as f64 + 0.5,
                y as f64 + 0.5,
                CELL - 1,
                CELL - 1
            );
        }
        Status::Partial => {
            let _ = writeln!(
                out,
                r#"<g><title>{tip}</title><rect class="c partial-o" x="{}" y="{}" width="{}" height="{}" rx="2"/><path class="partial-f" d="M{x} {y}h{h}v{CELL}h-{h}z"/></g>"#,
                x as f64 + 0.5,
                y as f64 + 0.5,
                CELL - 1,
                CELL - 1,
                h = CELL / 2
            );
        }
        Status::Verified => {
            let _ = writeln!(
                out,
                r#"<g><title>{tip}</title><rect class="c {k}" x="{x}" y="{y}" width="{CELL}" height="{CELL}" rx="2"/><path class="mark" d="M{} {}l3 3l6 -6"/></g>"#,
                x + 3,
                y + 7
            );
        }
        Status::Blocked => {
            let _ = writeln!(
                out,
                r#"<g><title>{tip}</title><rect class="c {k}" x="{x}" y="{y}" width="{CELL}" height="{CELL}" rx="2"/><path class="mark" d="M{} {}l6 6M{} {}l-6 6"/></g>"#,
                x + 4,
                y + 4,
                x + 10,
                y + 4
            );
        }
        Status::Investigating | Status::Implemented => {
            let _ = writeln!(
                out,
                r#"<rect class="c {k}" x="{x}" y="{y}" width="{CELL}" height="{CELL}" rx="2"><title>{tip}</title></rect>"#
            );
        }
    }
}

fn render_svg(p: &Progress) -> String {
    let per_row = cells_per_row().max(1);
    let mut overall = Tally::default();
    let mut body = String::new();
    let row_h = CELL + GAP;

    // Milestone strip
    let mut y: i64 = 104;
    let _ = writeln!(
        body,
        r#"<text class="lbl" x="{LABEL_X}" y="{}">Milestones</text>"#,
        y + CELL / 2 + 4
    );
    let mut mx = CELLS_X;
    for m in &p.milestones {
        let mut tip = format!("{} — {}: {}", m.id, m.name, m.status.label());
        if let Some(n) = &m.note {
            tip.push_str(" — ");
            tip.push_str(n);
        }
        cell_svg(&mut body, mx, y, m.status, &tip);
        let _ = writeln!(
            body,
            r#"<text class="muted" x="{}" y="{}">{}</text>"#,
            mx + CELL + 3,
            y + CELL / 2 + 4,
            esc(&m.id)
        );
        mx += CELL + 3 + (m.id.chars().count() as i64) * 8 + 10;
    }
    y += row_h + 18;

    for c in &p.categories {
        let mut t = Tally::default();
        for i in &c.items {
            t.add(i.status);
        }
        overall.merge(&t);
        let rows = (c.items.len() as i64 + per_row - 1) / per_row;
        let block_h = rows.max(1) * row_h;
        let mid = y + CELL / 2 + 4;
        let _ = writeln!(
            body,
            r#"<text class="lbl" x="{LABEL_X}" y="{mid}">{}</text>"#,
            esc(&c.name)
        );
        for (n, i) in c.items.iter().enumerate() {
            let n = n as i64;
            let cx = CELLS_X + (n % per_row) * (CELL + GAP);
            let cy = y + (n / per_row) * row_h;
            let mut tip = format!("{} / {} — {}", c.name, i.name, i.status.label());
            if let Some(n) = &i.note {
                tip.push_str(" — ");
                tip.push_str(n);
            }
            cell_svg(&mut body, cx, cy, i.status, &tip);
        }
        let px = W - PCT_COL_W;
        let _ = writeln!(
            body,
            r#"<text class="pct" x="{px}" y="{mid}">{}%</text><text class="muted" x="{}" y="{mid}">{}/{} · ✓{}</text>"#,
            pct(t.done(), t.total()),
            px + 46,
            t.done(),
            t.total(),
            t.get(Status::Verified)
        );
        y += block_h + 8;
    }

    // Legend
    let legend_y = y + 18;
    let mut legend = String::new();
    let mut lx = LABEL_X;
    for s in Status::ALL {
        cell_svg(&mut legend, lx, legend_y - 11, s, s.label());
        let label = format!("{} ({})", s.label(), overall.get(s));
        let _ = writeln!(
            legend,
            r#"<text class="muted" x="{}" y="{}">{}</text>"#,
            lx + CELL + 6,
            legend_y,
            esc(&label)
        );
        lx += CELL + 6 + (label.chars().count() as i64) * 7 + 22;
    }
    let h = legend_y + 26;

    let ms = p
        .milestones
        .iter()
        .find(|m| m.id == p.current_milestone)
        .map(|m| format!("{} — {}", m.id, m.name))
        .unwrap_or_default();

    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{h}" viewBox="0 0 {W} {h}" role="img" aria-label="{} — {}% complete, {}% verified">"#,
        esc(&p.title),
        pct(overall.done(), overall.total()),
        pct(overall.get(Status::Verified), overall.total())
    );
    out.push_str(
        r#"<style>
:root{--bg:#ffffff;--bd:#d0d7de;--fg:#1f2328;--mu:#59636e;--ns:#8c959f;--inv:#0969da;--par:#bf8700;--imp:#4ac26b;--ver:#1a7f37;--blk:#cf222e;--mk:#ffffff}
@media (prefers-color-scheme: dark){:root{--bg:#0d1117;--bd:#30363d;--fg:#e6edf3;--mu:#9198a1;--ns:#6e7681;--inv:#4493f8;--par:#d29922;--imp:#3fb950;--ver:#238636;--blk:#f85149;--mk:#ffffff}}
.bg{fill:var(--bg);stroke:var(--bd)}
text{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;fill:var(--fg)}
.ttl{font-size:20px;font-weight:600}
.sub{font-size:13px;fill:var(--mu)}
.lbl{font-size:13px}
.pct{font-size:13px;font-weight:600}
.muted{font-size:12px;fill:var(--mu)}
.not_started{fill:none;stroke:var(--ns);stroke-width:1}
.investigating{fill:var(--inv)}
.partial-o{fill:none;stroke:var(--par);stroke-width:1}
.partial-f{fill:var(--par)}
.implemented{fill:var(--imp)}
.verified{fill:var(--ver)}
.blocked{fill:var(--blk)}
.mark{fill:none;stroke:var(--mk);stroke-width:2;stroke-linecap:round;stroke-linejoin:round}
</style>
"#,
    );
    let _ = writeln!(
        out,
        r#"<rect class="bg" x="0.5" y="0.5" width="{}" height="{}" rx="8"/>"#,
        W - 1,
        h - 1
    );
    let _ = writeln!(
        out,
        r#"<text class="ttl" x="{LABEL_X}" y="38">{}</text>"#,
        esc(&p.title)
    );
    let _ = writeln!(
        out,
        r#"<text class="sub" x="{LABEL_X}" y="62">Completion {}% ({} of {} items implemented or verified) · Verified {}% ({}) · Current milestone: {}</text>"#,
        pct(overall.done(), overall.total()),
        overall.done(),
        overall.total(),
        pct(overall.get(Status::Verified), overall.total()),
        overall.get(Status::Verified),
        esc(&ms)
    );
    let _ = writeln!(
        out,
        r#"<text class="muted" x="{LABEL_X}" y="84">Generated from progress/progress.toml by tools/progress-gen. Completion = (implemented + verified) / total; floored.</text>"#
    );
    out.push_str(&body);
    out.push_str(&legend);
    out.push_str("</svg>\n");
    out
}

fn render_table(p: &Progress) -> String {
    let mut overall = Tally::default();
    let mut rows = String::new();
    for c in &p.categories {
        let mut t = Tally::default();
        for i in &c.items {
            t.add(i.status);
        }
        overall.merge(&t);
        let _ = writeln!(
            rows,
            "| {} | {} | {} | {} | {} | {}% | {}% |",
            c.name,
            t.total(),
            t.done(),
            t.get(Status::Verified),
            t.get(Status::Partial) + t.get(Status::Investigating),
            pct(t.done(), t.total()),
            pct(t.get(Status::Verified), t.total())
        );
    }
    let mut out = String::new();
    let _ = writeln!(out, "{README_START}");
    let _ = writeln!(
        out,
        "<!-- generated by `cargo run -p progress-gen`; do not edit by hand -->"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Category | Items | Implemented + verified | Verified | In progress | Completion | Verified % |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|");
    out.push_str(&rows);
    let _ = writeln!(
        out,
        "| **Overall** | **{}** | **{}** | **{}** | **{}** | **{}%** | **{}%** |",
        overall.total(),
        overall.done(),
        overall.get(Status::Verified),
        overall.get(Status::Partial) + overall.get(Status::Investigating),
        pct(overall.done(), overall.total()),
        pct(overall.get(Status::Verified), overall.total())
    );
    let _ = writeln!(out);
    let _ = write!(out, "{README_END}");
    out
}

fn splice_readme(readme: &str, table: &str) -> Result<String> {
    let start = readme
        .find(README_START)
        .context("README.md is missing the progress-table start marker")?;
    let end_rel = readme[start..]
        .find(README_END)
        .context("README.md is missing the progress-table end marker")?;
    let end = start + end_rel + README_END.len();
    let mut s = String::with_capacity(readme.len() + table.len());
    s.push_str(&readme[..start]);
    s.push_str(table);
    s.push_str(&readme[end..]);
    Ok(s)
}

fn default_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = args.root.unwrap_or_else(default_root);
    let toml_path = root.join("progress/progress.toml");
    let svg_path = root.join("progress/progress.svg");
    let readme_path = root.join("README.md");

    let src = fs::read_to_string(&toml_path)
        .with_context(|| format!("reading {}", toml_path.display()))?;
    let p: Progress = toml::from_str(&src).context("parsing progress.toml")?;
    validate(&p)?;

    let svg = render_svg(&p);
    let readme = fs::read_to_string(&readme_path)
        .with_context(|| format!("reading {}", readme_path.display()))?;
    let new_readme = splice_readme(&readme, &render_table(&p))?;

    if args.check {
        let mut stale = Vec::new();
        let cur_svg = fs::read_to_string(&svg_path).unwrap_or_default();
        // Normalise line endings so a Windows checkout with autocrlf does not fail.
        if cur_svg.replace("\r\n", "\n") != svg {
            stale.push("progress/progress.svg");
        }
        if readme.replace("\r\n", "\n") != new_readme.replace("\r\n", "\n") {
            stale.push("README.md progress table");
        }
        if !stale.is_empty() {
            bail!(
                "stale generated output: {} — run `cargo run -p progress-gen`",
                stale.join(", ")
            );
        }
        println!("progress-gen: progress.toml valid; generated output up to date");
    } else {
        fs::write(&svg_path, &svg)?;
        if new_readme != readme {
            fs::write(&readme_path, &new_readme)?;
        }
        println!(
            "progress-gen: wrote {} and README table",
            svg_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = r#"
schema = 1
title = "T"
current_milestone = "M0"
[[milestone]]
id = "M0"
name = "Zero"
status = "partial"
[[category]]
id = "a"
name = "A"
[[category.item]]
id = "x"
name = "X"
status = "implemented"
evidence = "tests"
[[category.item]]
id = "y"
name = "Y"
status = "not_started"
"#;

    #[test]
    fn floors_percentages() {
        assert_eq!(pct(2, 3), 66);
        assert_eq!(pct(0, 0), 0);
        assert_eq!(pct(5, 5), 100);
    }

    #[test]
    fn minimal_is_valid_and_deterministic() {
        let p: Progress = toml::from_str(MIN).unwrap();
        validate(&p).unwrap();
        assert_eq!(render_svg(&p), render_svg(&p));
        assert!(render_table(&p).contains("| A | 2 | 1 | 0 | 0 | 50% | 0% |"));
    }

    #[test]
    fn implemented_without_evidence_is_rejected() {
        let bad = MIN.replace("evidence = \"tests\"\n", "");
        let p: Progress = toml::from_str(&bad).unwrap();
        assert!(validate(&p).is_err());
    }

    #[test]
    fn blocked_requires_blocker() {
        let bad = MIN.replace("status = \"not_started\"", "status = \"blocked\"");
        let p: Progress = toml::from_str(&bad).unwrap();
        assert!(validate(&p).is_err());
    }

    #[test]
    fn unknown_status_is_rejected() {
        let bad = MIN.replace("status = \"not_started\"", "status = \"done\"");
        assert!(toml::from_str::<Progress>(&bad).is_err());
    }

    #[test]
    fn splice_replaces_only_block() {
        let r = format!("a\n{README_START}\nold\n{README_END}\nb\n");
        let s = splice_readme(&r, &format!("{README_START}\nnew\n{README_END}")).unwrap();
        assert_eq!(s, format!("a\n{README_START}\nnew\n{README_END}\nb\n"));
    }
}
