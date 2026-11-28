//! omp harness knowledge: title states, transcript reading, dialog parsing.
//! Owner: tmux worker. Pure functions over strings and files, unit tested.

use crate::tmux;
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneState {
    /// Title `π ` + spinner frame.
    Working,
    /// Title `π >`.
    Waiting,
    /// Title `π !`.
    Asking,
    /// Not an omp pane.
    Other,
}

/// Classify an omp pane by its title. Anything not starting with `π ` is Other.
pub fn classify(title: &str) -> PaneState {
    if title.starts_with("π !") {
        PaneState::Asking
    } else if title.starts_with("π >") {
        PaneState::Waiting
    } else if title.starts_with("π ") {
        PaneState::Working
    } else {
        PaneState::Other
    }
}

/// The transcript named by the pane's omp breadcrumb, `~/.omp/agent/terminal-sessions/`
/// `pts-N` line 2 for the pane's tty `/dev/pts/N`. The crumb names a session, but a
/// new tmux server's panes reuse old pts numbers, so a pane can read a crumb left
/// by an earlier pane's pty. The crumb counts only when it could be this pane's:
/// its mtime is at or after the pty's ctime, the pty's age is unknown, or the pane's
/// omp was started with `--resume` or `--restore` naming this session. Otherwise the
/// pane keeps no transcript until omp writes a fresh crumb.
pub fn transcript_path(pane: &tmux::Pane) -> Option<PathBuf> {
    let number = pane.tty.strip_prefix("/dev/pts/")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let session_file = PathBuf::from(home)
        .join(".omp/agent/terminal-sessions")
        .join(format!("pts-{number}"));
    let contents = fs::read_to_string(&session_file).ok()?;
    let path = contents
        .lines()
        .nth(1)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)?;
    let crumb_ms = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&session_file)
                .ok()
                .map(|metadata| metadata.mtime() * 1000 + metadata.mtime_nsec() / 1_000_000)
        }
        #[cfg(not(unix))]
        {
            None
        }
    };
    let born = pane.pty_born_ms();
    (crumb_ms
        .zip(born)
        .is_none_or(|(crumb, born)| crumb >= born)
        || (pane.command == "omp"
            && pane
                .foreground_argv()
                .is_some_and(|argv| resumes(&argv, &path))))
    .then_some(path)
}

/// The session id a transcript's file name carries. It is the stem after the first `_`.
fn session_id(transcript: &Path) -> Option<&str> {
    transcript.file_stem()?.to_str()?.split_once('_').map(|(_, id)| id)
}

/// Whether argv starts omp on this transcript's session. `-r`, `--resume` and
/// `--restore` take the session as the next argument, `--resume=<v>` and
/// `--restore=<v>` carry it inline. A value names the session when it equals the
/// transcript path or is a prefix of the session id, the part of the file stem
/// after its first `_`. `-c`, `--continue` and a bare `--resume` do not name a
/// session: omp rewrites the crumb whenever it continues a different session than
/// the crumb names.
pub fn resumes(argv: &[String], transcript: &Path) -> bool {
    let session_id = session_id(transcript);
    let mut value_expected = false;
    for argument in argv.iter().skip(1) {
        if value_expected {
            value_expected = false;
            if !argument.is_empty()
                && (Path::new(argument) == transcript
                    || session_id.is_some_and(|id| id.starts_with(argument.as_str())))
            {
                return true;
            }
            continue;
        }
        let value = match argument.strip_prefix("--") {
            Some(rest) => match rest.split_once('=') {
                Some((flag, value)) => {
                    if flag != "resume" && flag != "restore" {
                        continue;
                    }
                    value
                }
                None => {
                    value_expected = rest == "resume" || rest == "restore";
                    continue;
                }
            },
            None => match argument.strip_prefix('-') {
                Some("r") => {
                    value_expected = true;
                    continue;
                }
                Some(_) => continue,
                None => continue,
            },
        };
        if !value.is_empty()
            && (Path::new(value) == transcript
                || session_id.is_some_and(|id| id.starts_with(value)))
        {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AskOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    /// What the form says around the question, as Markdown, the way omp's own dialog shows it
    /// above the question. Legacy asks have none.
    #[serde(default)]
    pub context: String,
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub recommended: Option<usize>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub header: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCall {
    pub tool_call_id: String,
    pub questions: Vec<Question>,
    /// The HTML form behind `questions`, when omp asked with `html`.
    pub form: Option<HtmlAskForm>,
}

/// An HTML ask's form. `intro` is the text before its first field outside any fieldset, and
/// `controls` holds one control per question, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlAskForm {
    pub intro: String,
    pub controls: Vec<HtmlControl>,
}

/// How a question's control takes an answer. Radio and select options are picked by row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlControl {
    /// `enabled` counts the pickable authored options; the Other row, when `other`, follows them.
    Radio { enabled: usize, leave_unselected: bool, other: bool },
    /// A single select; `enabled` counts its pickable authored options.
    Select { enabled: usize, leave_unselected: bool },
    /// A lone checkbox: option 0 is its current state, option 1 the other state.
    Checkbox,
    /// A text input or textarea holding `initial_lines` lines.
    Text { multiline: bool, initial_lines: usize },
    /// Read-only; the card says to answer in the pane.
    Unsupported,
}

impl HtmlControl {
    /// Whether the card may take custom text for this control.
    pub fn allows_other(&self) -> bool {
        matches!(self, Self::Radio { other: true, .. } | Self::Text { .. })
    }
}

/// Parse a `selector` literal; the callers pass only constant CSS.
fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("constant CSS selector")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Propose {
    pub tool_call_id: String,
    pub title: String,
    pub plan_path: PathBuf,
}

/// The last assistant `ask` tool call in the transcript that has no matching
/// toolResult yet. Transcript lines are JSON: `{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":..,"name":"ask","arguments":{"questions":[..]}}]}}`
/// and results are `{"type":"message","message":{"role":"toolResult","toolCallId":..}}`.
pub fn pending_ask(transcript: &std::path::Path) -> std::io::Result<Option<AskCall>> {
    let mut calls = Vec::new();
    let mut results = HashSet::new();
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(content) = message.get("content").and_then(Value::as_array) else {
                    return true;
                };
                for item in content {
                    if item.get("type").and_then(Value::as_str) != Some("toolCall")
                        || item.get("name").and_then(Value::as_str) != Some("ask")
                    {
                        continue;
                    }
                    let (Some(id), Some(arguments)) = (
                        item.get("id").and_then(Value::as_str),
                        item.get("arguments"),
                    ) else {
                        continue;
                    };
                    // omp 18.18+ asks with an HTML form; older omp asks with `questions`.
                    let parsed = arguments
                        .get("html")
                        .and_then(Value::as_str)
                        .and_then(parse_html_ask)
                        .map(|(questions, form)| (questions, Some(form)))
                        .or_else(|| {
                            let questions = arguments.get("questions")?.clone();
                            let questions = serde_json::from_value::<Vec<Question>>(questions).ok()?;
                            Some((questions, None))
                        });
                    let Some((questions, form)) = parsed else {
                        continue;
                    };
                    calls.push(AskCall {
                        tool_call_id: id.to_owned(),
                        questions,
                        form,
                    });
                }
            }
            Some("toolResult") => {
                if let Some(id) = message.get("toolCallId").and_then(Value::as_str) {
                    results.insert(id.to_owned());
                }
            }
            _ => {}
        }
        true
    })?;
    Ok(calls
        .into_iter()
        .rev()
        .find(|call| !results.contains(&call.tool_call_id)))
}

/// Parse omp's native HTML ask (`ask.arguments.html`) from its first `<form>`, or from the whole
/// document. One question and control per field, in document order. Radios and checkboxes group
/// by `(type, name)`. Each question takes its text and context the way omp's own step-by-step
/// dialog names them. None when no control is named, and the caller then falls back to `questions`.
pub fn parse_html_ask(html: &str) -> Option<(Vec<Question>, HtmlAskForm)> {
    let document = Html::parse_document(html);
    let root = document
        .select(&selector("form"))
        .next()
        .unwrap_or_else(|| document.root_element());
    let labels: HashMap<String, String> = root
        .select(&selector("label[for]"))
        .filter_map(|label| Some((label.value().attr("for")?.to_owned(), squash(label.text()))))
        .collect();
    let mut fields: Vec<HtmlField> = Vec::new();
    for node in root.descendants() {
        let Some(element) = ElementRef::wrap(node) else {
            continue;
        };
        match element.value().name() {
            "input" | "select" | "textarea" => {
                let kind = control_kind(element);
                if matches!(kind.as_str(), "hidden" | "button" | "file" | "image" | "reset" | "submit") {
                    continue;
                }
                let name = element.value().attr("name").unwrap_or_default();
                let group = (matches!(kind.as_str(), "radio" | "checkbox") && !name.is_empty())
                    .then(|| (kind, name.to_owned()));
                let position = group
                    .as_ref()
                    .and_then(|_| fields.iter().position(|field| field.group == group));
                match position {
                    Some(index) => fields[index].members.push(element),
                    None => fields.push(HtmlField {
                        group,
                        members: vec![element],
                    }),
                }
            }
            _ => {}
        }
    }
    if !fields.iter().any(|field| field.members[0].value().attr("name").is_some_and(|name| !name.is_empty())) {
        return None;
    }
    let shape = form_shape(root, &fields, &labels);
    let (questions, controls): (Vec<Question>, Vec<HtmlControl>) = fields
        .iter()
        .enumerate()
        .map(|(index, field)| html_question(index, field, &labels, &shape))
        .unzip();
    Some((questions, HtmlAskForm { intro: shape.intro, controls }))
}

/// A control, or a radio/checkbox group, in document order.
struct HtmlField<'a> {
    /// `(type, name)` of a named radio or checkbox group.
    group: Option<(String, String)>,
    members: Vec<ElementRef<'a>>,
}

/// The input type, or the tag for `select` and `textarea`, lowercased.
fn control_kind(control: ElementRef) -> String {
    match control.value().name() {
        "input" => control.value().attr("type").unwrap_or("text").to_ascii_lowercase(),
        tag => tag.to_owned(),
    }
}

/// Text with whitespace runs collapsed to single spaces.
fn squash<'a>(text: impl Iterator<Item = &'a str>) -> String {
    text.flat_map(|part| part.split_whitespace()).collect::<Vec<_>>().join(" ")
}

/// The closest ancestor element named `tag`.
fn nearest<'a>(element: ElementRef<'a>, tag: &str) -> Option<ElementRef<'a>> {
    element
        .ancestors()
        .find_map(|node| ElementRef::wrap(node).filter(|ancestor| ancestor.value().name() == tag))
}

/// The text a `<label>` wraps around `control`. The text of the controls inside it stays out.
fn wrapping_label(control: ElementRef) -> Option<String> {
    nearest(control, "label")
        .map(|label| squash_str(&inline(label).0))
        .filter(|text| !text.is_empty())
}

/// One line of text with whitespace runs collapsed to single spaces.
fn squash_str(text: &str) -> String {
    squash(std::iter::once(text))
}

/// Text of the `<label for>` naming `control`'s id.
fn label_for(control: ElementRef, labels: &HashMap<String, String>) -> Option<String> {
    control
        .value()
        .attr("id")
        .and_then(|id| labels.get(id))
        .filter(|text| !text.is_empty())
        .cloned()
}

/// A radio's label: its wrapping `<label>`, then its `<label for>`, then its value.
fn option_label(option: ElementRef, labels: &HashMap<String, String>) -> String {
    wrapping_label(option)
        .or_else(|| label_for(option, labels))
        .or_else(|| option.value().attr("value").map(str::to_owned))
        .unwrap_or_default()
}

/// Labels, recommended index, enabled count, and leave flag of a radio group or select. An optional
/// choice with nothing selected leads with `Leave unselected`. None when no option is enabled.
fn choice_options(
    authored: &[(String, bool, bool)],
    required: bool,
) -> Option<(Vec<String>, Option<usize>, usize, bool)> {
    let enabled: Vec<&(String, bool, bool)> = authored.iter().filter(|(_, _, disabled)| !disabled).collect();
    if enabled.is_empty() {
        return None;
    }
    let leave_unselected = !required && !authored.iter().any(|(_, checked, disabled)| *checked && !disabled);
    let mut options = Vec::new();
    let mut recommended: Option<usize> = None;
    if leave_unselected {
        options.push("Leave unselected".to_owned());
        recommended = Some(0);
    }
    for (label, checked, _) in &enabled {
        options.push(label.clone());
        if *checked {
            recommended = Some(options.len() - 1);
        }
    }
    Some((options, recommended, enabled.len(), leave_unselected))
}

/// Options, recommended index, multi flag, and control of one field. A field that cannot be
/// answered in place is `Unsupported` with no options.
fn html_control(
    kind: &str,
    members: &[ElementRef],
    labels: &HashMap<String, String>,
) -> (Vec<String>, Option<usize>, bool, HtmlControl) {
    let control = members[0];
    let required = members.iter().any(|member| member.value().attr("required").is_some());
    match kind {
        "radio" => {
            let authored: Vec<(String, bool, bool)> = members
                .iter()
                .map(|member| {
                    (
                        option_label(*member, labels),
                        member.value().attr("checked").is_some(),
                        member.value().attr("disabled").is_some(),
                    )
                })
                .collect();
            match choice_options(&authored, required) {
                Some((options, recommended, enabled, leave_unselected)) => {
                    let other = authored.len() >= 2
                        && !authored
                            .iter()
                            .any(|(label, _, _)| label.trim().to_ascii_lowercase().starts_with("other"));
                    (options, recommended, false, HtmlControl::Radio { enabled, leave_unselected, other })
                }
                None => (Vec::new(), None, false, HtmlControl::Unsupported),
            }
        }
        "checkbox" => {
            if members.len() > 1 || control.value().attr("disabled").is_some() {
                return (Vec::new(), None, members.len() > 1, HtmlControl::Unsupported);
            }
            let options = if control.value().attr("checked").is_some() {
                vec!["Checked".to_owned(), "Unchecked".to_owned()]
            } else {
                vec!["Unchecked".to_owned(), "Checked".to_owned()]
            };
            (options, Some(0), false, HtmlControl::Checkbox)
        }
        "select" => {
            if control.value().attr("multiple").is_some() {
                return (Vec::new(), None, true, HtmlControl::Unsupported);
            }
            let authored: Vec<(String, bool, bool)> = control
                .select(&selector("option"))
                .map(|option| {
                    let text = squash(option.text());
                    let label = if text.is_empty() {
                        option.value().attr("value").unwrap_or_default().to_owned()
                    } else {
                        text
                    };
                    (
                        label,
                        option.value().attr("selected").is_some(),
                        option.value().attr("disabled").is_some(),
                    )
                })
                .collect();
            match choice_options(&authored, required) {
                Some((options, recommended, enabled, leave_unselected)) => {
                    (options, recommended, false, HtmlControl::Select { enabled, leave_unselected })
                }
                None => (Vec::new(), None, false, HtmlControl::Unsupported),
            }
        }
        _ => {
            let multiline = kind == "textarea";
            let value = if multiline {
                let raw: String = control.text().collect();
                raw.strip_prefix("\r\n").or_else(|| raw.strip_prefix('\n')).unwrap_or(raw.as_str()).to_owned()
            } else {
                control.value().attr("value").unwrap_or_default().to_owned()
            };
            let initial_lines = if multiline { value.split('\n').count() } else { 1 };
            let (options, recommended) = if !value.trim().is_empty() {
                (vec!["Keep current value".to_owned()], Some(0))
            } else if !required {
                (vec!["Leave blank".to_owned()], Some(0))
            } else {
                (Vec::new(), None)
            };
            (options, recommended, false, HtmlControl::Text { multiline, initial_lines })
        }
    }
}

/// One question for a field, its text and context read the way omp's own dialog reads them.
fn html_question(
    index: usize,
    field: &HtmlField,
    labels: &HashMap<String, String>,
    shape: &FormShape,
) -> (Question, HtmlControl) {
    let control = field.members[0];
    let kind = control_kind(control);
    let name = control.value().attr("name").unwrap_or_default().to_owned();
    let id = if name.is_empty() { format!("field-{}", index + 1) } else { name.clone() };
    let (options, recommended, multi, answering) = if name.is_empty() {
        (Vec::new(), None, false, HtmlControl::Unsupported)
    } else {
        html_control(&kind, &field.members, labels)
    };
    let question = Question {
        id,
        question: shape.questions[index].clone(),
        context: shape.contexts[index].clone(),
        options: options
            .into_iter()
            .map(|label| AskOption {
                label,
                description: None,
            })
            .collect(),
        recommended,
        multi,
        header: shape.headers[index].clone(),
    };
    (question, answering)
}

/// Tags omp's form reader drops with everything inside them.
const SKIPPED_TAGS: &[&str] =
    &["head", "iframe", "link", "meta", "noscript", "script", "style", "template", "title"];

/// Tags that start and end a block of text, as omp's form reader splits a form.
const BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "blockquote", "dd", "details", "div", "dl", "dt", "figure",
    "figcaption", "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "legend", "li", "main",
    "nav", "p", "section", "summary", "tr",
];

/// One block of an ask form, as omp's form reader splits the form.
enum Block {
    /// Paragraph text, as plain text and as Markdown.
    Text { plain: String, markdown: String },
    /// A `<pre>`, list or table as Markdown. It never names a field.
    Rich(String),
    /// Field `index` at its first control. Blocks before `options_end` sit between its options.
    Field { index: usize, options_end: usize },
}

/// The blocks under one top-level fieldset, or between two of them.
struct Section {
    title: String,
    fieldset: bool,
    blocks: Vec<Block>,
}

/// Walks an ask form in document order and splits it into sections and blocks.
struct FormReader<'a, 'f> {
    fields: &'f [HtmlField<'a>],
    /// `<label for>` elements that name a control. Their text labels that control alone.
    naming: Vec<ElementRef<'a>>,
    sections: Vec<Section>,
    /// The section and block of each field's first control.
    placed: Vec<Option<(usize, usize)>>,
    plain: String,
    markdown: String,
    depth: usize,
}

impl<'a> FormReader<'a, '_> {
    fn section(&mut self) -> &mut Section {
        self.sections.last_mut().expect("a reader always holds a section")
    }

    fn push(&mut self, text: &str) {
        self.push_parts(text, text);
    }

    fn push_parts(&mut self, plain: &str, markdown: &str) {
        self.plain.push_str(plain);
        self.markdown.push_str(markdown);
    }

    fn flush(&mut self) {
        let plain = normalize(&std::mem::take(&mut self.plain));
        let markdown = normalize(&std::mem::take(&mut self.markdown));
        if !plain.is_empty() {
            self.section().blocks.push(Block::Text { plain, markdown });
        }
    }

    fn rich(&mut self, markdown: String) {
        self.section().blocks.push(Block::Rich(markdown));
    }

    fn walk(&mut self, element: ElementRef<'a>, skip: Option<ElementRef<'a>>) {
        for node in element.children() {
            match node.value() {
                Node::Text(text) => self.push(&spaced(&text.text)),
                _ => {
                    if let Some(child) = ElementRef::wrap(node)
                        && Some(child) != skip
                    {
                        self.visit(child);
                    }
                }
            }
        }
    }

    fn visit(&mut self, element: ElementRef<'a>) {
        let name = element.value().name();
        if SKIPPED_TAGS.contains(&name) {
            return;
        }
        match name {
            "br" => self.push("\n"),
            "hr" => self.flush(),
            "img" => self.push(element.value().attr("alt").unwrap_or_default()),
            "input" | "select" | "textarea" => self.control(element),
            "label" => self.label(element),
            "fieldset" => self.fieldset(element),
            "pre" => {
                self.flush();
                let text = element.text().collect::<String>();
                let text = text.strip_prefix('\n').unwrap_or(&text).trim_end();
                if !text.trim().is_empty() {
                    self.rich(fenced(text));
                }
            }
            "ul" | "ol" => self.rich_block(element, || list_markdown(element, name == "ol")),
            "table" => self.rich_block(element, || table_markdown(element)),
            "td" | "th" => {
                self.push(" ");
                self.walk(element, None);
                self.push(" ");
            }
            _ if BLOCK_TAGS.contains(&name) => {
                self.flush();
                self.walk(element, None);
                self.flush();
            }
            _ => match mark(name) {
                Some(mark) => {
                    let (plain, markdown) = marked(element, mark);
                    self.push_parts(&plain, &markdown);
                }
                None => self.walk(element, None),
            },
        }
    }

    /// A list or table as one rich block. A list or table with controls in it reads as text.
    fn rich_block(&mut self, element: ElementRef<'a>, markdown: impl FnOnce() -> String) {
        self.flush();
        if has_control(element) {
            self.walk(element, None);
            self.flush();
            return;
        }
        let markdown = markdown();
        if !markdown.is_empty() {
            self.rich(markdown);
        }
    }

    fn control(&mut self, control: ElementRef<'a>) {
        let Some(index) = self.fields.iter().position(|field| field.members.contains(&control)) else {
            return;
        };
        self.flush();
        let at = self.sections.len() - 1;
        let count = self.sections[at].blocks.len();
        match self.placed[index] {
            None => {
                self.placed[index] = Some((at, count));
                self.sections[at].blocks.push(Block::Field { index, options_end: 0 });
            }
            Some((section, block)) => {
                if section == at
                    && block + 1 != count
                    && let Some(Block::Field { options_end, .. }) = self.sections[at].blocks.get_mut(block)
                {
                    *options_end = count;
                }
            }
        }
    }

    fn label(&mut self, label: ElementRef<'a>) {
        if self.naming.contains(&label) {
            return;
        }
        if !has_control(label) {
            self.walk(label, None);
            return;
        }
        self.flush();
        let controls: Vec<ElementRef<'a>> = label.descendent_elements().filter(|child| is_control(*child)).collect();
        for control in controls {
            self.control(control);
        }
    }

    fn fieldset(&mut self, fieldset: ElementRef<'a>) {
        self.flush();
        let legend = fieldset.child_elements().find(|child| child.value().name() == "legend");
        if self.depth == 0 {
            let title = legend.map(|legend| inline(legend).0).unwrap_or_default();
            self.open_section(title, true);
        }
        self.depth += 1;
        self.walk(fieldset, if self.depth == 1 { legend } else { None });
        self.flush();
        self.depth -= 1;
        if self.depth == 0 {
            self.open_section(String::new(), false);
        }
    }

    fn open_section(&mut self, title: String, fieldset: bool) {
        let current = self.section();
        if current.blocks.is_empty() && current.title.is_empty() {
            current.title = title;
            current.fieldset = fieldset;
            return;
        }
        self.sections.push(Section {
            title,
            fieldset,
            blocks: Vec::new(),
        });
    }
}

/// Whether `element` is an input, select or textarea.
fn is_control(element: ElementRef) -> bool {
    matches!(element.value().name(), "input" | "select" | "textarea")
}

/// Whether a control sits inside `element`.
fn has_control(element: ElementRef) -> bool {
    element.descendent_elements().any(is_control)
}

/// Source whitespace, line breaks included, as single spaces. Only `<br>` breaks a line.
fn spaced(text: &str) -> String {
    text.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect()
}

/// Text with each line's whitespace runs made one space, and blank ends trimmed.
fn normalize(text: &str) -> String {
    text.split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// The Markdown mark omp's form reader styles an inline tag with.
fn mark(tag: &str) -> Option<&'static str> {
    match tag {
        "b" | "strong" => Some("**"),
        "i" | "em" => Some("*"),
        "code" | "kbd" | "samp" | "tt" => Some("`"),
        _ => None,
    }
}

/// An inline element's plain text, and its Markdown wrapped in its mark.
fn marked(element: ElementRef, mark: &str) -> (String, String) {
    let (plain, markdown) = inline(element);
    if plain.is_empty() {
        return (plain, String::new());
    }
    if mark == "`" {
        let fence = if plain.contains('`') { "``" } else { "`" };
        let pad = if plain.contains('`') { " " } else { "" };
        return (plain, format!("{fence}{pad}{markdown}{pad}{fence}"));
    }
    (plain, format!("{mark}{markdown}{mark}"))
}

/// The plain text and Markdown of an element's content. Controls and their text stay out.
fn inline(element: ElementRef) -> (String, String) {
    let mut plain = String::new();
    let mut markdown = String::new();
    inline_into(element, &mut plain, &mut markdown);
    (normalize(&plain), normalize(&markdown))
}

fn inline_into(element: ElementRef, plain: &mut String, markdown: &mut String) {
    for node in element.children() {
        let Some(child) = ElementRef::wrap(node) else {
            if let Node::Text(text) = node.value() {
                plain.push_str(&spaced(&text.text));
                markdown.push_str(&spaced(&text.text));
            }
            continue;
        };
        let name = child.value().name();
        if SKIPPED_TAGS.contains(&name) || is_control(child) || name == "svg" {
            continue;
        }
        match name {
            "br" => {
                plain.push('\n');
                markdown.push('\n');
            }
            "img" => {
                let alt = child.value().attr("alt").unwrap_or_default();
                plain.push_str(alt);
                markdown.push_str(alt);
            }
            _ if BLOCK_TAGS.contains(&name) || name == "td" || name == "th" => {
                plain.push(' ');
                markdown.push(' ');
                inline_into(child, plain, markdown);
                plain.push(' ');
                markdown.push(' ');
            }
            _ => match mark(name) {
                Some(mark) => {
                    let (child_plain, child_markdown) = marked(child, mark);
                    plain.push_str(&child_plain);
                    markdown.push_str(&child_markdown);
                }
                None => inline_into(child, plain, markdown),
            },
        }
    }
}

/// A `<pre>` block's text as a fenced code block, fenced one backtick longer than the longest run inside.
fn fenced(text: &str) -> String {
    let mut fence = String::from("```");
    while text.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}\n{text}\n{fence}")
}

/// A list's `<li>` children as Markdown list lines.
fn list_markdown(list: ElementRef, ordered: bool) -> String {
    list.child_elements()
        .filter(|item| item.value().name() == "li")
        .filter_map(|item| {
            let text = inline(item).1.replace('\n', " ");
            let text = text.trim().to_owned();
            (!text.is_empty()).then_some(text)
        })
        .enumerate()
        .map(|(index, text)| match ordered {
            true => format!("{}. {text}", index + 1),
            false => format!("- {text}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A table's own rows as a Markdown table whose first row is its header.
fn table_markdown(table: ElementRef) -> String {
    let rows: Vec<Vec<String>> = table
        .descendent_elements()
        .filter(|row| row.value().name() == "tr" && nearest(*row, "table") == Some(table))
        .map(|row| {
            row.child_elements()
                .filter(|cell| matches!(cell.value().name(), "td" | "th"))
                .map(|cell| inline(cell).1.replace('\n', " ").trim().replace('|', "\\|"))
                .collect()
        })
        .filter(|row: &Vec<String>| row.iter().any(|cell| !cell.is_empty()))
        .collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let line = |row: &Vec<String>| {
        let mut cells = row.clone();
        cells.resize(width, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(line(&rows[0]));
    lines.push(format!("|{}", " --- |".repeat(width)));
    lines.extend(rows.iter().skip(1).map(line));
    lines.join("\n")
}

/// The label omp gives a field of its own. A radio or checkbox label names an option instead.
/// A text field without one takes its name. `_` and `-` there read as spaces.
fn field_label(field: &HtmlField, labels: &HashMap<String, String>) -> String {
    let control = field.members[0];
    let kind = control_kind(control);
    if kind == "radio" || kind == "checkbox" {
        return String::new();
    }
    let own = wrapping_label(control)
        .or_else(|| label_for(control, labels))
        .or_else(|| {
            control
                .value()
                .attr("aria-label")
                .or_else(|| control.value().attr("title"))
                .map(squash_str)
        })
        .unwrap_or_default();
    if !own.trim().is_empty() {
        return own;
    }
    if kind == "select" {
        return String::new();
    }
    control
        .value()
        .attr("name")
        .unwrap_or_default()
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The legend of the fieldset nearest `control`.
fn legend_of(control: ElementRef) -> Option<String> {
    nearest(control, "fieldset")
        .and_then(|fieldset| fieldset.child_elements().find(|child| child.value().name() == "legend"))
        .map(|legend| squash_str(&inline(legend).0))
        .filter(|text| !text.is_empty())
}

/// Whether two texts read the same, ignoring whitespace runs and ASCII case.
fn same_text(left: &str, right: &str) -> bool {
    squash_str(left).eq_ignore_ascii_case(&squash_str(right))
}

/// The question text, context and legend omp's own form dialog gives each field, and the form's intro.
struct FormShape {
    questions: Vec<String>,
    contexts: Vec<String>,
    headers: Vec<Option<String>>,
    intro: String,
}

/// Read `root` the way omp's form dialog does. A field's question is its own label, then the last
/// text before it, then its fieldset legend, then its only option's label, then its name. Its
/// context is everything written since the question before it. The intro is the form's text
/// before the first field outside any fieldset.
fn form_shape(root: ElementRef, fields: &[HtmlField], labels: &HashMap<String, String>) -> FormShape {
    let ids: Vec<Option<&str>> = fields
        .iter()
        .map(|field| field.members[0].value().attr("id"))
        .collect();
    let mut naming: Vec<ElementRef> = Vec::new();
    for label in root.descendent_elements().filter(|element| element.value().name() == "label") {
        let Some(target) = label.value().attr("for") else {
            continue;
        };
        let named = ids.contains(&Some(target));
        let kept = naming.iter().any(|kept| kept.value().attr("for") == Some(target));
        if named && !kept {
            naming.push(label);
        }
    }
    let mut reader = FormReader {
        fields,
        naming,
        sections: vec![Section {
            title: String::new(),
            fieldset: false,
            blocks: Vec::new(),
        }],
        placed: vec![None; fields.len()],
        plain: String::new(),
        markdown: String::new(),
        depth: 0,
    };
    reader.walk(root, None);
    reader.flush();
    let names: Vec<String> = fields
        .iter()
        .map(|field| field.members[0].value().attr("name").unwrap_or_default().to_owned())
        .collect();
    let own: Vec<String> = fields.iter().map(|field| field_label(field, labels)).collect();
    let only: Vec<String> = fields
        .iter()
        .map(|field| {
            let kind = control_kind(field.members[0]);
            if matches!(kind.as_str(), "radio" | "checkbox") && field.members.len() == 1 {
                option_label(field.members[0], labels)
            } else {
                String::new()
            }
        })
        .collect();
    let pick = |index: usize, previous: &str, title: &str| match [
        own[index].as_str(),
        previous,
        title,
        only[index].as_str(),
        names[index].as_str(),
    ]
    .into_iter()
    .find(|text| !text.trim().is_empty())
    {
        Some(text) => squash_str(text),
        None => String::new(),
    };
    let mut questions = vec![String::new(); fields.len()];
    for section in &reader.sections {
        let mut previous = String::new();
        let mut until = 0;
        for (at, block) in section.blocks.iter().enumerate() {
            match block {
                Block::Field { index, options_end } => {
                    questions[*index] = pick(*index, &previous, &section.title);
                    previous.clear();
                    until = (*options_end).max(at + 1).min(section.blocks.len());
                }
                // Text between a field's options describes that field alone.
                Block::Text { plain, .. } => {
                    if at >= until {
                        previous.clone_from(plain);
                    }
                }
                Block::Rich(_) => {
                    if at >= until {
                        previous.clear();
                    }
                }
            }
        }
    }
    let headers: Vec<Option<String>> = fields.iter().map(|field| legend_of(field.members[0])).collect();
    let mut contexts = vec![String::new(); fields.len()];
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut intro: Vec<(String, String)> = Vec::new();
    let mut last: Option<usize> = None;
    for section in &reader.sections {
        if !section.title.trim().is_empty() {
            pending.push((section.title.clone(), section.title.clone()));
        }
        let mut open: Option<usize> = None;
        let mut until = 0;
        for (at, block) in section.blocks.iter().enumerate() {
            let rich = matches!(block, Block::Rich(_));
            let (plain, markdown) = match block {
                Block::Field { index, options_end } => {
                    if names[*index].is_empty() {
                        continue;
                    }
                    let own_text = |text: &str| {
                        same_text(text, &questions[*index])
                            || headers[*index].as_deref().is_some_and(|header| same_text(text, header))
                    };
                    if last.is_none() {
                        intro.retain(|(plain, _)| !own_text(plain));
                    }
                    let mut context = String::new();
                    for (_, markdown) in pending.drain(..).filter(|(plain, _)| !own_text(plain)) {
                        append_block(&mut context, &markdown);
                    }
                    contexts[*index] = context;
                    last = Some(*index);
                    open = Some(*index);
                    let end = (*options_end).max(at + 1).min(section.blocks.len());
                    let nested = section.blocks[at + 1..end].iter().any(
                        |block| matches!(block, Block::Field { index, .. } if !names[*index].is_empty()),
                    );
                    until = if nested { 0 } else { end };
                    continue;
                }
                Block::Text { plain, markdown } => (plain, markdown),
                Block::Rich(markdown) => (markdown, markdown),
            };
            match open.filter(|_| at < until) {
                Some(index) => append_block(&mut contexts[index], markdown),
                None => {
                    if last.is_none() && !section.fieldset && !rich {
                        intro.push((plain.clone(), markdown.clone()));
                    }
                    pending.push((plain.clone(), markdown.clone()));
                }
            }
        }
    }
    if let Some(index) = last {
        for (_, markdown) in pending.drain(..) {
            append_block(&mut contexts[index], &markdown);
        }
    }
    let mut intro_text = String::new();
    for (_, markdown) in intro {
        append_block(&mut intro_text, &markdown);
    }
    FormShape {
        questions,
        contexts,
        headers,
        intro: intro_text,
    }
}

/// Add one block of Markdown to a context, one blank line after the last.
fn append_block(context: &mut String, markdown: &str) {
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str(markdown);
}


/// The title and plan file path a proposal's `inner` data names.
fn proposal_fields(inner: Option<&Value>) -> (Option<String>, Option<String>) {
    (
        inner
            .and_then(|inner| inner.get("title"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        inner
            .and_then(|inner| inner.get("planFilePath"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    )
}

/// Every object under `value` that names a `planFilePath`, as its title and plan file path.
fn collect_proposals(value: Option<&Value>, out: &mut Vec<(Option<String>, Option<String>)>) {
    match value {
        Some(Value::Object(map)) if map.get("planFilePath").is_some_and(Value::is_string) => {
            out.push(proposal_fields(value));
        }
        Some(Value::Object(map)) => map.values().for_each(|child| collect_proposals(Some(child), out)),
        Some(Value::Array(items)) => items.iter().for_each(|child| collect_proposals(Some(child), out)),
        _ => {}
    }
}

/// Read plan proposals and matching results from an omp JSONL transcript.
pub fn proposes(transcript: &Path) -> io::Result<Vec<Propose>> {
    let mut calls = Vec::new();
    let mut results = HashMap::new();
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(content) = message.get("content").and_then(Value::as_array) else {
                    return true;
                };
                for item in content {
                    if item.get("type").and_then(Value::as_str) != Some("toolCall")
                        || item.get("name").and_then(Value::as_str) != Some("write")
                        || item
                            .get("arguments")
                            .and_then(|args| args.get("path"))
                            .and_then(Value::as_str)
                            != Some("xd://propose")
                    {
                        continue;
                    }
                    let Some(id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let content = item
                        .get("arguments")
                        .and_then(|args| args.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    calls.push((id.to_owned(), content));
                }
            }
            Some("toolResult") => {
                let Some(id) = message.get("toolCallId").and_then(Value::as_str) else {
                    return true;
                };
                // omp has moved the proposal between tools and result shapes across releases.
                // Any object in the result that names a `planFilePath` is a proposal, wherever
                // it sits, so a tool or nesting change does not hide the plan.
                let mut found = Vec::new();
                collect_proposals(message.get("details"), &mut found);
                for (index, fields) in found.into_iter().enumerate() {
                    let key = if index == 0 { id.to_owned() } else { format!("{id}:{index}") };
                    if !calls.iter().any(|(call, _)| *call == key) {
                        calls.push((key.clone(), String::new()));
                    }
                    results.insert(key, fields);
                }
            }
            _ => {}
        }
        true
    })?;

    // omp stores a session's `local://` artifacts in the sibling directory named
    // like the transcript without `.jsonl`, so a plan file lives at
    // `<session dir>/local/<slug>-plan.md`.
    let session_directory = transcript.with_extension("");
    Ok(calls
        .into_iter()
        .map(|(tool_call_id, content)| {
            let result = results.get(&tool_call_id);
            let title = result
                .and_then(|(title, _)| title.as_deref())
                .unwrap_or(&content)
                .to_owned();
            let plan_path = result
                .and_then(|(_, plan_file_path)| plan_file_path.as_deref())
                .and_then(|plan_file_path| {
                    Path::new(plan_file_path.strip_prefix("local://").unwrap_or(plan_file_path))
                        .file_name()
                })
                .map(|basename| session_directory.join("local").join(basename))
                .unwrap_or_default();
            Propose {
                tool_call_id,
                title,
                plan_path,
            }
        })
        .collect())
}

/// Text of the last assistant message that has at least one text block, with the
/// entry id. Text blocks joined by blank lines.
pub fn last_assistant_text(transcript: &std::path::Path) -> std::io::Result<Option<(String, String)>> {
    let mut last = None;
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        if message.get("role").and_then(Value::as_str) == Some("assistant")
            && let (Some(id), Some(text)) = (
                value.get("id").and_then(Value::as_str),
                text_blocks(message),
            )
        {
            last = Some((id.to_owned(), text));
        }
        true
    })?;
    Ok(last)
}

/// How the transcript's last assistant message ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    /// `stopReason` `stop` or `length` with non-blank text. `report` is the latest
    /// earlier assistant text since the last user message whose first line is a
    /// status line, set only when `text` has none. `owed` is true while no reply since
    /// the session's latest brief, or since its start when it had none, has opened with a
    /// status line and carried a report.
    Reply { entry_id: String, text: String, report: Option<String>, owed: bool },
    /// `stopReason` `error`. `message` is `errorMessage`, empty when absent.
    Error { entry_id: String, message: String },
    /// `stopReason` `aborted`: an interrupt or a terminal tool ended the run.
    Aborted,
    /// `toolUse`, a missing or unknown stop reason, a stop without text, or a trailing
    /// delivered background-job result or supervised-process exit: the run is still going, or
    /// its last message is not on disk yet.
    Running,
}

/// Classify the transcript's last assistant message the way omp settles a run:
/// `error` is an error, `aborted` is idle, a `stop` with text is done. A trailing
/// delivered background-job result (`async-result`) or supervised-process exit
/// (`launch-completion`) wakes the run. It reads as `Running` until the resumed turn
/// writes its next message.
pub fn turn_end(transcript: &Path) -> io::Result<Option<TurnEnd>> {
    // (entry id, stopReason, errorMessage, non-blank text)
    type Last = (String, Option<String>, Option<String>, Option<String>);
    let mut last: Option<Last> = None;
    let mut report: Option<String> = None;
    let mut reported = false;
    let mut woken = false;
    scan_jsonl(transcript, |value| {
        if value.get("type").and_then(Value::as_str) == Some("custom_message") {
            if matches!(
                value.get("customType").and_then(Value::as_str),
                Some("async-result" | "launch-completion")
            ) {
                woken = true;
            }
            return true;
        }
        let Some(message) = message(&value) else {
            return true;
        };
        if hands_over_brief(message) {
            reported = false;
        }
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                report = None;
                woken = false;
            }
            Some("assistant") => {
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    return true;
                };
                let text = text_blocks(message).filter(|text| !text.trim().is_empty());
                if let Some(text) = text.as_deref()
                    && let Some((_, body)) = final_report(text)
                {
                    report = Some(text.to_owned());
                    reported |= !body.is_empty();
                }
                let field = |key| message.get(key).and_then(Value::as_str).map(str::to_owned);
                last = Some((id.to_owned(), field("stopReason"), field("errorMessage"), text));
                woken = false;
            }
            _ => {}
        }
        true
    })?;
    let Some((entry_id, stop, error, text)) = last else {
        return Ok(None);
    };
    Ok(Some(if woken {
        TurnEnd::Running
    } else {
        match (stop.as_deref(), text) {
            (Some("stop" | "length"), Some(text)) => TurnEnd::Reply {
                report: report.filter(|_| final_report(&text).is_none()),
                entry_id,
                text,
                owed: !reported,
            },
            (Some("error"), _) => TurnEnd::Error {
                entry_id,
                message: error.unwrap_or_default(),
            },
            (Some("aborted"), _) => TurnEnd::Aborted,
            _ => TurnEnd::Running,
        }
    }))
}

/// A running supervised process holds a reply card for at most this long, counted from its start.
/// Past that, a keepalive loop or other process meant to outlive the run cannot hide replies for good.
const SERVICE_HOLD_MS: u64 = 2 * 60 * 60 * 1000;
/// How long after a supervised process exits its completion may still be on its way to
/// the session. omp appends it within seconds, whether the session is idle or mid-turn.
const COMPLETION_DELIVERY_MS: u64 = 60 * 1000;

/// Whether omp will start another turn for the session behind `transcript` when one of
/// its supervised processes (`bash` with `name`) exits. omp settles the run while such a
/// process runs, then reports the exit as a new turn. That covers a process the session
/// owns, started without a readiness gate and with completion events on, that either
/// runs (state `starting`, `running` or `restarting`, pid alive when known) and started
/// less than `SERVICE_HOLD_MS` ago, or exited less than `COMPLETION_DELIVERY_MS` ago with
/// its completion still pending. A process with a readiness gate is a server meant to
/// outlive the run and never counts.
pub fn service_wake_pending(transcript: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    // shortcut: omp moves this root to $XDG_STATE_HOME/omp/run/daemons once that omp
    // directory exists, which this host lacks. Read that root if this host migrates.
    service_wake_pending_in(&PathBuf::from(home).join(".omp/run/daemons"), transcript, SystemTime::now())
}

fn service_wake_pending_in(root: &Path, transcript: &Path, now: SystemTime) -> bool {
    let Some(owner) = session_id(transcript) else {
        return false;
    };
    let now = now.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let read = |path: PathBuf| fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Ok(scopes) = fs::read_dir(root) else {
        return false;
    };
    scopes
        .flatten()
        .filter_map(|scope| fs::read_dir(scope.path().join("daemons")).ok())
        .flatten()
        .flatten()
        .any(|service| {
            let dir = service.path();
            let Some(meta) = read(dir.join("meta.json")) else {
                return false;
            };
            let daemon = &meta["daemon"];
            if daemon["owner"].as_str() != Some(owner) || meta["completionEvents"].as_bool() != Some(true) {
                return false;
            }
            if read(dir.join("spec.json")).is_none_or(|spec| !spec["ready"].is_null()) {
                return false;
            }
            let since = |key: &str| daemon[key].as_u64().map(|at| now.saturating_sub(at));
            let running = matches!(daemon["state"].as_str(), Some("starting" | "running" | "restarting"))
                && since("startedAt").is_some_and(|age| age < SERVICE_HOLD_MS)
                && daemon["pid"].as_u64().is_none_or(|pid| Path::new("/proc").join(pid.to_string()).exists());
            let undelivered = since("exitedAt").is_some_and(|age| age < COMPLETION_DELIVERY_MS)
                && meta["pendingCompletions"].as_array().is_some_and(|pending| !pending.is_empty());
            running || undelivered
        })
}

/// Whether the transcript's latest goal state is an active goal. Its last `mode_change` has
/// mode `goal` and goal status `active`. omp's TUI then starts a continuation turn 800 ms
/// after each settled turn, unless a gate it keeps only in memory (no progress, composer
/// text) skips it.
pub fn goal_active(transcript: &Path) -> io::Result<bool> {
    let mut active = false;
    scan_jsonl(transcript, |value| {
        if value.get("type").and_then(Value::as_str) == Some("mode_change") {
            active = value.get("mode").and_then(Value::as_str) == Some("goal")
                && value.pointer("/data/goal/status").and_then(Value::as_str) == Some("active");
        }
        true
    })?;
    Ok(active)
}

/// Options the toolResult of `tool_call_id` recorded as selected, if present.
pub fn ask_result(transcript: &std::path::Path, tool_call_id: &str) -> std::io::Result<Option<Vec<String>>> {
    let mut selected = None;
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        if message.get("role").and_then(Value::as_str) == Some("toolResult")
            && message.get("toolCallId").and_then(Value::as_str) == Some(tool_call_id)
        {
            let result_details = message.get("details");
            selected = result_details
                .and_then(|details| details.get("selectedOptions"))
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<String>>()
                })
                .or_else(|| {
                    // HTML asks answer with `details.answers[].answer`, in form order.
                    let answers = result_details?.get("answers")?.as_array()?;
                    Some(answers.iter().filter_map(|answer| answer.get("answer")?.as_str().map(str::to_owned)).collect::<Vec<String>>())
                });
        }
        true
    })?;
    Ok(selected)
}

/// Parse omp's Ask dialog on a captured screen. Rows look like `│   ○ Red`,
/// chosen rows `│   ◉ Green`, and the cursor row `│ ❯ ○ Green (Recommended)`;
/// description rows are indented without a marker. A question revisited in the
/// pane still parses because chosen rows count too.
/// Returns (cursor index, total rows incl. Other).
pub fn dialog_cursor(screen: &str) -> Option<(usize, usize)> {
    let mut in_dialog = false;
    let mut closed = false;
    let mut cursor = None;
    let mut total = 0;
    for line in screen.lines() {
        if !in_dialog && line.contains("╭─ Ask") {
            in_dialog = true;
            closed = false;
            cursor = None;
            total = 0;
            continue;
        }
        if !in_dialog {
            continue;
        }
        if line.contains('╰') {
            in_dialog = false;
            closed = true;
            continue;
        }
        if line.contains('○') || line.contains('◉') {
            if line.contains('❯') {
                cursor = Some(total);
            }
            total += 1;
        }
    }
    if closed && total > 0 {
        cursor.map(|cursor| (cursor, total))
    } else {
        None
    }
}

/// Parse omp's Ask review tab on a captured screen. The last `╭─ Ask` box must
/// close with a `╰` line and show `Review answers` before its first `├` divider;
/// numbered rows `│ 1. f1: unanswered` after the divider contribute the text
/// after `<digits>. ` (here `f1: unanswered`). Any other tab parses to None.
pub fn review_answers(screen: &str) -> Option<Vec<String>> {
    let mut in_dialog = false;
    let mut review = false;
    let mut divider = false;
    let mut closed = false;
    let mut rows = Vec::new();
    for line in screen.lines() {
        if line.contains("╭─ Ask") {
            in_dialog = true;
            review = false;
            divider = false;
            closed = false;
            rows.clear();
            continue;
        }
        if !in_dialog {
            continue;
        }
        if line.contains('╰') {
            in_dialog = false;
            closed = true;
            continue;
        }
        if line.contains('├') {
            divider = true;
            continue;
        }
        let content = line
            .strip_prefix('│')
            .map(|rest| rest.trim_end().strip_suffix('│').unwrap_or(rest).trim())
            .unwrap_or_default();
        if !divider {
            review |= content == "Review answers";
        } else if let Some((number, rest)) = content.split_once(". ")
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
        {
            rows.push(rest.to_owned());
        }
    }
    (review && closed).then_some(rows)
}

/// Detect the fullscreen Plan Review header in a captured screen.
pub fn plan_review_open(screen: &str) -> bool {
    screen.lines().any(|line| line.starts_with("╭─ Plan Review"))
}

/// A looser read of the review overlay than `plan_review_open`: a boxed header that names a plan
/// review, or a boxed row that starts an approval choice. It lets the tracker notice a review
/// that `plan_review_open` no longer recognizes after an omp update. Plain chat text does not
/// count, because the markers must open a line that starts with a box border.
pub fn looks_like_plan_review(screen: &str) -> bool {
    screen.lines().any(|line| {
        let line = line.trim_start().to_lowercase();
        let row = line.strip_prefix('│').map(|rest| rest.trim_start_matches([' ', '❯']));
        (line.starts_with('╭') && line.contains("plan review"))
            || row.is_some_and(|row| row.starts_with("approve and "))
    })
}

/// Read the Plan Review footer's option labels and the row the `❯` marks, in display order. The
/// rows sit between the two `├` rules of the open box; the title, slider and its model hint are
/// skipped. A header without its footer rows, or without a marked row, reads as none.
pub fn plan_review_options(screen: &str) -> Option<(Vec<String>, usize)> {
    let mut open = false;
    let mut footer = false;
    let mut rows: Vec<String> = Vec::new();
    let mut selected = None;
    for line in screen.lines() {
        if line.starts_with("╭─ Plan Review") {
            open = true;
            footer = false;
            rows.clear();
            selected = None;
            continue;
        }
        if !open {
            continue;
        }
        if line.starts_with('╰') {
            break;
        }
        if line.starts_with('├') {
            footer = !footer;
            continue;
        }
        if !footer {
            continue;
        }
        let content = line
            .strip_prefix('│')
            .map(|rest| rest.trim_end().strip_suffix('│').unwrap_or(rest).trim())
            .unwrap_or_default();
        if content.is_empty()
            || content.starts_with("Plan mode - next step")
            || content.starts_with("continue with")
            || content.starts_with('↳')
        {
            continue;
        }
        let label = match content.strip_prefix('❯') {
            Some(rest) => {
                selected = Some(rows.len());
                rest.trim()
            }
            None => content,
        };
        rows.push(label.to_owned());
    }
    selected.filter(|_| !rows.is_empty()).map(|selected| (rows, selected))
}

/// Reads the visible role track and selected index from Plan Review.
pub fn plan_execution_models(screen: &str) -> Option<(Vec<String>, usize)> {
    if !screen.lines().any(|line| line.contains("←/→ model")) {
        return None;
    }
    let line = screen.lines().find(|line| line.contains("continue with"))?;
    let track = line.split_once('◂')?.1.split_once('▸')?.0
        .replace('', "◀")
        .replace('', "▶")
        .replace('', "┆");
    let (before, remaining) = track.split_once('◀')?;
    let (selected, after) = remaining.split_once('▶')?;
    let split_roles = |part: &str| {
        part.split('┆')
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let mut roles = split_roles(before);
    let selected_index = roles.len();
    let selected = selected.trim();
    if selected.is_empty() {
        return None;
    }
    roles.push(selected.to_owned());
    roles.extend(split_roles(after));
    Some((roles, selected_index))
}


/// Detects plan mode from the idle-screen status row only.
pub fn plan_mode_on(screen: &str) -> bool {
    screen.lines().any(|line| line.contains("🗺 Plan"))
}

/// A launch whose omp came up without a model: its TUI printed omp's `No model selected.` error
/// and its status row shows `⬢ no-model`. Both must show, because the status row alone also
/// appears while omp is still binding its model at startup.
pub fn no_model(screen: &str) -> bool {
    screen.lines().any(|line| line.contains("No model selected."))
        && screen.lines().any(|line| line.contains("⬢ no-model"))
}

/// Whether omp's own log for `pid` records the `SQLITE_BUSY` startup crash. omp writes each
/// run to `~/.omp/logs/omp.<date>.<pid>.log`, so the pane's pid names the file without the
/// date. A crash there reads `"code":"SQLITE_BUSY"` with `database is locked` as its message.
pub fn startup_busy(pid: u32) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    busy_log(&PathBuf::from(home).join(".omp/logs"), pid)
}

/// `startup_busy` over an explicit log directory.
pub fn busy_log(dir: &Path, pid: u32) -> bool {
    let suffix = format!(".{pid}.log");
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        if !name.starts_with("omp.") || !name.ends_with(&suffix) {
            return false;
        }
        fs::read_to_string(entry.path()).is_ok_and(|log| {
            log.contains("SQLITE_BUSY") || log.contains("database is locked")
        })
    })
}

/// Line indexes of the last two lines containing `───`, the rules around omp's editor.
fn editor_rules(screen: &str) -> Option<(usize, usize)> {
    let mut previous = None;
    let mut last = None;
    for (index, line) in screen.lines().enumerate() {
        if line.contains("───") {
            previous = last;
            last = Some(index);
        }
    }
    Some((previous?, last?))
}

/// True when omp's editor (between the last two `───` rules) holds nothing but hints.
pub fn editor_empty(screen: &str) -> bool {
    let Some((start, end)) = editor_rules(screen) else {
        return false;
    };
    screen
        .lines()
        .skip(start + 1)
        .take(end.saturating_sub(start + 1))
        .all(|line| line.split("←←").all(|part| part.trim().is_empty()))
}

/// True when omp's working row, the last non-blank line above the editor's top
/// rule, shows a background-job wait or an auto-retry, so the run has not stopped.
pub fn run_paused(screen: &str) -> bool {
    let Some((top, _)) = editor_rules(screen) else {
        return false;
    };
    let Some(row) = screen
        .lines()
        .take(top)
        .filter(|line| !line.trim().is_empty())
        .last()
    else {
        return false;
    };
    // Drop the `⎋` hint and any spinner frame before the label.
    let row = row
        .split_whitespace()
        .skip_while(|token| !token.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>()
        .join(" ");
    let counted_jobs = row.strip_prefix("Waiting for ").is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with(" background jobs: ")
    });
    counted_jobs
        || [
            "Waiting for background work",
            "Waiting for background job ",
            "Retrying (",
            "Retrying · attempt ",
        ]
        .iter()
        .any(|prefix| row.starts_with(prefix))
}

/// Replace common secret shapes (AWS keys, bearer tokens, long hex/base64 runs) with `…`.
/// For a captured screen or a plan file, where paths and hashes matter less than a leak.
pub fn redact(text: &str) -> String {
    redact_with(text, true)
}

/// Replace only unambiguous credentials, AWS access keys and `Bearer` tokens, with `…`.
/// For card text that the user reads as a report, where a masked path or commit hash would
/// cost more than the long-run rule catches.
pub fn redact_credentials(text: &str) -> String {
    redact_with(text, false)
}

fn redact_with(text: &str, long_runs: bool) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if text[index..].starts_with("Bearer ") {
            let start = index + "Bearer ".len();
            let mut end = start;
            while end < bytes.len() && bearer_byte(bytes[end]) {
                end += 1;
            }
            if end > start {
                output.push_str("Bearer …");
                index = end;
                continue;
            }
        }

        if bytes.get(index..index + 4) == Some(b"AKIA".as_slice())
            && bytes.get(index + 4..index + 20).is_some_and(|suffix| {
                suffix.len() == 16
                    && suffix
                        .iter()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            })
        {
            output.push('…');
            index += 20;
            continue;
        }

        if long_runs && secret_byte(bytes[index]) {
            let mut end = index;
            while end < bytes.len() && secret_byte(bytes[end]) {
                end += 1;
            }
            if end - index >= 32 {
                output.push('…');
                index = end;
                continue;
            }
        }

        let character = text[index..].chars().next().unwrap();
        output.push(character);
        index += character.len_utf8();
    }
    output
}

/// A reply that opens with a status line such as `ENVOY_STATUS: complete` is the
/// agent's final report. `complete` means the work was delivered and its full gate passed;
/// `agent-done` means the work finished with a step still open; `incomplete` means
/// the run could not do what its brief named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalStatus {
    Complete,
    AgentDone,
    Incomplete,
}

/// Splits a final report into its status and body. The first non-empty line must be a
/// `*_STATUS: complete|agent-done|incomplete` line. Markdown emphasis, code ticks or a
/// heading mark around it still count, because omp renders them away and the pane shows
/// a plain status line. Text after the status word on the same line starts the body.
pub fn final_report(text: &str) -> Option<(FinalStatus, &str)> {
    let markup = |c: char| c.is_whitespace() || matches!(c, '*' | '_' | '`' | '#');
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    let (key, value) = line.split_once(':')?;
    let key = key.trim_matches(markup);
    if !key.ends_with("_STATUS")
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    let value = value.trim_start_matches(markup);
    let word = &value[..value
        .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
        .unwrap_or(value.len())];
    let status = match word {
        "complete" => FinalStatus::Complete,
        "agent-done" => FinalStatus::AgentDone,
        "incomplete" => FinalStatus::Incomplete,
        _ => return None,
    };
    let rest = value[word.len()..]
        .trim_start_matches(|c: char| markup(c) || matches!(c, '.' | ',' | ';' | ':' | '-' | '–' | '—'));
    let start = text.find(line).unwrap_or_default() + line.len() - rest.len();
    Some((status, text[start..].trim()))
}

const ACTION_LABEL: &str = "recommended action";
const REMAINING_LABEL: &str = "remaining";

/// The text after `label` on a line that opens with it, as in `Remaining: push main`. List,
/// quote, heading and emphasis marks before the label still count. A bare label gives an
/// empty text. `None` when the line opens with anything else.
fn labelled<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let lead = |c: char| c.is_whitespace() || matches!(c, '*' | '_' | '#' | '>' | '-');
    let emphasis = |c: char| c.is_whitespace() || matches!(c, '*' | '_');
    let text = line.trim_start_matches(lead);
    if !text.get(..label.len())?.eq_ignore_ascii_case(label) {
        return None;
    }
    let rest = text[label.len()..].trim_start_matches(emphasis);
    match rest.strip_prefix(':') {
        Some(text) => Some(text.trim_start_matches(emphasis).trim_end()),
        None if rest.is_empty() => Some(""),
        None => None,
    }
}

/// Splits the last labelled `Recommended action` paragraph from a report. List, quote,
/// heading and emphasis marks before the label still count. A bare label takes the next
/// paragraph. The paragraph ends at a blank line or at a `Remaining` label, so a remaining
/// line written after the recommendation stays in the report. Returns the report without the
/// label and paragraph, then the step; an empty step means the label had no content or said
/// `none`. Returns `None` when no valid label exists.
pub fn recommended_action(report: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = report.lines().collect();
    let (start, inline) = lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, line)| Some((index, labelled(line, ACTION_LABEL)?)))?;
    let first = if inline.is_empty() {
        (start + 1..lines.len()).find(|&index| !lines[index].trim().is_empty()).unwrap_or(lines.len())
    } else {
        start + 1
    };
    let end = (first..lines.len())
        .find(|&index| lines[index].trim().is_empty() || labelled(lines[index], REMAINING_LABEL).is_some())
        .unwrap_or(lines.len());
    let step = std::iter::once(inline)
        .chain(lines[first..end].iter().copied())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let word: String = step
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .chars()
        .take_while(|c| c.is_alphanumeric())
        .collect();
    let step = if step.trim().is_empty() || word.eq_ignore_ascii_case("none") {
        String::new()
    } else {
        step.trim().to_owned()
    };
    let before = lines[..start].join("\n");
    let after = lines[end..].join("\n");
    let rest = match (before.trim(), after.trim()) {
        (before, "") => before.to_owned(),
        ("", after) => after.to_owned(),
        (before, after) => format!("{before}\n\n{after}"),
    };
    Some((rest, step))
}

/// The open items a report lists, one per `Remaining:` line, in report order. `Remaining: none`
/// and an empty label name no item. A bare `Remaining` label takes the paragraph under it, up
/// to a blank line or the `Recommended action` label.
pub fn remaining_items(report: &str) -> Vec<&str> {
    let mut items = Vec::new();
    // `Some(false)` after a bare label, `Some(true)` once its paragraph has started.
    let mut listing = None;
    for line in report.lines() {
        match labelled(line, REMAINING_LABEL) {
            Some("") => listing = Some(false),
            Some(item) => {
                listing = None;
                items.push(item);
            }
            None if labelled(line, ACTION_LABEL).is_some() => listing = None,
            None if line.trim().is_empty() => {
                if listing == Some(true) {
                    listing = None;
                }
            }
            None => {
                if listing.is_some() {
                    listing = Some(true);
                    items.push(line.trim());
                }
            }
        }
    }
    items.retain(|item| {
        let word: String = item
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect();
        !word.is_empty() && !word.eq_ignore_ascii_case("none")
    });
    items
}

/// One-pass audit of a transcript whose omp process is gone: was the session
/// dispatched from a triage brief, and did its last reply carry a final report?
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DispatchAudit {
    pub dispatched: bool,
    pub reported: bool,
    pub entry_id: Option<String>,
    pub last_text: Option<String>,
}

/// Whether `message` hands the session a triage brief. That is a user message with a brief
/// path under `triage/briefs/`, or a developer message that starts with `Plan approved.` and
/// carries the `ENVOY_STATUS` contract, which is an approved triage plan running in a fresh
/// session.
fn hands_over_brief(message: &Value) -> bool {
    match message.get("role").and_then(Value::as_str) {
        Some("user") => text_blocks(message).is_some_and(|text| text.contains("triage/briefs/")),
        Some("developer") => text_blocks(message)
            .is_some_and(|text| text.starts_with("Plan approved.") && text.contains("ENVOY_STATUS")),
        _ => false,
    }
}

/// A session is dispatched from triage when a message hands it a brief (see
/// [`hands_over_brief`]). `reported` means some assistant reply after the latest such
/// message opens with a `*_STATUS` line (see [`final_report`]). `entry_id` and `last_text`
/// are the last reply.
pub fn dispatch_audit(transcript: &Path) -> io::Result<DispatchAudit> {
    let mut audit = DispatchAudit::default();
    scan_jsonl(transcript, |value| {
        let Some(message) = message(&value) else {
            return true;
        };
        if hands_over_brief(message) {
            audit.dispatched = true;
            audit.reported = false;
        } else if message.get("role").and_then(Value::as_str) == Some("assistant")
            && let (Some(id), Some(text)) = (value.get("id").and_then(Value::as_str), text_blocks(message))
        {
            if final_report(&text).is_some() {
                audit.reported = true;
            }
            audit.entry_id = Some(id.to_owned());
            audit.last_text = Some(text);
        }
        true
    })?;
    Ok(audit)
}

fn scan_jsonl(path: &Path, mut visit: impl FnMut(Value) -> bool) -> io::Result<()> {
    let reader = BufReader::new(File::open(path)?);
    for line in reader.lines() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !visit(value) {
            break;
        }
    }
    Ok(())
}

fn message(value: &Value) -> Option<&Value> {
    if value.get("type")?.as_str()? != "message" {
        return None;
    }
    value.get("message")
}

fn text_blocks(message: &Value) -> Option<String> {
    let blocks = message.get("content")?.as_array()?;
    let texts = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

fn secret_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn bearer_byte(byte: u8) -> bool {
    secret_byte(byte) || matches!(byte, b'.' | b'~')
}


#[cfg(test)]
mod tests {
    use super::*;

    fn transcript() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_ask.jsonl")
    }

    #[test]
    fn busy_log_names_the_pid_and_the_crash() {
        let dir = std::env::temp_dir().join(format!("triage-omp-logs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let crash = r#"{"level":"error","pid":4242,"message":"Fatal error","err":{"name":"SQLiteError","message":"Database \"/home/user/.omp/agent/agent.db\": database is locked","code":"SQLITE_BUSY"}}"#;
        fs::write(dir.join("omp.2026-11-09.4242.log"), crash).unwrap();
        fs::write(dir.join("omp.2026-11-09.7.log"), r#"{"level":"warn"}"#).unwrap();
        fs::write(dir.join("other.4242.log"), crash).unwrap();

        assert!(busy_log(&dir, 4242));
        assert!(!busy_log(&dir, 7), "a log without the crash is not a startup death");
        assert!(!busy_log(&dir, 8), "another pid's log says nothing about this run");
        assert!(!busy_log(&dir.join("missing"), 4242));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn classifies_omp_title_prefixes() {
        assert_eq!(classify("π ⠋ 🧑‍💼 HR: Onboard…"), PaneState::Working);
        assert_eq!(classify("π > waiting"), PaneState::Waiting);
        assert_eq!(classify("π ! asking"), PaneState::Asking);
        assert_eq!(classify("zsh"), PaneState::Other);
    }

    #[test]
    fn parses_plan_execution_models_in_display_order() {
        assert_eq!(
            plan_execution_models(include_str!("../tests/fixtures/plan_review_overlay.txt")),
            Some((vec!["smol", "default", "slow"].into_iter().map(str::to_owned).collect(), 1))
        );
        assert_eq!(
            plan_execution_models("continue with ◂ smol ┆ default ◀ slow ▶ plan ┆ task ▸\n↑/↓ select · ⏎ confirm · ←/→ model"),
            Some((vec!["smol", "default", "slow", "plan", "task"].into_iter().map(str::to_owned).collect(), 2))
        );
        assert_eq!(
            plan_execution_models("continue with ◂ smol \u{e0b1} default \u{e0b2}slow\u{e0b0} plan ▸\n↑/↓ select · ⏎ confirm · ←/→ model"),
            Some((vec!["smol", "default", "slow", "plan"].into_iter().map(str::to_owned).collect(), 2))
        );
        assert_eq!(plan_execution_models("continue with ◂ smol ◀ default ▶ slow ▸"), None);
        assert_eq!(plan_execution_models("continue with ◂ smol ◀ default ▶ slow ▸\n↑/↓ scroll · ⏎ open"), None);
    }

    #[test]
    fn parses_plan_review_options_and_marks_the_selected_row() {
        let (rows, selected) =
            plan_review_options(include_str!("../tests/fixtures/plan_review_overlay.txt")).unwrap();
        assert_eq!(
            rows,
            [
                "Approve and execute",
                "Approve and compact context",
                "Approve and keep context (~118k / 1m)",
                "Refine plan",
                "Save and quit"
            ]
        );
        assert_eq!(selected, 0);
        let moved = "╭─ Plan Review ─╮\n\
                     │ plan body │\n\
                     ├───────────┤\n\
                     │ Plan mode - next step │\n\
                     │ continue with  ◂  smol  ◀ default ▶  slow  ▸ │\n\
                     │   ↳ Example Model │\n\
                     │   Approve and execute │\n\
                     │   Approve and keep context (~9k / 1m) │\n\
                     │ ❯ Refine plan │\n\
                     │   Save and quit │\n\
                     ├───────────┤\n\
                     │ ↑/↓ select · ⏎ confirm · ←/→ model │\n\
                     ╰───────────╯\n";
        assert_eq!(
            plan_review_options(moved),
            Some((rows_without_compact(), 2))
        );
        assert_eq!(plan_review_options("╭─ Plan Review ─╮\n"), None);
        assert_eq!(
            plan_review_options("╭─ Plan Review ─╮\n├───┤\n│ ❯ Approve and execute │\n╰───╯\n"),
            Some((vec!["Approve and execute".to_owned()], 0))
        );
    }

    fn rows_without_compact() -> Vec<String> {
        [
            "Approve and execute",
            "Approve and keep context (~9k / 1m)",
            "Refine plan",
            "Save and quit",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn parses_ask_dialog_cursor() {
        assert_eq!(dialog_cursor(include_str!("../tests/fixtures/ask_dialog.txt")), Some((1, 4)));
    }

    #[test]
    fn parses_multi_question_dialog_cursor() {
        assert_eq!(
            dialog_cursor(include_str!("../tests/fixtures/ask_dialog_multi.txt")),
            Some((0, 3))
        );
    }

    #[test]
    fn dialog_cursor_counts_chosen_rows() {
        assert_eq!(dialog_cursor("╭─ Ask ─╮\n│ ❯ ◉ Green\n╰─\n"), Some((0, 1)));
    }

    #[test]
    fn review_answers_parses_answered_review() {
        assert_eq!(
            review_answers(include_str!("../tests/fixtures/ask_dialog_multi_review.txt")).unwrap(),
            ["f1: Formal (Recommended)", "f2: Yearly", "color: “Violet with spaces”"]
        );
    }

    #[test]
    fn review_answers_parses_unanswered_review() {
        assert_eq!(
            review_answers(include_str!("../tests/fixtures/ask_dialog_multi_review_empty.txt")).unwrap(),
            ["f1: unanswered", "f2: unanswered", "color: unanswered"]
        );
    }

    #[test]
    fn review_answers_rejects_question_tab() {
        assert!(review_answers(include_str!("../tests/fixtures/ask_dialog_multi.txt")).is_none());
    }

    #[test]
    fn detects_plan_review_overlay() {
        assert!(plan_review_open(include_str!("../tests/fixtures/plan_review_overlay.txt")));
        assert!(!plan_review_open(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!plan_review_open(include_str!("../tests/fixtures/ask_dialog.txt")));
    }

    #[test]
    fn detects_plan_mode_status() {
        assert!(plan_mode_on(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!plan_mode_on(include_str!("../tests/fixtures/ask_dialog.txt")));
        assert!(!plan_mode_on(include_str!("../tests/fixtures/plan_review_overlay.txt")));
        assert!(plan_mode_on("╭─ Plan Review ─╮\n⬢ Example Model · 🗺 Plan · ~/project"));
    }

    #[test]
    fn detects_no_model_launch() {
        assert!(no_model(include_str!("../tests/fixtures/no_model.txt")));
        assert!(!no_model(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!no_model(include_str!("../tests/fixtures/ask_dialog.txt")));
        assert!(!no_model("⬢ no-model · 🗑 ~/tmp\n"));
    }

    #[test]
    fn detects_paused_run_from_working_row() {
        assert!(run_paused(include_str!("../tests/fixtures/async_wait.txt")));
        assert!(run_paused("  ⎋ Retrying (1/3) in 4s…\n\n───\n\n───\n status"));
        assert!(run_paused("  ⎋ ⠋ Waiting for 2 background jobs: bg_1, bg_2\n───\n───\n"));
        assert!(!run_paused(include_str!("../tests/fixtures/plan_mode_idle.txt")));
        assert!(!run_paused(include_str!("../tests/fixtures/ask_dialog.txt")));
        assert!(!run_paused(include_str!("../tests/fixtures/plan_review_overlay.txt")));
        assert!(!run_paused(" Retrying the deploy fixed it.\n\n───\n\n───\n status"));
        assert!(!run_paused("  ⎋ Waiting for background job bg_1\n\n Done.\n───\n\n───\n status"));
        assert!(!run_paused("  ⎋ Waiting for background job bg_1\n"));
    }

    fn turn_end_of(lines: &[&str]) -> Option<TurnEnd> {
        // Each call writes a file of its own, because tests run on parallel threads.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "triage-omp-turn-end-{}-{}.jsonl",
            std::process::id(),
            CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::write(&path, lines.join("\n")).unwrap();
        let end = turn_end(&path).unwrap();
        fs::remove_file(path).unwrap();
        end
    }

    #[test]
    fn turn_end_follows_omp_settled_status() {
        let user = r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#;
        let report = r#"{"type":"message","id":"a1","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"ENVOY_STATUS: complete\n\nShipped."}]}}"#;
        let wake = r#"{"type":"custom_message","id":"c1","customType":"async-result","content":"done"}"#;
        let exit = r#"{"type":"custom_message","id":"c2","customType":"launch-completion","content":"Supervised process build exited with exit code 0."}"#;
        let reply = r#"{"type":"message","id":"a2","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"Background job finished."}]}}"#;
        let tool_use = r#"{"type":"message","id":"t1","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"text","text":"Dumping rows now."}]}}"#;
        let length = r#"{"type":"message","id":"l1","message":{"role":"assistant","stopReason":"length","content":[{"type":"text","text":"Cut off"}]}}"#;
        let aborted = r#"{"type":"message","id":"x1","message":{"role":"assistant","stopReason":"aborted","errorMessage":"Interrupted by user","content":[]}}"#;
        let error = r#"{"type":"message","id":"e1","message":{"role":"assistant","stopReason":"error","errorMessage":"Bad request","content":[]}}"#;
        let blank_stop = r#"{"type":"message","id":"b1","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"  \n"}]}}"#;
        let no_reason = r#"{"type":"message","id":"n1","message":{"role":"assistant","content":[{"type":"text","text":"Hello"}]}}"#;

        assert_eq!(turn_end_of(&[user]), None);
        assert_eq!(
            turn_end_of(&[user, report, wake, reply]),
            Some(TurnEnd::Reply {
                entry_id: "a2".into(),
                text: "Background job finished.".into(),
                report: Some("ENVOY_STATUS: complete\n\nShipped.".into()),
                owed: false,
            })
        );
        assert_eq!(turn_end_of(&[user, report, wake]), Some(TurnEnd::Running));
        assert_eq!(turn_end_of(&[user, report, exit]), Some(TurnEnd::Running));
        assert_eq!(
            turn_end_of(&[report, wake, user, reply]),
            Some(TurnEnd::Reply {
                entry_id: "a2".into(),
                text: "Background job finished.".into(),
                report: None,
                owed: false,
            })
        );
        assert_eq!(
            turn_end_of(&[user, report]),
            Some(TurnEnd::Reply {
                entry_id: "a1".into(),
                text: "ENVOY_STATUS: complete\n\nShipped.".into(),
                report: None,
                owed: false,
            })
        );
        assert_eq!(
            turn_end_of(&[report, user, reply]),
            Some(TurnEnd::Reply {
                entry_id: "a2".into(),
                text: "Background job finished.".into(),
                report: None,
                owed: false,
            })
        );
        assert_eq!(
            turn_end_of(&[user, length]),
            Some(TurnEnd::Reply { entry_id: "l1".into(), text: "Cut off".into(), report: None, owed: true })
        );
        assert_eq!(
            turn_end_of(&[user, tool_use, error]),
            Some(TurnEnd::Error { entry_id: "e1".into(), message: "Bad request".into() })
        );
        assert_eq!(turn_end_of(&[user, tool_use, aborted]), Some(TurnEnd::Aborted));
        assert_eq!(turn_end_of(&[user, tool_use]), Some(TurnEnd::Running));
        assert_eq!(turn_end_of(&[user, blank_stop]), Some(TurnEnd::Running));
        assert_eq!(turn_end_of(&[user, no_reason]), Some(TurnEnd::Running));
    }

    // On 2026-11-16 a session the user drove by hand reported its work. Triage then asked it
    // for another final report after four of the questions he typed. A session owes one
    // report per brief, or one in total when it had no brief. A later reply to a typed
    // message owes none.
    #[test]
    fn a_reply_owes_a_report_until_one_follows_the_latest_brief() {
        let entry = |id: &str, role: &str, text: &str| {
            serde_json::json!({
                "type": "message",
                "id": id,
                "message": {"role": role, "stopReason": "stop", "content": [{"type": "text", "text": text}]},
            })
            .to_string()
        };
        let typed = entry("u1", "user", "how do i use it");
        let brief = entry("u2", "user", "Read the brief at /data/triage/briefs/triage-brief-1.md and carry it out.");
        let approved = entry("d1", "developer", "Plan approved. End with `ENVOY_STATUS: complete`.");
        let report = entry("a1", "assistant", "ENVOY_STATUS: complete\nS: Asked.\nR: Shipped.");
        let status_only = entry("a2", "assistant", "ENVOY_STATUS: complete");
        let reply = entry("a3", "assistant", "Run it from the project directory.");
        let owed = |lines: &[&String]| {
            let lines: Vec<&str> = lines.iter().map(|line| line.as_str()).collect();
            match turn_end_of(&lines) {
                Some(TurnEnd::Reply { owed, .. }) => owed,
                other => panic!("not a reply: {other:?}"),
            }
        };

        assert!(owed(&[&typed, &reply]), "a session that never reported owes its first report");
        assert!(owed(&[&brief, &reply]), "a brief owes a report");
        assert!(!owed(&[&typed, &report, &typed, &reply]), "a typed message after the report owes none");
        assert!(!owed(&[&brief, &report, &typed, &reply]), "a typed message after a brief's report owes none");
        assert!(owed(&[&typed, &report, &brief, &reply]), "a new brief owes a report of its own");
        assert!(owed(&[&typed, &report, &approved, &reply]), "an approved plan owes a report of its own");
        assert!(owed(&[&typed, &status_only, &typed, &reply]), "a status line without a report pays nothing");
    }

    #[test]
    fn resume_flags_name_the_breadcrumb_session() {
        let session = Path::new(
            "/sessions/-proj/2026-10-29T23-31-18-428Z_01a18a00-eb1c-7530-91cb-d6fd8ac4075d.jsonl",
        );
        let session_str = session.to_str().unwrap();
        let argv = |flags: &[&str]| {
            std::iter::once("omp")
                .chain(flags.iter().copied())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };

        assert!(resumes(
            &argv(&["--resume", session_str, "--profile", "default"]),
            session
        ));
        assert!(resumes(&argv(&[&format!("--resume={session_str}")]), session));
        assert!(resumes(&argv(&["-r", "01a18a00"]), session));
        assert!(resumes(&argv(&["--restore", session_str]), session));

        assert!(!resumes(&argv(&[]), session));
        assert!(!resumes(&argv(&["--model", "k3"]), session));
        assert!(!resumes(&argv(&["-c"]), session));
        assert!(!resumes(&argv(&["--continue"]), session));
        assert!(!resumes(&argv(&["--resume", "/other.jsonl"]), session));
        assert!(!resumes(&argv(&["-r"]), session));
        assert!(!resumes(&argv(&["--resume", "--profile", "default"]), session));
        assert!(!resumes(&argv(&["-r", "01a19"]), session));
        assert!(!resumes(&argv(&["--resume="]), session));
    }

    #[test]
    fn dialog_cursor_returns_latest_ask_panel() {
        let screen = concat!(
            "╭─ Ask ─╮\n",
            "│   ○ First\n",
            "│ ❯ ○ Selected\n",
            "╰─\n",
            "╭─ Ask ─╮\n",
            "│ ❯ ○ Latest\n",
            "│   ○ Other (type your own)\n",
            "╰─\n",
        );
        assert_eq!(dialog_cursor(screen), Some((0, 2)));
    }

    #[test]
    fn dialog_cursor_ignores_ask_marker_inside_dialog() {
        let screen = concat!(
            "╭─ Ask ─╮\n",
            "│   ○ First\n",
            "│ prompt text includes ╭─ Ask as ordinary text\n",
            "│ ❯ ○ Second\n",
            "│   ○ Other (type your own)\n",
            "╰─\n",
        );
        assert_eq!(dialog_cursor(screen), Some((1, 3)));
    }

    #[test]
    fn finds_pending_ask_and_completed_result() {
        let prefix = include_str!("../tests/fixtures/transcript_ask.jsonl")
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        let pending = std::env::temp_dir().join(format!("triage-omp-pending-{}.jsonl", std::process::id()));
        fs::write(&pending, prefix).unwrap();
        let call = pending_ask(&pending).unwrap().unwrap();
        assert_eq!(call.questions.len(), 1);
        assert_eq!(call.questions[0].options.len(), 3);
        assert_eq!(call.questions[0].recommended, Some(1));
        fs::remove_file(pending).unwrap();

        assert!(pending_ask(&transcript()).unwrap().is_none());
    }

    fn html_ask_fixture_html() -> String {
        let line = include_str!("../tests/fixtures/transcript_ask_html.jsonl")
            .lines()
            .nth(1)
            .unwrap();
        let value: Value = serde_json::from_str(line).unwrap();
        value["message"]["content"][0]["arguments"]["html"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn option_labels(question: &Question) -> Vec<String> {
        question.options.iter().map(|option| option.label.clone()).collect()
    }

    #[test]
    fn finds_pending_html_ask_with_its_form() {
        let prefix = include_str!("../tests/fixtures/transcript_ask_html.jsonl")
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        let pending = std::env::temp_dir().join(format!("triage-omp-html-pending-{}.jsonl", std::process::id()));
        fs::write(&pending, prefix).unwrap();
        let call = pending_ask(&pending).unwrap().unwrap();
        fs::remove_file(pending).unwrap();
        assert_eq!(call.tool_call_id, "call_ask_html");
        assert_eq!(call.questions.len(), 2);
        assert!(call.form.as_ref().is_some_and(|form| form.controls.len() == 2));
    }

    #[test]
    fn html_ask_turns_each_fieldset_into_a_question_and_control() {
        let (questions, form) = parse_html_ask(&html_ask_fixture_html()).unwrap();
        assert_eq!(questions.len(), form.controls.len());
        let finding = &questions[0];
        assert_eq!(finding.id, "finding-001");
        assert_eq!(finding.header.as_deref(), Some("Rotate the deploy key"));
        assert_eq!(finding.question, "Task: Decide whether to rotate it now.");
        assert_eq!(finding.context, "Situation: The deploy key is 120 days old.");
        assert_eq!(
            option_labels(finding),
            [
                "Do as recommended (recommended) — Execute the action shown in this finding",
                "Investigate first",
                "Skip",
            ]
        );
        assert_eq!(finding.recommended, Some(0));
        assert_eq!(
            form.controls[0],
            HtmlControl::Radio { enabled: 3, leave_unselected: false, other: true }
        );
        let reason = &questions[1];
        assert_eq!(reason.header.as_deref(), Some("Reason"));
        assert_eq!(reason.question, "Reason");
        assert_eq!(reason.context, "Task: Say why the key stays, if you skip.");
        assert_eq!(option_labels(reason), ["Leave blank"]);
        assert_eq!(reason.recommended, Some(0));
        assert_eq!(form.controls[1], HtmlControl::Text { multiline: false, initial_lines: 1 });
        assert_eq!(form.intro, "");
    }

    #[test]
    fn html_ask_gives_each_question_the_text_since_the_one_before() {
        let html = "<form><p>Two edits wait.</p>\
            <fieldset><legend>Finding 1: Rename the key</legend><p>Target: <code>README.md</code></p><pre>OLD: a\nNEW: b</pre>\
            <label><input type='radio' name='f1' value='approve' checked> Approve</label><label><input type='radio' name='f1' value='reject'> Reject</label></fieldset>\
            <fieldset><legend>Finding 2: Add a note</legend><ul><li>Adds one line</li><li>Keeps <b>the</b> rest</li></ul>\
            <label><input type='radio' name='f2' value='approve'> Approve</label><label><input type='radio' name='f2' value='reject'> Reject</label></fieldset>\
            <table><tr><th>File</th><th>Change</th></tr><tr><td>README.md</td><td>rename</td></tr></table>\
            <p>Answer both before I edit.</p></form>";
        let (questions, form) = parse_html_ask(html).unwrap();
        assert_eq!(form.intro, "Two edits wait.");
        assert_eq!(questions[0].question, "Finding 1: Rename the key");
        assert_eq!(
            questions[0].context,
            "Two edits wait.\n\nTarget: `README.md`\n\n```\nOLD: a\nNEW: b\n```"
        );
        assert_eq!(questions[1].question, "Finding 2: Add a note");
        assert_eq!(
            questions[1].context,
            "- Adds one line\n- Keeps **the** rest\n\n| File | Change |\n| --- | --- |\n| README.md | rename |\n\nAnswer both before I edit."
        );
    }

    #[test]
    fn html_ask_names_fields_like_omp() {
        let html = "<form>\
            <fieldset><legend>Deploy</legend>\
            <label><input type='radio' name='when' value='now' checked> Now</label><p>Now restarts the service.</p>\
            <label><input type='radio' name='when' value='later'> Later</label></fieldset>\
            <fieldset><legend>Notes</legend><input type='text' name='why_not'>\
            <label>Tier <select name='tier'><option>Basic</option><option>Pro</option></select></label></fieldset></form>";
        let (questions, form) = parse_html_ask(html).unwrap();
        let texts: Vec<&str> = questions.iter().map(|question| question.question.as_str()).collect();
        assert_eq!(texts, ["Deploy", "why not", "Tier"]);
        let contexts: Vec<&str> = questions.iter().map(|question| question.context.as_str()).collect();
        assert_eq!(contexts, ["Now restarts the service.", "", ""]);
        assert_eq!(option_labels(&questions[0]), ["Now", "Later"]);
        assert_eq!(form.controls[0], HtmlControl::Radio { enabled: 2, leave_unselected: false, other: true });
    }

    #[test]
    fn html_text_select_and_required_fields_keep_their_defaults() {
        let html = "<form>\
            <fieldset><legend>Pick a tier</legend><label>Tier <select name='tier'><option>Basic</option><option>Pro</option></select></label></fieldset>\
            <fieldset><legend>Notes</legend><textarea name='notes'>Line one\nLine two</textarea></fieldset>\
            <fieldset><legend>Owner</legend><input name='owner' value='Nick' required></fieldset>\
            <fieldset><legend>Ticket</legend><input name='ticket' required></fieldset>\
            </form>";
        let (questions, form) = parse_html_ask(html).unwrap();
        assert_eq!(option_labels(&questions[0]), ["Leave unselected", "Basic", "Pro"]);
        assert_eq!(questions[0].recommended, Some(0));
        assert_eq!(form.controls[0], HtmlControl::Select { enabled: 2, leave_unselected: true });
        assert_eq!(option_labels(&questions[1]), ["Keep current value"]);
        assert_eq!(form.controls[1], HtmlControl::Text { multiline: true, initial_lines: 2 });
        assert_eq!(option_labels(&questions[2]), ["Keep current value"]);
        assert_eq!(form.controls[2], HtmlControl::Text { multiline: false, initial_lines: 1 });
        assert!(questions[3].options.is_empty());
        assert_eq!(questions[3].recommended, None);
        assert_eq!(form.controls[3], HtmlControl::Text { multiline: false, initial_lines: 1 });
    }

    #[test]
    fn html_radio_rules_cover_leave_unselected_other_and_suppression() {
        let html = "<form>\
            <fieldset><legend>Optional</legend><label><input type='radio' name='a' value='1'> Alpha</label><label><input type='radio' name='a' value='2'> Beta</label></fieldset>\
            <fieldset><legend>Wording</legend><label><input type='radio' name='b' value='x' checked> Yes</label><label><input type='radio' name='b' value='y'> Other, later</label></fieldset>\
            <fieldset><legend>Disabled</legend><label><input type='radio' name='c' value='p' checked> Keep</label><label><input type='radio' name='c' value='q' disabled> Gone</label></fieldset>\
            </form>";
        let (questions, form) = parse_html_ask(html).unwrap();
        assert_eq!(option_labels(&questions[0]), ["Leave unselected", "Alpha", "Beta"]);
        assert_eq!(form.controls[0], HtmlControl::Radio { enabled: 2, leave_unselected: true, other: true });
        assert_eq!(option_labels(&questions[1]), ["Yes", "Other, later"]);
        assert_eq!(form.controls[1], HtmlControl::Radio { enabled: 2, leave_unselected: false, other: false });
        assert_eq!(option_labels(&questions[2]), ["Keep"]);
        assert_eq!(questions[2].recommended, Some(0));
        assert_eq!(form.controls[2], HtmlControl::Radio { enabled: 1, leave_unselected: false, other: true });
    }

    #[test]
    fn html_checkbox_groups_unnamed_and_multi_selects_are_read_only() {
        let html = "<form>\
            <fieldset><legend>Features</legend><label><input type='checkbox' name='f' value='a'> A</label><label><input type='checkbox' name='f' value='b'> B</label></fieldset>\
            <fieldset><legend>Loose</legend><input type='text'></fieldset>\
            <fieldset><legend>Many</legend><select name='m' multiple><option>X</option></select></fieldset>\
            </form>";
        let (questions, form) = parse_html_ask(html).unwrap();
        assert_eq!(questions.len(), 3);
        assert!(form.controls.iter().all(|control| *control == HtmlControl::Unsupported));
    }

    #[test]
    fn html_without_named_controls_makes_no_form() {
        assert!(parse_html_ask("<form><p>Nothing to answer.</p></form>").is_none());
        assert!(parse_html_ask("<form><input type='hidden' name='token' value='x'><button>Go</button></form>").is_none());
    }

    #[test]
    fn html_ask_result_returns_answers_in_form_order() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_ask_html.jsonl");
        assert_eq!(
            ask_result(&fixture, "call_ask_html").unwrap(),
            Some(vec!["Investigate first".to_owned(), "Old key, rotated Tuesday".to_owned()])
        );
        assert_eq!(ask_result(&transcript(), "call_ask_1").unwrap(), Some(vec!["Blue".to_owned()]));
    }

    #[test]
    fn parses_plan_proposal_from_transcript() {
        let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcript_propose.jsonl");
        let proposals = proposes(&transcript).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "planverify-proof");
        assert!(proposals[0]
            .plan_path
            .ends_with("local/planverify-proof-plan.md"));
    }

    #[test]
    fn includes_in_flight_plan_proposal() {
        let path = std::env::temp_dir()
            .join(format!("triage-omp-propose-pending-{}.jsonl", std::process::id()));
        fs::write(
            &path,
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"pending","name":"write","arguments":{"path":"xd://propose"}}]}}"#,
        )
        .unwrap();

        let proposals = proposes(&path).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "");
        assert!(proposals[0].plan_path.as_os_str().is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn falls_back_to_write_content_when_proposal_title_is_missing() {
        let path = std::env::temp_dir()
            .join(format!("triage-omp-propose-fallback-{}.jsonl", std::process::id()));
        let transcript = concat!(
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"call","name":"write","arguments":{"path":"xd://propose","content":"fallback plan title"}}]}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"call","details":{"xdev":{"inner":{"planFilePath":"local://fallback-plan.md"}}}}}"#,
        );
        fs::write(&path, transcript).unwrap();

        let proposals = proposes(&path).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "fallback plan title");
        assert!(proposals[0].plan_path.ends_with("local/fallback-plan.md"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn parses_bash_propose_result_from_omp_19_1() {
        let path = std::env::temp_dir()
            .join(format!("triage-omp-propose-bash-{}.jsonl", std::process::id()));
        let transcript = concat!(
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"bash-call","name":"bash","arguments":{"command":"propose say-hello"}}]}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"bash-call","toolName":"bash","details":{"toolCommands":[{"tool":"read","inner":{"title":"ignored"}},{"tool":"propose","args":{"title":"say-hello"},"inner":{"planFilePath":"local://say-hello-plan.md","title":"say-hello"}}]}}}"#,
        );
        fs::write(&path, transcript).unwrap();

        let proposals = proposes(&path).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "say-hello");
        assert!(proposals[0].plan_path.ends_with("local/say-hello-plan.md"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn finds_a_proposal_wherever_the_result_nests_it() {
        let path = std::env::temp_dir()
            .join(format!("triage-omp-propose-nested-{}.jsonl", std::process::id()));
        // A tool and a nesting that no code names: only the `planFilePath` field identifies it.
        let transcript = concat!(
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"new-tool","toolName":"exit_plan","details":{"a":[{"b":{"title":"deep-plan","planFilePath":"local://deep-plan.md"}}]}}}"#,
            "\n",
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"read-call","toolName":"read","details":{"path":"local://deep-plan.md","lines":3}}}"#,
        );
        fs::write(&path, transcript).unwrap();

        let proposals = proposes(&path).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "deep-plan");
        assert!(proposals[0].plan_path.ends_with("local/deep-plan.md"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_renamed_review_still_looks_like_one_and_chat_does_not() {
        assert!(looks_like_plan_review("╭─ Plan review ─╮\n│ ❯ Go ahead │\n"));
        assert!(looks_like_plan_review("│ ❯ Approve and run │\n"));
        assert!(!looks_like_plan_review("I opened the Plan Review and chose Approve and execute.\n"));
        assert!(!looks_like_plan_review("π > session\n"));
    }

    #[test]
    fn reads_ask_result_and_assistant_text() {
        assert_eq!(ask_result(&transcript(), "call_ask_1").unwrap(), Some(vec!["Blue".to_owned()]));
        assert_eq!(last_assistant_text(&transcript()).unwrap().unwrap().1, "Blue");
    }

    #[test]
    fn checks_empty_editor_between_last_rules() {
        assert!(editor_empty("──────\n  ←←  \n──────"));
        assert!(!editor_empty("──────\ntext\n──────"));
        assert!(!editor_empty("no rules"));
    }

    #[test]
    fn redacts_secret_shapes() {
        let key = format!("{}{}", "AKIA", "1234567890ABCDEF");
        assert_eq!(redact(&format!("key {key}")), "key …");
        assert_eq!(redact("Bearer short-token"), "Bearer …");
        assert_eq!(redact(&"a".repeat(32)), "…");
    }

    #[test]
    fn final_report_needs_a_status_line() {
        assert_eq!(
            final_report("\nENVOY_STATUS: complete\nShipped the fix.\nTests pass."),
            Some((FinalStatus::Complete, "Shipped the fix.\nTests pass."))
        );
        assert_eq!(
            final_report("ENVOY_STATUS: agent-done\n\nPush still open."),
            Some((FinalStatus::AgentDone, "Push still open."))
        );
        assert_eq!(
            final_report("TRIAGE_STATUS: incomplete\nBlocked on access."),
            Some((FinalStatus::Incomplete, "Blocked on access."))
        );
        assert_eq!(final_report("ENVOY_STATUS: maybe\nx"), None);
        assert_eq!(final_report("Waiting on the build job."), None);
        assert_eq!(final_report("Note: ENVOY_STATUS: complete"), None);
    }

    #[test]
    fn final_report_accepts_status_without_report_body() {
        assert_eq!(
            final_report("ENVOY_STATUS: complete"),
            Some((FinalStatus::Complete, ""))
        );
        assert_eq!(
            final_report("TRIAGE_STATUS: incomplete\n \n"),
            Some((FinalStatus::Incomplete, ""))
        );
    }

    #[test]
    fn final_report_reads_markdown_wrapped_and_run_on_status_lines() {
        assert_eq!(
            final_report("**ENVOY_STATUS: complete**\n\nDraft created: \"Weekly fixes\" for the caller."),
            Some((FinalStatus::Complete, "Draft created: \"Weekly fixes\" for the caller."))
        );
        assert_eq!(
            final_report("`ENVOY_STATUS: agent-done`\nPush still open."),
            Some((FinalStatus::AgentDone, "Push still open."))
        );
        assert_eq!(
            final_report("**ENVOY_STATUS:** incomplete\nBlocked on access."),
            Some((FinalStatus::Incomplete, "Blocked on access."))
        );
        assert_eq!(
            final_report("ENVOY_STATUS: agent-done — open items are the push\nDetails."),
            Some((FinalStatus::AgentDone, "open items are the push\nDetails."))
        );
        assert_eq!(
            final_report("ENVOY_STATUS: complete. I wrote the line."),
            Some((FinalStatus::Complete, "I wrote the line."))
        );
        assert_eq!(final_report("**ENVOY_STATUS: complete**"), Some((FinalStatus::Complete, "")));
        assert_eq!(
            final_report("Both are finished (last reply `ENVOY_STATUS: complete`) — done."),
            None
        );
        assert_eq!(final_report("ENVOY_STATUS: completed\nx"), None);
    }
    #[test]
    fn remaining_items_lists_each_open_item_and_none_names_no_item() {
        let report = "S: Hook refused the push.\nT: Ship the fix.\nA: Fixed it in 4f1c2aa.\nR: Suite passes on 4f1c2aa.\nRemaining: Push 4f1c2aa, which waits on Nick removing an untracked file.\n**Remaining:** Test a first sign-up live, which waits on the next sign-up.\nRecommended action: none";
        assert_eq!(
            remaining_items(report),
            vec![
                "Push 4f1c2aa, which waits on Nick removing an untracked file.",
                "Test a first sign-up live, which waits on the next sign-up.",
            ]
        );
        assert!(remaining_items("R: Shipped.\nRemaining: none\nRecommended action: none").is_empty());
        assert!(remaining_items("R: Shipped.\n- Remaining: None.\n").is_empty());
        assert!(remaining_items("R: Shipped.\nRemaining:\nRecommended action: Push it.").is_empty());
        // A sentence that merely opens with the word, and a label quoted inside an `R:` line.
        assert!(remaining_items("Remaining work is tracked in the worklog.").is_empty());
        assert!(remaining_items("R: Remaining: nothing, all shipped.").is_empty());
        // A bare label takes the paragraph under it.
        assert_eq!(
            remaining_items("R: Shipped.\n\n## Remaining\n\n- Push main, held by the hook.\n- Deploy, which waits on the push.\n\nRecommended action: none"),
            vec!["- Push main, held by the hook.", "- Deploy, which waits on the push."]
        );
        assert!(remaining_items("Remaining:\nnone\n").is_empty());
    }

    #[test]
    fn a_remaining_line_after_the_recommended_action_stays_in_the_report() {
        assert_eq!(
            recommended_action("R: Shipped.\nRecommended action: none\nRemaining: Push main, held by the hook."),
            Some(("R: Shipped.\n\nRemaining: Push main, held by the hook.".to_owned(), String::new()))
        );
        assert_eq!(
            recommended_action("R: Shipped.\nRecommended action: Approve the deploy.\nRemaining: Deploy, which waits on Nick."),
            Some((
                "R: Shipped.\n\nRemaining: Deploy, which waits on Nick.".to_owned(),
                "Approve the deploy.".to_owned()
            ))
        );
    }

    #[test]
    fn recommended_action_splits_the_last_labelled_step_from_the_report() {
        assert_eq!(
            recommended_action(
                "Both lack the grant.\n\n**Recommended action:** Add both as readers.\n\nTime used: 9 seconds."
            ),
            Some((
                "Both lack the grant.\n\nTime used: 9 seconds.".to_owned(),
                "Add both as readers.".to_owned()
            ))
        );
        assert_eq!(
            recommended_action(
                "Found it.\n\n## Recommended action\n\nRotate the key\nthen restart the unit."
            ),
            Some(("Found it.".to_owned(), "Rotate the key\nthen restart the unit.".to_owned()))
        );
        assert_eq!(
            recommended_action("Nothing is wrong.\n\nRecommended action: none."),
            Some(("Nothing is wrong.".to_owned(), String::new()))
        );
        assert_eq!(
            recommended_action("Found it.\n\nRecommended action:"),
            Some(("Found it.".to_owned(), String::new()))
        );
        assert_eq!(recommended_action("Recommended actions differ by host."), None);
        assert_eq!(recommended_action("No step named."), None);
    }

    #[test]
    fn audit_detects_dispatch_and_report_state() {
        let path = std::env::temp_dir().join(format!("triage-omp-audit-{}.jsonl", std::process::id()));
        let brief = r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"<file name=\"/home/x/.local/share/triage/briefs/triage-brief-a.md\">\ncarry it out"}]}}"#;
        let unreported = r#"{"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"Work done, details inside."}]}}"#;
        let reported = r#"{"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"text","text":"ENVOY_STATUS: complete\n\nAll done."}]}}"#;

        fs::write(&path, format!("{brief}\n{unreported}\n")).unwrap();
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && !audit.reported);
        assert_eq!(audit.entry_id.as_deref(), Some("a1"));
        assert_eq!(audit.last_text.as_deref(), Some("Work done, details inside."));

        fs::write(&path, format!("{brief}\n{reported}\n")).unwrap();
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && audit.reported);

        fs::write(&path, format!("{unreported}\n")).unwrap();
        let audit = dispatch_audit(&path).unwrap();
        assert!(!audit.dispatched && !audit.reported);

        fs::write(&path, "\n").unwrap();
        assert_eq!(dispatch_audit(&path).unwrap(), DispatchAudit::default());

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn dispatch_audit_counts_plan_executions_and_reports_since_the_brief() {
        let path = std::env::temp_dir().join(format!("triage-omp-audit-plan-{}.jsonl", std::process::id()));
        let line = |id: &str, role: &str, text: &str| {
            serde_json::json!({"type": "message", "id": id, "message": {"role": role, "content": [{"type": "text", "text": text}]}}).to_string()
        };
        let write = |lines: &[String]| fs::write(&path, lines.join("\n") + "\n").unwrap();
        let brief = line("u1", "user", "Read /home/x/.local/share/triage/briefs/a.md and do it.");
        let report = line("a1", "assistant", "ENVOY_STATUS: complete\n\nDone.");

        write(&[
            line("d1", "developer", "Plan approved.\n\nEnd with ENVOY_STATUS: complete."),
            line("a1", "assistant", "Working on it."),
        ]);
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && !audit.reported);

        write(&[
            line("d1", "developer", "Plan approved.\n\nEnd with a report."),
            line("a1", "assistant", "Working on it."),
        ]);
        assert!(!dispatch_audit(&path).unwrap().dispatched);

        write(&[
            brief.clone(),
            report.clone(),
            line("u2", "user", "thanks"),
            line("a2", "assistant", "You're welcome."),
        ]);
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && audit.reported);
        assert_eq!(audit.last_text.as_deref(), Some("You're welcome."));

        write(&[brief.clone(), report, brief, line("a2", "assistant", "Started again.")]);
        let audit = dispatch_audit(&path).unwrap();
        assert!(audit.dispatched && !audit.reported);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn service_wake_pending_reads_owned_job_processes() {
        let root = std::env::temp_dir().join(format!("triage-omp-services-{}", std::process::id()));
        let dir = root.join("0000000000000000/daemons/build");
        fs::create_dir_all(&dir).unwrap();
        let transcript = Path::new("/sessions/-tmp/2026-11-07T16-18-24-308Z_01a1b6cd-test.jsonl");
        let now = SystemTime::now();
        let ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let (pid, started) = (std::process::id(), ms - 60_000);
        let check = |spec: &Value, meta: &Value| {
            fs::write(dir.join("spec.json"), spec.to_string()).unwrap();
            fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
            service_wake_pending_in(&root, transcript, now)
        };
        let spec = serde_json::json!({"name": "build"});
        let base = serde_json::json!({
            "daemon": {"name": "build", "state": "running", "owner": "01a1b6cd-test", "pid": pid, "startedAt": started},
            "completionEvents": true,
            "pendingCompletions": []
        });
        assert!(check(&spec, &base));

        let ready = serde_json::json!({"name": "build", "ready": {"port": 8080}});
        assert!(!check(&ready, &base));
        let mut meta = base.clone();
        meta["daemon"]["owner"] = serde_json::json!("someone-else");
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["pid"] = serde_json::json!(999_999_999);
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["startedAt"] = serde_json::json!(ms - 3 * 60 * 60 * 1000);
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["completionEvents"] = serde_json::json!(false);
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["state"] = serde_json::json!("stopping");
        assert!(!check(&spec, &meta));
        meta = base.clone();
        meta["daemon"]["state"] = serde_json::json!("starting");
        meta["daemon"].as_object_mut().unwrap().remove("pid");
        assert!(check(&spec, &meta));

        let mut exited = base.clone();
        exited["daemon"]["state"] = serde_json::json!("exited");
        exited["daemon"].as_object_mut().unwrap().remove("pid");
        exited["daemon"]["exitedAt"] = serde_json::json!(ms - 1000);
        exited["pendingCompletions"] = serde_json::json!([{"name": "build"}]);
        assert!(check(&spec, &exited));
        exited["daemon"]["exitedAt"] = serde_json::json!(ms - 120_000);
        assert!(!check(&spec, &exited));
        exited["daemon"]["exitedAt"] = serde_json::json!(ms - 1000);
        exited["pendingCompletions"] = serde_json::json!([]);
        assert!(!check(&spec, &exited));

        fs::remove_dir_all(&root).unwrap();
    }

    fn goal_active_of(lines: &[&str]) -> bool {
        let path = std::env::temp_dir().join(format!("triage-omp-goal-{}-{}.jsonl", std::process::id(), lines.len()));
        fs::write(&path, lines.join("\n")).unwrap();
        let active = goal_active(&path).unwrap();
        fs::remove_file(path).unwrap();
        active
    }

    #[test]
    fn goal_active_reads_latest_mode_change() {
        let user = r#"{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#;
        let active = r#"{"type":"mode_change","id":"m1","mode":"goal","data":{"goal":{"id":"g1","status":"active"}}}"#;
        let complete = r#"{"type":"mode_change","id":"m1","mode":"goal","data":{"goal":{"id":"g1","status":"complete"}}}"#;
        let none = r#"{"type":"mode_change","id":"m2","mode":"none"}"#;
        let paused = r#"{"type":"mode_change","id":"m3","mode":"goal_paused","data":{"goal":{"id":"g1","status":"paused"}}}"#;
        let budget = r#"{"type":"mode_change","id":"m4","mode":"goal","data":{"goal":{"id":"g1","status":"budget-limited"}}}"#;

        assert!(goal_active_of(&[active]));
        assert!(!goal_active_of(&[active, complete]));
        assert!(!goal_active_of(&[active, none]));
        assert!(!goal_active_of(&[paused]));
        assert!(!goal_active_of(&[budget]));
        assert!(!goal_active_of(&[user]));
    }
}
