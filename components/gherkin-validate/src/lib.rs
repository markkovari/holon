//! `gherkin-validate` — is this `.feature` file actually Gherkin, and will it run?
//!
//! `tests/validate.rs` is the specification and is not writable from here.
//!
//! Gherkin is executable specification, and the ways it breaks are quiet. An
//! `Examples` row one cell short, a `<placeholder>` with a typo in it, an `And`
//! with nothing above it — none of these announce themselves. The typo is the
//! worst: nothing fails, the step just receives the literal text `<usrename>`.
//!
//! So this reports EVERY problem with a line and a column. A validator that
//! surfaces one problem per run is a validator people run once.
//!
//! ## What it does not do
//!
//! It does not check that a step has a matching step definition. That needs the
//! step registry of whatever runs the suite — a different input and a different
//! capability. This is syntax and structure only.
//!
//! ## Severity, because the reference implementation disagreed with me
//!
//! The first version of this treated a scenario with no steps and an outline with
//! no `Examples` as errors. Running `cucumber/gherkin`'s own `testdata/` said
//! otherwise: both appear in files that corpus calls GOOD. A validator that refuses
//! them disagrees with cucumber about whether a file is valid, which makes it
//! useless as a gate. They are `warning` now — still worth saying, never a refusal.
//!
//! The same run corrected three outright bugs, all of them mine:
//!
//!   - `*` is a step keyword, not a continuation. `good/star-keywords.feature` is a
//!     whole file of bullets and it is idiomatic Gherkin.
//!   - A plain `Scenario:` with an `Examples:` under it IS an outline. Gherkin
//!     treats the keywords as interchangeable; `good/scenario_outline.feature`
//!     depends on it.
//!   - A table row has to close with `|`. Not checking that missed two of the
//!     twelve files the corpus calls bad.
//!
//! ## A language it cannot read is declined, not failed
//!
//! Gherkin has keywords in seventy-odd languages. This one knows English. A file
//! declaring `# language: de` therefore gets ONE problem saying so, rather than a
//! page of complaints about missing English keywords — a confident wrong answer
//! being worse than an honest refusal.

use std::collections::BTreeSet;

/// What is wrong. See the WIT for why each case carries what it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    NoFeature,
    MultipleFeatures,
    ContentBeforeFeature(String),
    StepOutsideScenario,
    ContinuationWithoutAStep(String),
    OutlineWithoutExamples(String),
    ExamplesOutsideAScenario,
    EmptyScenario(String),
    RowWidth { expected: u32, found: u32 },
    RepeatedDocstring,
    DanglingTag,
    MalformedTag(String),
    InvalidLanguage(String),
    UnknownPlaceholder(String),
    DuplicateColumn(String),
    BackgroundAfterAScenario,
    MultipleBackgrounds,
    UnterminatedDocstring(String),
    UnsupportedLanguage(String),
}

/// How much a problem means. See the WIT: the split is what the corpus forced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Cucumber cannot parse this.
    Error,
    /// It parses, runs, and is almost certainly not what anyone meant.
    Warning,
    /// The file was not judged at all.
    Declined,
}

impl Kind {
    /// Errors are what cucumber refuses; warnings are what it accepts and nobody
    /// meant. Every classification here was checked against `testdata/`: each of
    /// the twelve bad files yields at least one `Error`, and none of the fifty good
    /// ones does.
    pub fn severity(&self) -> Severity {
        match self {
            Kind::UnsupportedLanguage(_) => Severity::Declined,
            Kind::ContinuationWithoutAStep(_)
            | Kind::OutlineWithoutExamples(_)
            | Kind::EmptyScenario(_)
            | Kind::UnknownPlaceholder(_)
            | Kind::DuplicateColumn(_) => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

/// One step of a scenario, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// `Given`, `When`, `Then`, `And`, `But` or `*` — as written, not resolved.
    /// A runner that wants the resolved sense can walk the list; one that wants to
    /// print the scenario back needs what the author typed.
    pub keyword: String,
    pub text: String,
    pub line: u32,
    /// A docstring or data table attached to this step, line by line, with the
    /// fences removed. Empty when the step has neither.
    pub argument: Vec<String>,
}

/// One `Examples:` table under an outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExampleTable {
    pub name: String,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// A scenario, an outline, or a background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub name: String,
    pub line: u32,
    pub tags: Vec<String>,
    pub steps: Vec<Step>,
    /// Empty for a plain scenario. Non-empty means every row is a test case.
    pub examples: Vec<ExampleTable>,
}

/// A whole feature file, once it is known to be readable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub feature: String,
    pub tags: Vec<String>,
    /// Steps every scenario runs first. Flattened across `Rule:` containers, since
    /// a runner cares what runs, not which container declared it.
    pub background: Vec<Step>,
    pub scenarios: Vec<Scenario>,
}

/// One problem, and where. Both coordinates are 1-based; the column counts
/// characters, not bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: u32,
    pub column: u32,
    pub kind: Kind,
}

impl Problem {
    pub fn severity(&self) -> Severity {
        self.kind.severity()
    }
}

/// Step keywords. `*` is a real one — Gherkin allows a bullet in place of any.
const STEPS: &[&str] = &["Given", "When", "Then", "And", "But", "*"];
/// The two that only make sense after another step.
///
/// `*` is NOT one. A bullet is a step keyword in its own right, a whole file of them
/// is idiomatic, and treating it as a continuation flagged four files cucumber calls
/// good — including `star-keywords.feature`, which exists to demonstrate exactly
/// this.
const CONTINUATIONS: &[&str] = &["And", "But"];
const FENCES: &[&str] = &["\"\"\"", "```"];

/// Every language Gherkin has keywords for, from `gherkin-languages.json`.
///
/// The list is here to tell two different things apart. `# language: fr` names a
/// real dialect this component cannot read — a file to DECLINE. `#language:no-such`
/// names nothing, and is a broken header: an error, and one of the twelve files in
/// `testdata/bad/`. Without the list both look identical.
const DIALECTS: &[&str] = &[
    "af", "am", "amh", "an", "ar", "ast", "az", "be", "bg", "bm", "bs", "ca",
    "cs", "cy-GB", "da", "de", "el", "em", "en", "en-Scouse", "en-au",
    "en-lol", "en-old", "en-pirate", "en-tx", "eo", "es", "et", "fa", "fi",
    "fr", "ga", "gj", "gl", "he", "hi", "hr", "ht", "hu", "id", "is", "it",
    "ja", "jv", "ka", "kn", "ko", "lt", "lu", "lv", "mk-Cyrl", "mk-Latn", "ml",
    "mn", "mr", "ne", "nl", "no", "pa", "pl", "pt", "ro", "ru", "sk", "sl",
    "sr-Cyrl", "sr-Latn", "sv", "ta", "te", "th", "tlh", "tr", "tt", "uk",
    "ur", "uz", "vi", "zh-CN", "zh-TW",
];

#[derive(PartialEq, Eq, Clone, Copy)]
enum BlockKind {
    Background,
    Scenario,
    Outline,
}

/// The scenario, outline or background currently being read.
struct Block {
    kind: BlockKind,
    name: String,
    line: u32,
    column: u32,
    steps: usize,
    /// Outline only: how many `Examples` blocks it has.
    examples: usize,
    /// Outline only: the union of every `Examples` header's columns. Union, not
    /// intersection: a placeholder provided by any one block is substituted there,
    /// and flagging it because a *sibling* block omits it would refuse a legitimate
    /// two-table outline.
    headers: BTreeSet<String>,
    /// Every `<placeholder>` seen, with where it was. Checked only for outlines.
    placeholders: Vec<(String, u32, u32)>,
    /// Whether the step being read already has a docstring.
    had_docstring: bool,
    /// The parsed form, built alongside the checks so one walk does both.
    parsed: Scenario,
}

/// Which table, if any, the next `|` row belongs to.
enum Table {
    None,
    /// A step's data table; the first row set the width.
    Data(usize),
    /// Straight after `Examples:` — the next row is the header.
    ExamplesHeader,
    /// Rows under a header of this width.
    ExamplesRows(usize),
}

/// 1-based column where a line's content starts.
fn indent_col(raw: &str) -> u32 {
    raw.chars().take_while(|c| c.is_whitespace()).count() as u32 + 1
}

/// The step keyword a line starts with, if any.
///
/// The keyword must be followed by whitespace, so `Andrew types` is a description
/// line and not a step called `rew types`.
fn step_keyword(t: &str) -> Option<&'static str> {
    STEPS.iter().copied().find(|kw| {
        t.strip_prefix(kw).is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })
}

/// The structural keyword a line starts with, and whatever followed the colon.
fn block_keyword(t: &str) -> Option<(&'static str, String)> {
    // `Scenario Outline:` cannot be confused with `Scenario:` — the character after
    // `Scenario` is a space rather than a colon — so this list is ordered for
    // reading rather than for correctness.
    const KEYWORDS: &[(&str, &str)] = &[
        ("Feature:", "feature"),
        ("Rule:", "rule"),
        ("Background:", "background"),
        ("Scenario Outline:", "outline"),
        ("Scenario Template:", "outline"),
        ("Scenario:", "scenario"),
        ("Example:", "scenario"),
        ("Examples:", "examples"),
        ("Scenarios:", "examples"),
    ];
    for (literal, canonical) in KEYWORDS {
        if let Some(rest) = t.strip_prefix(literal) {
            return Some((canonical, rest.trim().to_string()));
        }
    }
    None
}

/// The cells of a table row: only what is BETWEEN pipes.
///
/// Anything after the last `|` is not part of the table. That is not a liberty —
/// `testdata/good/extra_table_content.feature` says so in its own description, and
/// is a file cucumber calls good whose rows carry trailing junk.
///
/// It also removes the need for a rule about unclosed rows: a row that got cut, or
/// one ending in a `\` that eats the closing pipe, simply yields fewer cells and is
/// caught by the width check. Both of those are files in `testdata/bad/`, and both
/// were missed when this counted a trailing fragment as a cell.
///
/// `\|` is an escaped pipe and does not split a cell — which is how a Gherkin table
/// carries a value with a pipe in it.
fn cells(row: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    let mut inside = false;
    for c in row.chars() {
        if escaped {
            match c {
                'n' => cur.push('\n'),
                '|' | '\\' => cur.push(c),
                other => {
                    cur.push('\\');
                    cur.push(other);
                }
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '|' {
            if inside {
                out.push(cur.trim().to_string());
            }
            cur.clear();
            inside = true;
        } else if inside {
            cur.push(c);
        }
    }
    // `cur` here is whatever followed the last pipe. Deliberately dropped.
    out
}

/// Every `<placeholder>` in a line, with the character offset of its `<`.
fn placeholders(text: &str) -> Vec<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            // Stop at the next `<` too, so `a < b and <c>` finds `c` and not `
            // b and <c`.
            if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '>' || chars[j] == '<') {
                if chars[end] == '>' && end > i + 1 {
                    out.push((chars[i + 1..end].iter().collect(), i));
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// `# language: xx`, if that is what this comment is.
fn language_of(t: &str) -> Option<String> {
    let rest = t.strip_prefix('#')?;
    let rest = rest.trim_start().strip_prefix("language")?;
    let code = rest.trim_start().strip_prefix(':')?;
    Some(code.trim().to_string())
}

/// Every problem in `source`, in the order they appear. Empty means valid.
pub fn validate(source: &str) -> Vec<Problem> {
    walk(source).1
}

/// The parsed feature, when nothing is an `error`.
///
/// Warnings do NOT stop it: a scenario with no steps and an outline with no
/// examples both parse, cucumber says so, and a runner that refused them would
/// disagree with the reference about what a feature file IS. They come back from
/// `validate` and a caller decides.
///
/// One walk produces both, because a parser and a validator that disagree about a
/// file is the bug neither of them can report.
pub fn parse(source: &str) -> Result<Document, Vec<Problem>> {
    let (doc, problems) = walk(source);
    if problems.iter().any(|p| p.severity() == Severity::Error) {
        return Err(problems);
    }
    Ok(doc)
}

fn walk(source: &str) -> (Document, Vec<Problem>) {
    let lines: Vec<&str> = source.lines().collect();
    let mut problems: Vec<Problem> = Vec::new();
    let mut doc = Document::default();

    // The language header belongs on the first line; scanning the leading comments
    // for it is more forgiving than the specification and costs nothing.
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if !t.starts_with('#') {
            break;
        }
        if let Some(code) = language_of(t) {
            if code == "en" {
                break;
            }
            if !DIALECTS.contains(&code.as_str()) {
                // Not a dialect at all — a broken header, and the file's own claim
                // about itself is wrong. That IS a defect.
                return (
                    doc,
                    vec![Problem {
                        line: i as u32 + 1,
                        column: indent_col(raw),
                        kind: Kind::InvalidLanguage(code),
                    }],
                );
            }
            // A real dialect this component cannot read. Decline the whole file:
            // judging French keywords against an English table would bury the one
            // useful sentence under a page of noise.
            return (
                doc,
                vec![Problem {
                    line: i as u32 + 1,
                    column: indent_col(raw),
                    kind: Kind::UnsupportedLanguage(code),
                }],
            );
        }
    }

    let mut feature: Option<u32> = None;
    let mut first_content: Option<(u32, u32)> = None;
    // Per `Feature:` or `Rule:` — a Rule is its own container, so it may have its
    // own Background.
    let mut backgrounds = 0usize;
    let mut scenarios = 0usize;
    let mut block: Option<Block> = None;
    let mut table = Table::None;
    let mut docstring: Option<(&str, u32)> = None;
    // Tags waiting for something to tag. At the end of the file they are a defect:
    // `testdata/bad/unexpected_eof.feature` is a feature, a blank line and a tag.
    let mut pending_tags: Option<(u32, u32)> = None;
    // Tag lines stack: `@a\n@b\nScenario:` is two tags on one scenario.
    let mut tag_names: Vec<String> = Vec::new();

    for (idx, raw) in lines.iter().enumerate() {
        let line = idx as u32 + 1;
        let col = indent_col(raw);
        let t = raw.trim_start();
        let trimmed = raw.trim();

        // Inside a docstring everything is text, including things that look like
        // Gherkin. Getting this wrong turns every example in the documentation of a
        // testing tool into a parse error.
        if let Some((fence, _)) = docstring {
            if trimmed.starts_with(fence) {
                docstring = None;
            } else if let Some(b) = block.as_mut() {
                for (name, at) in placeholders(raw) {
                    b.placeholders.push((name, line, at as u32 + 1));
                }
                // The docstring belongs to the step above it.
                if let Some(step) = b.parsed.steps.last_mut() {
                    step.argument.push(raw.trim().to_string());
                }
            }
            continue;
        }

        if trimmed.is_empty() {
            table = Table::None;
            continue;
        }
        // A comment may sit inside a table without ending it.
        if t.starts_with('#') {
            continue;
        }
        if t.starts_with('@') {
            table = Table::None;
            pending_tags = Some((line, col));
            tag_names.extend(
                t.split_whitespace().take_while(|w| !w.starts_with('#')).map(str::to_string),
            );
            // A tag cannot contain whitespace, so every token on the line has to be
            // one. `testdata/bad/whitespace_in_tags.feature` is `@a tag containing
            // whitespace`, which reads as a sentence and is four broken tags.
            for word in t.split_whitespace() {
                // A trailing comment is legal on a tag line, and it ends the tags:
                // `good/tags.feature` has `@comment_tag1 #a comment`. Note that the
                // same file has `@comment_tag#2`, so a `#` only starts a comment
                // when it starts a token.
                if word.starts_with('#') {
                    break;
                }
                if !word.starts_with('@') {
                    problems.push(Problem {
                        line,
                        column: col,
                        kind: Kind::MalformedTag(word.to_string()),
                    });
                    break;
                }
            }
            continue;
        }
        if let Some(fence) = FENCES.iter().find(|f| trimmed.starts_with(**f)) {
            if let Some(b) = block.as_mut() {
                if b.had_docstring {
                    // The first docstring is the step's argument. There is nowhere
                    // for a second one to go.
                    problems.push(Problem { line, column: col, kind: Kind::RepeatedDocstring });
                }
                b.had_docstring = true;
            }
            docstring = Some((fence, line));
            table = Table::None;
            continue;
        }

        if t.starts_with('|') {
            let row = cells(raw);
            match table {
                Table::ExamplesHeader => {
                    let mut seen = BTreeSet::new();
                    for name in &row {
                        if !seen.insert(name.clone()) {
                            problems.push(Problem {
                                line,
                                column: col,
                                kind: Kind::DuplicateColumn(name.clone()),
                            });
                        }
                    }
                    if let Some(b) = block.as_mut() {
                        b.headers.extend(row.iter().cloned());
                        if let Some(t) = b.parsed.examples.last_mut() {
                            t.header = row.clone();
                        }
                    }
                    table = Table::ExamplesRows(row.len());
                }
                Table::ExamplesRows(width) | Table::Data(width) => {
                    if row.len() != width {
                        problems.push(Problem {
                            line,
                            column: col,
                            kind: Kind::RowWidth {
                                expected: width as u32,
                                found: row.len() as u32,
                            },
                        });
                    }
                    if let Some(b) = block.as_mut() {
                        match table {
                            Table::ExamplesRows(_) => {
                                if let Some(t) = b.parsed.examples.last_mut() {
                                    t.rows.push(row.clone());
                                }
                            }
                            // A data table belongs to the step above it, raw, so a
                            // runner can decide whether it is a table of rows or a
                            // table of key/value pairs.
                            _ => {
                                if let Some(step) = b.parsed.steps.last_mut() {
                                    step.argument.push(raw.trim().to_string());
                                }
                            }
                        }
                    }
                    // Only a DATA table can carry a placeholder; an `Examples` cell
                    // is the literal value being substituted in.
                    if matches!(table, Table::Data(_)) {
                        if let Some(b) = block.as_mut() {
                            for (name, at) in placeholders(raw) {
                                b.placeholders.push((name, line, at as u32 + 1));
                            }
                        }
                    }
                }
                Table::None => {
                    table = Table::Data(row.len());
                    if let Some(b) = block.as_mut() {
                        if let Some(step) = b.parsed.steps.last_mut() {
                            step.argument.push(raw.trim().to_string());
                        }
                        for (name, at) in placeholders(raw) {
                            b.placeholders.push((name, line, at as u32 + 1));
                        }
                    }
                }
            }
            continue;
        }

        if first_content.is_none() {
            first_content = Some((line, col));
        }

        if let Some((keyword, title)) = block_keyword(t) {
            pending_tags = None;
            if keyword != "examples" {
                table = Table::None;
                close_block(&mut problems, &mut doc, block.take());
            }
            match keyword {
                "feature" => {
                    if feature.is_some() {
                        problems.push(Problem { line, column: col, kind: Kind::MultipleFeatures });
                    } else {
                        feature = Some(line);
                        doc.feature = title.clone();
                        doc.tags = std::mem::take(&mut tag_names);
                    }
                    backgrounds = 0;
                    scenarios = 0;
                }
                "rule" => {
                    backgrounds = 0;
                    scenarios = 0;
                }
                "background" => {
                    if scenarios > 0 {
                        problems.push(Problem {
                            line,
                            column: col,
                            kind: Kind::BackgroundAfterAScenario,
                        });
                    } else if backgrounds > 0 {
                        problems.push(Problem {
                            line,
                            column: col,
                            kind: Kind::MultipleBackgrounds,
                        });
                    }
                    backgrounds += 1;
                    block = Some(new_block(BlockKind::Background, "Background".into(), line, col));
                }
                "scenario" | "outline" => {
                    scenarios += 1;
                    let kind = if keyword == "outline" {
                        BlockKind::Outline
                    } else {
                        BlockKind::Scenario
                    };
                    let mut b = new_block(kind, title, line, col);
                    b.parsed.tags = std::mem::take(&mut tag_names);
                    block = Some(b);
                }
                // `Examples:` under a plain `Scenario:` PROMOTES it to an outline.
                // Gherkin treats the two keywords as interchangeable, and
                // `testdata/good/scenario_outline.feature` is a `Scenario:` with an
                // `Examples:` under it. Refusing that flagged three good files.
                "examples" => match block.as_mut() {
                    Some(b) => {
                        b.kind = BlockKind::Outline;
                        b.examples += 1;
                        b.parsed.examples.push(ExampleTable {
                            name: title.clone(),
                            header: Vec::new(),
                            rows: Vec::new(),
                        });
                        table = Table::ExamplesHeader;
                    }
                    None => {
                        problems.push(Problem {
                            line,
                            column: col,
                            kind: Kind::ExamplesOutsideAScenario,
                        });
                        table = Table::None;
                    }
                },
                _ => {}
            }
            continue;
        }

        if let Some(keyword) = step_keyword(t) {
            table = Table::None;
            match block.as_mut() {
                None => problems.push(Problem {
                    line,
                    column: col,
                    kind: Kind::StepOutsideScenario,
                }),
                Some(b) => {
                    if b.steps == 0 && CONTINUATIONS.contains(&keyword) {
                        problems.push(Problem {
                            line,
                            column: col,
                            kind: Kind::ContinuationWithoutAStep(keyword.to_string()),
                        });
                    }
                    b.steps += 1;
                    b.had_docstring = false;
                    b.parsed.steps.push(Step {
                        keyword: keyword.to_string(),
                        text: t[keyword.len()..].trim().to_string(),
                        line,
                        argument: Vec::new(),
                    });
                    // Collected for a plain `Scenario:` too, because an `Examples:`
                    // further down promotes it to an outline and by then these lines
                    // are read. Only CHECKED if it ends up an outline.
                    for (name, at) in placeholders(raw) {
                        b.placeholders.push((name, line, at as u32 + 1));
                    }
                }
            }
            continue;
        }

        // Free text. Legal as a description under a `Feature:` or a scenario;
        // above the feature it is a stray paste.
        table = Table::None;
        if feature.is_none() {
            problems.push(Problem {
                line,
                column: col,
                kind: Kind::ContentBeforeFeature(trimmed.to_string()),
            });
        }
    }

    if let Some((line, column)) = pending_tags {
        problems.push(Problem { line, column, kind: Kind::DanglingTag });
    }
    if let Some((fence, line)) = docstring {
        problems.push(Problem {
            line,
            // Reported where it OPENED, because that is where the fix goes.
            column: indent_col(lines[line as usize - 1]),
            kind: Kind::UnterminatedDocstring(fence.to_string()),
        });
    }
    close_block(&mut problems, &mut doc, block.take());

    if feature.is_none() {
        if let Some((line, column)) = first_content {
            problems.push(Problem { line, column, kind: Kind::NoFeature });
        }
    }

    (doc, problems)
}

fn new_block(kind: BlockKind, name: String, line: u32, column: u32) -> Block {
    let parsed_name = name.clone();
    Block {
        kind,
        name,
        line,
        column,
        steps: 0,
        examples: 0,
        headers: BTreeSet::new(),
        placeholders: Vec::new(),
        had_docstring: false,
        parsed: Scenario {
            name: parsed_name,
            line,
            tags: Vec::new(),
            steps: Vec::new(),
            examples: Vec::new(),
        },
    }
}

/// The checks that can only be made once a block has ended.
fn close_block(problems: &mut Vec<Problem>, doc: &mut Document, block: Option<Block>) {
    let Some(b) = block else { return };
    // A background's steps run before every scenario, so a runner wants them
    // flattened rather than as a scenario of their own.
    match b.kind {
        BlockKind::Background => doc.background.extend(b.parsed.steps.clone()),
        _ => doc.scenarios.push(b.parsed.clone()),
    }
    if b.steps == 0 {
        problems.push(Problem {
            line: b.line,
            column: b.column,
            kind: Kind::EmptyScenario(b.name.clone()),
        });
    }
    if b.kind != BlockKind::Outline {
        return;
    }
    if b.examples == 0 {
        problems.push(Problem {
            line: b.line,
            column: b.column,
            kind: Kind::OutlineWithoutExamples(b.name.clone()),
        });
    }
    let mut reported = BTreeSet::new();
    for (name, line, column) in b.placeholders {
        if !b.headers.contains(&name) && reported.insert(name.clone()) {
            problems.push(Problem { line, column, kind: Kind::UnknownPlaceholder(name) });
        }
    }
}

// ---- the component -----------------------------------------------------
//
// A mapping between the WIT types and the ones above, and nothing else. The logic is
// judged by `tests/validate.rs` against the plain functions, and a component that
// re-derived any of it would be untested by its own specification.
//
// Gated on the target like `components/demo`: a `cdylib` carrying wit-bindgen's
// exports does not link natively, and `cargo test` builds every crate-type before it
// runs a test — so without this the held-out specification cannot run at all.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::gherkin::validate::validator as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
fn kind_out(k: Kind) -> w::ProblemKind {
    match k {
        Kind::NoFeature => w::ProblemKind::NoFeature,
        Kind::MultipleFeatures => w::ProblemKind::MultipleFeatures,
        Kind::ContentBeforeFeature(s) => w::ProblemKind::ContentBeforeFeature(s),
        Kind::StepOutsideScenario => w::ProblemKind::StepOutsideScenario,
        Kind::ContinuationWithoutAStep(s) => w::ProblemKind::ContinuationWithoutAStep(s),
        Kind::OutlineWithoutExamples(s) => w::ProblemKind::OutlineWithoutExamples(s),
        Kind::ExamplesOutsideAScenario => w::ProblemKind::ExamplesOutsideAScenario,
        Kind::EmptyScenario(s) => w::ProblemKind::EmptyScenario(s),
        Kind::RowWidth { expected, found } => w::ProblemKind::RowWidth((expected, found)),
        Kind::RepeatedDocstring => w::ProblemKind::RepeatedDocstring,
        Kind::DanglingTag => w::ProblemKind::DanglingTag,
        Kind::MalformedTag(s) => w::ProblemKind::MalformedTag(s),
        Kind::InvalidLanguage(s) => w::ProblemKind::InvalidLanguage(s),
        Kind::UnknownPlaceholder(s) => w::ProblemKind::UnknownPlaceholder(s),
        Kind::DuplicateColumn(s) => w::ProblemKind::DuplicateColumn(s),
        Kind::BackgroundAfterAScenario => w::ProblemKind::BackgroundAfterAScenario,
        Kind::MultipleBackgrounds => w::ProblemKind::MultipleBackgrounds,
        Kind::UnterminatedDocstring(s) => w::ProblemKind::UnterminatedDocstring(s),
        Kind::UnsupportedLanguage(s) => w::ProblemKind::UnsupportedLanguage(s),
    }
}

#[cfg(target_arch = "wasm32")]
fn problem_out(p: Problem) -> w::Problem {
    w::Problem {
        line: p.line,
        column: p.column,
        severity: match p.severity() {
            Severity::Error => w::Severity::Error,
            Severity::Warning => w::Severity::Warning,
            Severity::Declined => w::Severity::Declined,
        },
        kind: kind_out(p.kind),
    }
}

#[cfg(target_arch = "wasm32")]
fn step_out(s: Step) -> w::Step {
    w::Step { keyword: s.keyword, text: s.text, line: s.line, argument: s.argument }
}

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn parse(source: String) -> Result<w::Document, Vec<w::Problem>> {
        match crate::parse(&source) {
            Ok(d) => Ok(w::Document {
                feature: d.feature,
                tags: d.tags,
                background: d.background.into_iter().map(step_out).collect(),
                scenarios: d
                    .scenarios
                    .into_iter()
                    .map(|sc| w::Scenario {
                        name: sc.name,
                        line: sc.line,
                        tags: sc.tags,
                        steps: sc.steps.into_iter().map(step_out).collect(),
                        examples: sc
                            .examples
                            .into_iter()
                            .map(|e| w::ExampleTable {
                                name: e.name,
                                header: e.header,
                                rows: e.rows,
                            })
                            .collect(),
                    })
                    .collect(),
            }),
            Err(ps) => Err(ps.into_iter().map(problem_out).collect()),
        }
    }

    fn validate(source: String) -> Vec<w::Problem> {
        crate::validate(&source).into_iter().map(problem_out).collect()
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
