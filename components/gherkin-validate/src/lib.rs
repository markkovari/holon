//! `gherkin-validate` — is this `.feature` file actually Gherkin, and will it run?

use std::collections::BTreeSet;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    /// It parses, runs, and is almost certainly not what anyone meant.
    Warning,
    /// The file was not judged at all.
    Declined,
}

impl Kind {
    /// Checked against `testdata/`: each of the twelve bad files yields an `Error`, no good one does.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// As written, not resolved.
    pub keyword: String,
    pub text: String,
    pub line: u32,
    pub argument: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExampleTable {
    pub name: String,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub name: String,
    pub line: u32,
    pub tags: Vec<String>,
    pub steps: Vec<Step>,
    pub examples: Vec<ExampleTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub feature: String,
    pub tags: Vec<String>,
    /// Flattened across `Rule:` containers: a runner cares what runs, not where it was declared.
    pub background: Vec<Step>,
    pub scenarios: Vec<Scenario>,
}

/// 1-based; the column counts characters, not bytes.
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

const STEPS: &[&str] = &["Given", "When", "Then", "And", "But", "*"];
/// `*` is not one: it is a step keyword in its own right (`star-keywords.feature`).
const CONTINUATIONS: &[&str] = &["And", "But"];
const FENCES: &[&str] = &["\"\"\"", "```"];

/// Tells a real dialect this cannot read (decline) from `#language:no-such` (an error).
const DIALECTS: &[&str] = &[
    "af",
    "am",
    "amh",
    "an",
    "ar",
    "ast",
    "az",
    "be",
    "bg",
    "bm",
    "bs",
    "ca",
    "cs",
    "cy-GB",
    "da",
    "de",
    "el",
    "em",
    "en",
    "en-Scouse",
    "en-au",
    "en-lol",
    "en-old",
    "en-pirate",
    "en-tx",
    "eo",
    "es",
    "et",
    "fa",
    "fi",
    "fr",
    "ga",
    "gj",
    "gl",
    "he",
    "hi",
    "hr",
    "ht",
    "hu",
    "id",
    "is",
    "it",
    "ja",
    "jv",
    "ka",
    "kn",
    "ko",
    "lt",
    "lu",
    "lv",
    "mk-Cyrl",
    "mk-Latn",
    "ml",
    "mn",
    "mr",
    "ne",
    "nl",
    "no",
    "pa",
    "pl",
    "pt",
    "ro",
    "ru",
    "sk",
    "sl",
    "sr-Cyrl",
    "sr-Latn",
    "sv",
    "ta",
    "te",
    "th",
    "tlh",
    "tr",
    "tt",
    "uk",
    "ur",
    "uz",
    "vi",
    "zh-CN",
    "zh-TW",
];

#[derive(PartialEq, Eq, Clone, Copy)]
enum BlockKind {
    Background,
    Scenario,
    Outline,
}

struct Block {
    kind: BlockKind,
    name: String,
    line: u32,
    column: u32,
    steps: usize,
    examples: usize,
    /// Union, not intersection: a placeholder any one `Examples` block provides is substituted there.
    headers: BTreeSet<String>,
    placeholders: Vec<(String, u32, u32)>,
    had_docstring: bool,
    parsed: Scenario,
}

enum Table {
    None,
    Data(usize),
    ExamplesHeader,
    ExamplesRows(usize),
}

fn indent_col(raw: &str) -> u32 {
    raw.chars().take_while(|c| c.is_whitespace()).count() as u32 + 1
}

/// Must be followed by whitespace, so `Andrew types` is not a step called `rew types`.
fn step_keyword(t: &str) -> Option<&'static str> {
    STEPS
        .iter()
        .copied()
        .find(|kw| t.strip_prefix(kw).is_some_and(|rest| rest.starts_with(char::is_whitespace)))
}

fn block_keyword(t: &str) -> Option<(&'static str, String)> {
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

/// Only what is between pipes (`good/extra_table_content.feature`); a cut row yields fewer cells
/// and fails the width check. `\|` is an escaped pipe.
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
    out
}

fn placeholders(text: &str) -> Vec<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            // Stop at the next `<` too, so `a < b and <c>` finds `c`.
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

fn language_of(t: &str) -> Option<String> {
    let rest = t.strip_prefix('#')?;
    let rest = rest.trim_start().strip_prefix("language")?;
    let code = rest.trim_start().strip_prefix(':')?;
    Some(code.trim().to_string())
}

pub fn validate(source: &str) -> Vec<Problem> {
    walk(source).1
}

/// Warnings do not stop it (cucumber accepts those files); one walk, so parser and validator agree.
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

    // Scanning the leading comments is more forgiving than the spec's first-line rule.
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
                return (
                    doc,
                    vec![Problem {
                        line: i as u32 + 1,
                        column: indent_col(raw),
                        kind: Kind::InvalidLanguage(code),
                    }],
                );
            }
            // Declined with one problem rather than a page of missing-English-keyword noise.
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
    // Per `Feature:` or `Rule:`: a Rule may have its own Background.
    let mut backgrounds = 0usize;
    let mut scenarios = 0usize;
    let mut block: Option<Block> = None;
    let mut table = Table::None;
    let mut docstring: Option<(&str, u32)> = None;
    // Dangling at EOF is a defect (`bad/unexpected_eof.feature`).
    let mut pending_tags: Option<(u32, u32)> = None;
    let mut tag_names: Vec<String> = Vec::new();

    for (idx, raw) in lines.iter().enumerate() {
        let line = idx as u32 + 1;
        let col = indent_col(raw);
        let t = raw.trim_start();
        let trimmed = raw.trim();

        if let Some((fence, _)) = docstring {
            if trimmed.starts_with(fence) {
                docstring = None;
            } else if let Some(b) = block.as_mut() {
                for (name, at) in placeholders(raw) {
                    b.placeholders.push((name, line, at as u32 + 1));
                }
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
            for word in t.split_whitespace() {
                // A `#` starts a comment only at the start of a token (`@comment_tag#2`).
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
                            _ => {
                                if let Some(step) = b.parsed.steps.last_mut() {
                                    step.argument.push(raw.trim().to_string());
                                }
                            }
                        }
                    }
                    // Only a data table can carry a placeholder; an `Examples` cell is the substituted value.
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
                    let kind =
                        if keyword == "outline" { BlockKind::Outline } else { BlockKind::Scenario };
                    let mut b = new_block(kind, title, line, col);
                    b.parsed.tags = std::mem::take(&mut tag_names);
                    block = Some(b);
                }
                // `Examples:` under a plain `Scenario:` promotes it to an outline (`good/scenario_outline.feature`).
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
                None => {
                    problems.push(Problem { line, column: col, kind: Kind::StepOutsideScenario })
                }
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
                    // Collected for plain scenarios too, since a later `Examples:` promotes them.
                    for (name, at) in placeholders(raw) {
                        b.placeholders.push((name, line, at as u32 + 1));
                    }
                }
            }
            continue;
        }

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

fn close_block(problems: &mut Vec<Problem>, doc: &mut Document, block: Option<Block>) {
    let Some(b) = block else { return };
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

// Gated on the target like `components/demo`: a cdylib with wit-bindgen exports does not link natively.

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
