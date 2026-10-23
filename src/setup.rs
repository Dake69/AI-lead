use crate::config::{Config, Provider};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{fs, path::Path};
use unicode_width::UnicodeWidthChar;

const TEXT: Color = Color::Rgb(218, 220, 226);
const DIM: Color = Color::Rgb(144, 149, 160);
const LINE: Color = Color::Rgb(66, 70, 78);
const BG: Color = Color::Rgb(18, 19, 22);

#[derive(Clone, Default)]
pub(crate) struct Text {
    pub(crate) value: String,
    pub(crate) cursor: usize,
}
impl From<String> for Text {
    fn from(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }
}
impl Text {
    pub(crate) fn insert(&mut self, value: &str) {
        self.value.insert_str(self.cursor, value);
        self.cursor += value.len();
    }
    pub(crate) fn left(&mut self) {
        self.cursor = self.value[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
    pub(crate) fn right(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }
    pub(crate) fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.value.replace_range(self.cursor..end, "");
    }
    pub(crate) fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        let end = self.cursor;
        self.value.replace_range(start..end, "");
        self.cursor = start;
    }
    pub(crate) fn home(&mut self) {
        self.cursor = self.value[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }
    pub(crate) fn end(&mut self) {
        self.cursor += self.value[self.cursor..]
            .find('\n')
            .unwrap_or(self.value.len() - self.cursor);
    }
    pub(crate) fn vertical(&mut self, down: bool) {
        let start = self.value[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let column = self.value[start..self.cursor].chars().count();
        let target = if down {
            let Some(end) = self.value[self.cursor..].find('\n') else {
                return;
            };
            self.cursor + end + 1
        } else {
            if start == 0 {
                return;
            }
            self.value[..start - 1]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0)
        };
        let line = self.value[target..].split('\n').next().unwrap_or_default();
        self.cursor = target
            + line
                .char_indices()
                .nth(column)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
    }
}

struct Draft {
    provider: Provider,
    model: Text,
    reasoning: Text,
    prompt: Text,
    original_prompt: String,
    base_url: Text,
    key_env: Text,
    tokens: Text,
    tool_rounds: Text,
    skills: Text,
}
pub struct Editor {
    config: Config,
    drafts: Vec<Draft>,
    selected: usize,
    focus: usize,
    pub notice: String,
    failed: bool,
    connection: bool,
    live: bool,
    models_only: bool,
    manual_model: bool,
    picker: Option<crate::picker::Picker>,
    catalogs: std::collections::HashMap<String, crate::catalog::Catalog>,
}
impl Editor {
    pub fn new(config: &Config, path: &Path) -> Result<Self> {
        let root = path.parent().unwrap_or(Path::new("."));
        let drafts = std::iter::once(&config.leader)
            .chain(&config.agents)
            .map(|a| {
                let prompt = a.role_prompt()?;
                Ok(Draft {
                    provider: a.provider.clone(),
                    base_url: a
                        .api
                        .as_ref()
                        .and_then(|a| a.base_url.clone())
                        .unwrap_or_default()
                        .into(),
                    key_env: a
                        .api
                        .as_ref()
                        .and_then(|a| a.api_key_env.clone())
                        .map(|v| if v.is_empty() { "-".into() } else { v })
                        .unwrap_or_default()
                        .into(),
                    tokens: a
                        .api
                        .as_ref()
                        .and_then(|a| a.max_output_tokens)
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                        .into(),
                    tool_rounds: a
                        .api
                        .as_ref()
                        .and_then(|a| a.max_tool_rounds)
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                        .into(),
                    model: a.model.clone().unwrap_or_default().into(),
                    reasoning: a.reasoning.clone().unwrap_or_default().into(),
                    prompt: prompt.clone().into(),
                    original_prompt: prompt,
                    skills: a
                        .skills
                        .iter()
                        .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            config: config.clone(),
            drafts,
            selected: 0,
            focus: 0,
            notice:
                "Edit any agent. Ctrl+S saves the entire team. Esc closes without saving edits."
                    .into(),
            failed: false,
            connection: false,
            live: false,
            models_only: false,
            manual_model: false,
            picker: None,
            catalogs: Default::default(),
        })
    }
    pub fn save(&mut self, path: &Path) -> Result<Config> {
        for index in 0..self.drafts.len() {
            self.sync_skills_prompt(index, path);
        }
        let root = path.parent().unwrap_or(Path::new("."));
        let mut config = self.config.clone();
        config.setup_pending = false;
        // Validate all fields and paths before creating any prompt overrides.
        for (a, d) in std::iter::once(&mut config.leader)
            .chain(&mut config.agents)
            .zip(&self.drafts)
        {
            a.provider = d.provider.clone();
            a.model = optional(&d.model.value);
            a.reasoning = optional(&d.reasoning.value);
            a.api = if d.provider.is_api() {
                Some(crate::config::ApiConfig {
                    base_url: optional(&d.base_url.value),
                    // An empty override on a custom localhost connection means no key.
                    api_key_env: if d.key_env.value.trim() == "-" {
                        Some(String::new())
                    } else {
                        optional(&d.key_env.value)
                    },
                    max_output_tokens: optional(&d.tokens.value)
                        .map(|s| s.parse())
                        .transpose()
                        .context("Output tokens must be an integer")?,
                    max_tool_rounds: optional(&d.tool_rounds.value)
                        .map(|s| s.parse())
                        .transpose()
                        .context("Tool rounds must be an integer")?,
                })
            } else {
                None
            };
            a.skills = d
                .skills
                .value
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| crate::config::resolve_skill(root, Path::new(s)))
                .collect();
            for skill in &a.skills {
                fs::read_to_string(skill).map_err(|error| {
                    anyhow::anyhow!(
                        "{}: cannot read skill '{}': {}\nPath: {}",
                        a.name,
                        skill.file_name().unwrap_or_default().to_string_lossy(),
                        error,
                        skill.strip_prefix(root).unwrap_or(skill).display()
                    )
                })?;
            }
        }
        config.validate()?;
        let mut created = Vec::new();
        for (a, d) in std::iter::once(&mut config.leader)
            .chain(&mut config.agents)
            .zip(&self.drafts)
        {
            if d.prompt.value != d.original_prompt {
                // New immutable file per save: existing external/shared prompts are never overwritten.
                let folder = root.join("custom-contexts");
                fs::create_dir_all(&folder)?;
                let target = folder.join(format!("{}-{}.md", a.name, uuid::Uuid::new_v4()));
                fs::write(&target, &d.prompt.value)?;
                created.push(target.clone());
                a.context = target;
            }
        }
        if let Err(error) = config.save(path) {
            for path in created {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        self.config = config.clone();
        for d in &mut self.drafts {
            d.original_prompt = d.prompt.value.clone();
        }
        Ok(config)
    }
    fn sync_skills_prompt(&mut self, index: usize, path: &Path) {
        let root = path.parent().unwrap_or(Path::new("."));
        let draft = &mut self.drafts[index];
        let skills = draft
            .skills
            .value
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| crate::config::resolve_skill(root, Path::new(s)))
            .collect::<Vec<_>>();
        draft.prompt.value = crate::prompts::with_skills(&draft.prompt.value, &skills);
        draft.prompt.cursor = draft.prompt.cursor.min(draft.prompt.value.len());
        while !draft.prompt.value.is_char_boundary(draft.prompt.cursor) {
            draft.prompt.cursor -= 1;
        }
    }
    fn text_mut(&mut self) -> Option<&mut Text> {
        let d = &mut self.drafts[self.selected];
        if self.connection {
            return match self.focus {
                2 => Some(&mut d.model),
                3 => Some(&mut d.reasoning),
                4 => Some(&mut d.base_url),
                5 => Some(&mut d.key_env),
                6 => Some(&mut d.tokens),
                7 => Some(&mut d.tool_rounds),
                _ => None,
            };
        }
        match self.focus {
            2 => Some(&mut d.model),
            3 => Some(&mut d.reasoning),
            4 => Some(&mut d.prompt),
            5 => Some(&mut d.skills),
            _ => None,
        }
    }
    fn catalog_connection(&self) -> (crate::config::Agent, String) {
        self.catalog_connection_for(self.selected)
    }
    fn catalog_connection_for(&self, index: usize) -> (crate::config::Agent, String) {
        let draft = &self.drafts[index];
        let mut agent = std::iter::once(&self.config.leader)
            .chain(&self.config.agents)
            .nth(index)
            .unwrap()
            .clone();
        agent.provider = draft.provider.clone();
        agent.model = optional(&draft.model.value);
        agent.api = if draft.provider.is_api() {
            Some(crate::config::ApiConfig {
                base_url: optional(&draft.base_url.value),
                api_key_env: if draft.key_env.value.trim() == "-" {
                    Some(String::new())
                } else {
                    optional(&draft.key_env.value)
                },
                ..Default::default()
            })
        } else {
            None
        };
        let key = serde_json::json!({"provider":agent.provider,"api":agent.api}).to_string();
        (agent, key)
    }
    fn open_models(&mut self, refresh: bool) -> Result<()> {
        let (agent, key) = self.catalog_connection();
        if refresh {
            self.catalogs.remove(&key);
        }
        self.picker = Some(crate::picker::Picker::models(
            &agent,
            key.clone(),
            self.catalogs.get(&key).cloned(),
        )?);
        Ok(())
    }
    fn choose_provider(&mut self, provider: Provider) {
        let draft = &mut self.drafts[self.selected];
        if draft.provider != provider {
            draft.provider = provider;
            draft.model = Text::default();
            draft.reasoning = Text::default();
            draft.base_url = Text::default();
            draft.key_env = Text::default();
        }
        self.focus = 2;
        self.manual_model = false;
        self.notice =
            "Provider chosen. Press Enter to choose a model; Ctrl+S saves the team.".into();
    }
    fn selected_model(&self) -> Option<&crate::catalog::Model> {
        let (_, key) = self.catalog_connection();
        self.catalogs
            .get(&key)?
            .models
            .iter()
            .find(|m| m.id == self.drafts[self.selected].model.value)
    }
    fn model_label(&self, index: usize) -> String {
        let id = &self.drafts[index].model.value;
        if id.is_empty() {
            return if self.drafts[index].provider.is_api() {
                "Choose a model…"
            } else {
                "Account default"
            }
            .into();
        }
        // Catalog labels are presentation only. The saved value remains the exact ID.
        let (_, key) = self.catalog_connection_for(index);
        self.catalogs
            .get(&key)
            .into_iter()
            .flat_map(|c| &c.models)
            .find(|m| &m.id == id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| id.clone())
    }
    fn poll_picker(&mut self) {
        if let Some(picker) = &mut self.picker
            && let Some(catalog) = picker.poll()
            && let Some(key) = &picker.connection
        {
            self.catalogs.insert(key.clone(), catalog);
        }
    }
    fn picker_key(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        use crate::picker::{Outcome, Selection};
        let outcome = self.picker.as_mut().unwrap().handle(key);
        match outcome {
            Outcome::None => {}
            Outcome::Cancel => self.picker = None,
            Outcome::Providers => {
                self.picker = Some(crate::picker::Picker::providers(
                    &self.drafts[self.selected].provider,
                ))
            }
            Outcome::Refresh => {
                self.picker = None;
                self.open_models(true)?;
            }
            Outcome::Select(selection) => {
                self.picker = None;
                match selection {
                    Selection::Provider(provider) => {
                        self.choose_provider(provider);
                        self.open_models(false)?;
                    }
                    Selection::Model(model) => {
                        let draft = &mut self.drafts[self.selected];
                        if (draft.model.value != model.id || !model.efforts.is_empty())
                            && !model
                                .efforts
                                .iter()
                                .any(|effort| effort == &draft.reasoning.value)
                        {
                            draft.reasoning = Text::default();
                        }
                        draft.model = model.id.into();
                        self.notice = format!(
                            "Selected {}. Ctrl+S saves; current calls continue unchanged.",
                            model.name
                        );
                        self.manual_model = false;
                    }
                    Selection::Effort(effort) => {
                        self.drafts[self.selected].reasoning = effort.unwrap_or_default().into();
                    }
                }
            }
        }
        Ok(())
    }
    fn paste(&mut self, text: &str) {
        let multiline = !self.connection && self.focus >= 4;
        let text: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|c| !c.is_control() || (multiline && *c == '\n'))
            .collect();
        if let Some(field) = self.text_mut() {
            field.insert(&text);
        }
    }
}
fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

pub fn run<B: Backend>(terminal: &mut Terminal<B>, config: &mut Config, path: &Path) -> Result<()> {
    run_at(terminal, config, path, 0, None)
}

pub fn run_at<B: Backend>(
    terminal: &mut Terminal<B>,
    config: &mut Config,
    path: &Path,
    selected: usize,
    live_config: Option<&std::sync::Arc<std::sync::RwLock<Config>>>,
) -> Result<()> {
    let live = live_config.is_some();
    let first_setup = config.setup_pending;
    let mut editor = Editor::new(config, path)?;
    editor.selected = selected.min(editor.drafts.len().saturating_sub(1));
    editor.live = live;
    editor.models_only = live;
    if live {
        editor.focus = 2;
        editor.notice = "Enter chooses provider/model from a searchable list. Alt+↑↓ changes agent. Ctrl+S saves; Esc returns to logs.".into();
        editor.open_models(false)?;
    }
    if first_setup {
        editor.notice = "Configure the team. Tab to Provider / Model, then Enter to choose from a list. Ctrl+S saves and continues; Esc exits.".into();
    }
    loop {
        editor.poll_picker();
        terminal.draw(|f| {
            render(f, &editor);
            if let Some(picker) = &editor.picker {
                picker.render(f);
            }
        })?;
        if editor.picker.is_some() && !event::poll(std::time::Duration::from_millis(100))? {
            continue;
        }
        let input = event::read()?;
        if let Event::Paste(text) = &input {
            if let Some(picker) = &mut editor.picker {
                picker.paste(text);
                continue;
            }
            if editor.focus == 2 && !editor.manual_model {
                editor.open_models(false)?;
                editor.picker.as_mut().unwrap().paste(text);
                continue;
            }
            editor.paste(text);
            if !editor.connection && editor.focus == 5 {
                editor.sync_skills_prompt(editor.selected, path);
            }
            continue;
        }
        let Event::Key(key) = input else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if editor.picker.is_some()
            && !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            editor.picker_key(key)?;
            continue;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(());
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            match editor.save(path) {
                Ok(saved) => {
                    if let Some(shared) = live_config {
                        *shared.write().unwrap_or_else(|e| e.into_inner()) = saved.clone();
                    }
                    *config = saved;
                    if first_setup {
                        return Ok(());
                    }
                    editor.notice = if live { "Saved. Changes apply on the next invocation; current calls keep their model. Esc returns to logs." } else { "Saved. These prompts and skills will be used on the next run." }.into();
                    editor.failed = false;
                }
                Err(error) => {
                    editor.notice = format!("Not saved: {error:#}");
                    editor.failed = true;
                }
            }
            continue;
        }
        let fields = if editor.connection {
            8
        } else if editor.models_only {
            4
        } else {
            6
        };
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            let count = editor.drafts.len();
            editor.selected = (editor.selected
                + if key.code == KeyCode::Down {
                    1
                } else {
                    count - 1
                })
                % count;
            continue;
        }
        match key.code {
            KeyCode::F(4) if editor.focus == 2 => {
                editor.manual_model = !editor.manual_model;
                editor.notice = if editor.manual_model {
                    "Advanced custom model ID entry. F4 returns to the catalog selector."
                } else {
                    "Model selector. Enter opens the catalog."
                }
                .into();
            }
            KeyCode::Enter | KeyCode::Char(' ') if editor.focus == 1 => {
                editor.picker = Some(crate::picker::Picker::providers(
                    &editor.drafts[editor.selected].provider,
                ));
            }
            KeyCode::Enter | KeyCode::Char(' ') if editor.focus == 2 && !editor.manual_model => {
                editor.open_models(false)?
            }
            KeyCode::Enter if editor.focus == 3 => {
                let efforts = if matches!(
                    editor.drafts[editor.selected].provider,
                    Provider::Codex | Provider::Openai
                ) {
                    editor
                        .selected_model()
                        .map(|m| m.efforts.clone())
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                editor.picker = Some(crate::picker::Picker::efforts(
                    efforts,
                    &editor.drafts[editor.selected].reasoning.value,
                ));
            }
            KeyCode::Char(c)
                if editor.focus == 2
                    && !editor.manual_model
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                editor.open_models(false)?;
                editor.picker.as_mut().unwrap().paste(&c.to_string());
            }
            KeyCode::Backspace | KeyCode::Delete | KeyCode::Char('u')
                if editor.focus == 2 && !editor.manual_model => {}
            KeyCode::F(3) if live => {
                editor.connection = false;
                editor.models_only = !editor.models_only;
                editor.focus = if editor.models_only { 2 } else { 4 };
            }
            KeyCode::F(2) => {
                editor.connection = !editor.connection;
                editor.focus = 0;
            }
            KeyCode::Tab => editor.focus = (editor.focus + 1) % fields,
            KeyCode::BackTab => editor.focus = (editor.focus + fields - 1) % fields,
            KeyCode::Down if editor.focus == 0 => {
                editor.selected = (editor.selected + 1) % editor.drafts.len()
            }
            KeyCode::Up if editor.focus == 0 => {
                editor.selected = (editor.selected + editor.drafts.len() - 1) % editor.drafts.len()
            }
            KeyCode::Enter if editor.focus == 0 => {
                editor.focus = if editor.models_only { 2 } else { 4 }
            }
            KeyCode::Left | KeyCode::Right if editor.focus == 1 => {
                let d = &editor.drafts[editor.selected];
                let index = Provider::ALL
                    .iter()
                    .position(|p| *p == d.provider)
                    .unwrap_or(0);
                let offset = if key.code == KeyCode::Left {
                    Provider::ALL.len() - 1
                } else {
                    1
                };
                editor
                    .choose_provider(Provider::ALL[(index + offset) % Provider::ALL.len()].clone());
                editor.focus = 1;
            }
            code => {
                let multiline = !editor.connection && editor.focus >= 4;
                if let Some(field) = editor.text_mut() {
                    match code {
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            *field = Text::default()
                        }
                        KeyCode::Char(c)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                && !c.is_control() =>
                        {
                            field.insert(&c.to_string())
                        }
                        KeyCode::Enter if multiline => field.insert("\n"),
                        KeyCode::Backspace => field.backspace(),
                        KeyCode::Delete => field.delete(),
                        KeyCode::Left => field.left(),
                        KeyCode::Right => field.right(),
                        KeyCode::Up => field.vertical(false),
                        KeyCode::Down => field.vertical(true),
                        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            field.cursor = 0
                        }
                        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            field.cursor = field.value.len()
                        }
                        KeyCode::PageUp => {
                            for _ in 0..8 {
                                field.vertical(false);
                            }
                        }
                        KeyCode::PageDown => {
                            for _ in 0..8 {
                                field.vertical(true);
                            }
                        }
                        KeyCode::Home => field.home(),
                        KeyCode::End => field.end(),
                        _ => {}
                    }
                }
            }
        }
        if !editor.connection && editor.focus == 5 {
            editor.sync_skills_prompt(editor.selected, path);
        }
    }
}
fn block(title: &str, active: bool) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(if active { TEXT } else { DIM }),
        ))
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if active { TEXT } else { LINE }))
        .style(Style::default().fg(TEXT))
}
fn field(frame: &mut Frame, area: Rect, title: &str, text: &Text, active: bool) {
    let block = block(title, active);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width.max(1) as usize;
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut col = 0;
    let mut cursor = (0, 0);
    for (index, c) in text.value.char_indices() {
        let w = c.width().unwrap_or(0);
        if c != '\n' && col + w > width {
            lines.push(std::mem::take(&mut current));
            col = 0;
        }
        if index == text.cursor {
            cursor = (lines.len(), col);
        }
        if c == '\n' {
            lines.push(std::mem::take(&mut current));
            col = 0;
        } else {
            current.push(c);
            col += w;
        }
    }
    if text.cursor == text.value.len() {
        if col >= width {
            lines.push(std::mem::take(&mut current));
            col = 0;
        }
        cursor = (lines.len(), col);
    }
    lines.push(current);
    let sy = if active {
        cursor
            .0
            .saturating_sub(inner.height.saturating_sub(1) as usize)
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .scroll((sy.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
    if active && inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((
            inner.x + cursor.1.min(width - 1) as u16,
            inner.y + (cursor.0 - sy) as u16,
        ));
    }
}

fn model_field(frame: &mut Frame, area: Rect, editor: &Editor) {
    if editor.manual_model {
        field(
            frame,
            area,
            "Custom model ID / F4 returns to list",
            &editor.drafts[editor.selected].model,
            editor.focus == 2,
        );
    } else {
        frame.render_widget(
            Paragraph::new(editor.model_label(editor.selected))
                .block(block("Model / Enter choose", editor.focus == 2))
                .style(Style::default().fg(TEXT)),
            area,
        );
    }
}

pub fn render(frame: &mut Frame, editor: &Editor) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().fg(TEXT).bg(BG)),
        area,
    );
    if area.width < 60 || area.height < 20 {
        frame.render_widget(
            Paragraph::new("Agent setup — resize to 60 × 20. Esc closes."),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(10),
        Constraint::Length(if editor.failed { 6 } else { 3 }),
    ])
    .margin(1)
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "ai-lead / agent setup",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if editor.live {
                    "    models / providers · next invocation"
                } else {
                    "    configure before launch"
                },
                Style::default().fg(DIM),
            ),
        ])),
        rows[0],
    );
    let columns = Layout::horizontal([Constraint::Length(20), Constraint::Min(30)])
        .spacing(2)
        .split(rows[1]);
    let names: Vec<_> = std::iter::once(&editor.config.leader)
        .chain(&editor.config.agents)
        .enumerate()
        .map(|(index, a)| {
            if editor.models_only && columns[0].height >= 15 {
                ListItem::new(vec![
                    Line::from(a.name.clone()),
                    Line::from(Span::styled(
                        editor.model_label(index),
                        Style::default().fg(DIM),
                    )),
                ])
            } else {
                ListItem::new(a.name.clone())
            }
        })
        .collect();
    let mut selection = ListState::default().with_selected(Some(editor.selected));
    frame.render_stateful_widget(
        List::new(names)
            .block(block("Agents", editor.focus == 0))
            .highlight_symbol("› ")
            .highlight_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
        columns[0],
        &mut selection,
    );
    let compact = columns[1].height < 14;
    let panes = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(if compact { 3 } else { 5 }),
    ])
    .spacing(if compact { 0 } else { 1 })
    .split(columns[1]);
    let top = Layout::horizontal([
        Constraint::Percentage(36),
        Constraint::Percentage(38),
        Constraint::Percentage(26),
    ])
    .spacing(1)
    .split(panes[0]);
    let d = &editor.drafts[editor.selected];
    frame.render_widget(
        Paragraph::new(d.provider.label())
            .block(block("Provider / Enter choose", editor.focus == 1)),
        top[0],
    );
    model_field(frame, top[1], editor);
    field(
        frame,
        top[2],
        "Effort / Codex, OpenAI",
        &d.reasoning,
        editor.focus == 3,
    );
    if editor.connection {
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(columns[1]);
        let top = Layout::horizontal([
            Constraint::Percentage(36),
            Constraint::Percentage(38),
            Constraint::Percentage(26),
        ])
        .spacing(1)
        .split(rows[0]);
        frame.render_widget(
            Paragraph::new(d.provider.label())
                .block(block("Provider / Enter choose", editor.focus == 1)),
            top[0],
        );
        model_field(frame, top[1], editor);
        field(frame, top[2], "Effort", &d.reasoning, editor.focus == 3);
        let (base, key) = d.provider.defaults();
        field(
            frame,
            rows[1],
            &format!("Base URL / blank: {base}"),
            &d.base_url,
            editor.focus == 4,
        );
        let bottom = Layout::horizontal([
            Constraint::Percentage(60),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
        ])
        .spacing(1)
        .split(rows[2]);
        field(
            frame,
            bottom[0],
            &format!("Key ENV / {key}"),
            &d.key_env,
            editor.focus == 5,
        );
        field(
            frame,
            bottom[1],
            "Tokens/8192",
            &d.tokens,
            editor.focus == 6,
        );
        field(
            frame,
            bottom[2],
            "Turns/40",
            &d.tool_rounds,
            editor.focus == 7,
        );
        frame.render_widget(Paragraph::new(if d.provider.is_api() {format!("API preset: {}\n{}\nKey variable: {}\nEnter the environment variable NAME, never the secret key. Blank fields use preset defaults; Key ENV = - for anonymous localhost. Model must support tool calling.",d.provider.label(),base,key)} else {"CLI connection: sign in using codex / claude. API fields are unused. Model is optional (CLI default).".into()}).wrap(Wrap{trim:false}).style(Style::default().fg(DIM)), rows[3]);
    } else if editor.models_only {
        frame.render_widget(ratatui::widgets::Clear, columns[1]);
        frame.render_widget(Block::default().style(Style::default().bg(BG)), columns[1]);
        let model_rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .spacing(1)
        .split(columns[1]);
        frame.render_widget(
            Paragraph::new(d.provider.label())
                .block(block("Provider / Enter choose", editor.focus == 1)),
            model_rows[0],
        );
        model_field(frame, model_rows[1], editor);
        field(
            frame,
            model_rows[2],
            "Reasoning / Codex and OpenAI",
            &d.reasoning,
            editor.focus == 3,
        );
        frame.render_widget(Paragraph::new(format!("Agent: {}\nEnter chooses provider/model from a searchable list.\nModel IDs are saved automatically.\nAlt+↑↓ switches agents. Ctrl+S applies to next calls.\nF2: API connection. F3: prompts and skills.", std::iter::once(&editor.config.leader).chain(&editor.config.agents).nth(editor.selected).unwrap().name)).style(Style::default().fg(DIM)).wrap(Wrap { trim: false }), model_rows[3]);
    } else {
        field(
            frame,
            panes[1],
            "Full role prompt / multiline",
            &d.prompt,
            editor.focus == 4,
        );
        field(
            frame,
            panes[2],
            "Skills / one file path per line",
            &d.skills,
            editor.focus == 5,
        );
    }
    let footer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(rows[2]);
    frame.render_widget(
        Paragraph::new(editor.notice.as_str())
            .style(Style::default().fg(if editor.failed { Color::LightRed } else { DIM }))
            .wrap(Wrap { trim: false }),
        footer[0],
    );
    frame.render_widget(
        Paragraph::new(if editor.live {
            "Alt+↑↓ agent · Tab fields · F2 API · F3 roles · Ctrl+S save · Esc back"
        } else {
            "F2 connection/role · Tab fields · Ctrl+S save · Esc close"
        })
        .style(Style::default().fg(DIM)),
        footer[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saves_custom_prompt_and_skills_for_only_the_selected_role() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        let original = fs::read_to_string(&config.agents[0].context).unwrap();
        let skill = path.parent().unwrap().join("strict.md");
        fs::write(&skill, "Strict custom skill").unwrap();
        let mut editor = Editor::new(&config, &path).unwrap();
        editor.drafts[1].prompt = "Строгий промпт\nDo exactly this.".to_owned().into();
        editor.drafts[1].skills = "strict.md".to_owned().into();
        editor.save(&path).unwrap();
        let saved = Config::load(&path).unwrap();
        assert!(
            saved.agents[0]
                .instructions()
                .unwrap()
                .contains("Строгий промпт")
        );
        assert!(
            saved.agents[0]
                .instructions()
                .unwrap()
                .contains("Strict custom skill")
        );
        assert_eq!(
            fs::read_to_string(&config.agents[0].context).unwrap(),
            original
        );
        assert_eq!(saved.agents[1].context, config.agents[1].context);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("custom-contexts/")
        );
    }
    #[test]
    fn attached_skill_directories_update_the_settings_prompt_without_duplication() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        let custom = path.parent().unwrap().join("custom-rules");
        fs::create_dir(&custom).unwrap();
        fs::write(custom.join("SKILL.md"), "My custom workflow").unwrap();
        let mut editor = Editor::new(&config, &path).unwrap();
        editor.drafts[1].prompt = "Keep my exact role instructions".to_owned().into();
        editor.drafts[1].skills = "custom-rules".to_owned().into();
        editor.sync_skills_prompt(1, &path);
        assert!(editor.drafts[1].prompt.value.contains("custom-rules"));
        assert!(
            editor.drafts[1]
                .prompt
                .value
                .contains("Read all attached skills")
        );
        assert!(!editor.drafts[1].prompt.value.contains("skills/designer.md"));
        editor.save(&path).unwrap();
        let saved = Config::load(&path).unwrap();
        let raw = fs::read_to_string(&saved.agents[0].context).unwrap();
        assert_eq!(
            crate::prompts::without_skills(&raw),
            "Keep my exact role instructions"
        );
        assert_eq!(raw.matches(crate::prompts::START).count(), 1);
        assert!(
            saved.agents[0]
                .instructions()
                .unwrap()
                .contains("My custom workflow")
        );
        editor.drafts[1].skills = Text::default();
        editor.save(&path).unwrap();
        let saved = Config::load(&path).unwrap();
        assert!(saved.agents[0].skills.is_empty());
        assert!(
            saved.agents[0]
                .role_prompt()
                .unwrap()
                .contains("No skills are attached")
        );
    }
    #[test]
    fn invalid_skill_does_not_change_saved_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        let before = fs::read(&path).unwrap();
        let mut editor = Editor::new(&config, &path).unwrap();
        editor.drafts[0].skills = "missing.md".to_owned().into();
        assert!(editor.save(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    #[test]
    fn deleted_skill_can_be_repaired_in_editor() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let initial = Config::load(&path).unwrap();
        fs::remove_file(&initial.agents[0].skills[0]).unwrap();
        assert!(Config::load(&path).is_err());
        let editable = Config::load_for_edit(&path).unwrap();
        let mut editor = Editor::new(&editable, &path).unwrap();
        editor.drafts[1].skills = Text::default();
        editor.save(&path).unwrap();
        assert!(Config::load(&path).unwrap().agents[0].skills.is_empty());
    }
    #[test]
    fn save_error_exposes_filename_and_cause_in_small_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        let mut editor = Editor::new(&config, &path).unwrap();
        editor.drafts[1].skills = "missing.md".to_owned().into();
        editor.notice = format!("Not saved: {:#}", editor.save(&path).unwrap_err());
        editor.failed = true;
        editor.selected = 1;
        editor.focus = 5;
        for (w, h) in [(80, 24), (60, 20)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| render(f, &editor)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(text.contains("missing.md"));
            assert!(
                text.matches("missing.md").count() >= 2,
                "The skill field must stay visible along with its error: {text}"
            );
            assert!(text.contains("os error 2"), "OS cause is hidden: {text}");
        }
    }
    #[test]
    fn unicode_editor_preserves_multiline_text() {
        let mut t: Text = "Привет\n世界".to_owned().into();
        t.left();
        t.insert("!");
        assert_eq!(t.value, "Привет\n世!界");
        t.home();
        t.vertical(false);
        t.insert("X");
        assert!(t.value.starts_with('X'));
        t.delete();
        t.end();
        t.backspace();
        assert!(t.value.is_char_boundary(t.cursor));
    }
    #[test]
    fn setup_renders_at_terminal_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        let editor = Editor::new(&config, &path).unwrap();
        for (w, h) in [(120, 40), (80, 24), (60, 20)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| render(f, &editor)).unwrap();
        }
    }
    #[test]
    fn api_connection_editor_saves_each_role_and_preserves_anonymous_local_auth() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::config::init(dir.path(), Provider::Codex).unwrap();
        let config = Config::load(&path).unwrap();
        let mut editor = Editor::new(&config, &path).unwrap();
        editor.drafts[0].provider = Provider::Openai;
        editor.drafts[0].model = "lead-account-model".to_owned().into();
        editor.drafts[0].reasoning = "high".to_owned().into();
        editor.drafts[1].provider = Provider::Compatible;
        editor.drafts[1].model = "local-model".to_owned().into();
        editor.drafts[1].base_url = "http://localhost:1234/v1".to_owned().into();
        editor.drafts[1].key_env = "-".to_owned().into();
        editor.drafts[1].tokens = "4096".to_owned().into();
        editor.drafts[1].tool_rounds = "12".to_owned().into();
        editor.save(&path).unwrap();
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.leader.provider, Provider::Openai);
        assert_eq!(saved.agents[1].provider, Provider::Codex);
        assert_eq!(
            saved.agents[0].api.as_ref().unwrap().max_tool_rounds,
            Some(12)
        );
        assert_eq!(crate::api::settings(&saved.agents[0]).unwrap().key_env, "");
        let mut reopened = Editor::new(&saved, &path).unwrap();
        reopened.save(&path).unwrap();
        assert_eq!(
            crate::api::settings(&Config::load(&path).unwrap().agents[0])
                .unwrap()
                .key_env,
            ""
        );
        reopened.connection = true;
        reopened.selected = 1;
        for (width, height) in [(60, 20), (80, 24), (120, 40)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| render(f, &reopened)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("local-model"));
            assert!(text.contains("localhost:1234"));
            assert!(text.contains("4096"));
        }
    }
}
