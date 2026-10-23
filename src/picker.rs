//! Searchable selectors for connection presets, models and reasoning levels.
use crate::{
    catalog::{Catalog, Model},
    config::{Agent, Provider},
    setup::Text,
};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::sync::mpsc::{Receiver, channel};

#[derive(Clone)]
pub enum Selection {
    Provider(Provider),
    Model(Model),
    Effort(Option<String>),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_uses_names_but_preserves_ids_and_empty_search_renders_at_terminal_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            crate::config::Config::load(&crate::config::init(dir.path(), Provider::Codex).unwrap())
                .unwrap();
        let catalog = Catalog {
            models: vec![Model {
                id: "opaque-internal-id".into(),
                name: "Friendly Coding Model".into(),
                description: "For project work".into(),
                efforts: vec!["medium".into()],
                recommended: false,
            }],
            source: "Test catalog".into(),
            note: String::new(),
        };
        let mut picker = Picker::models(&config.leader, "test".into(), Some(catalog)).unwrap();
        picker.paste("friendly coding");
        assert!(
            matches!(picker.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Outcome::Select(Selection::Model(model)) if model.id == "opaque-internal-id")
        );
        picker.paste("no matches");
        assert!(matches!(
            picker.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::None
        ));
        for (width, height) in [(120, 40), (80, 24), (60, 20)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| picker.render(frame)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Choose model"));
            assert!(text.contains("No matching models"));
        }
        let mut providers = Picker::providers(&Provider::Codex);
        providers.paste("claude code");
        assert!(matches!(
            providers.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Select(Selection::Provider(Provider::Claude))
        ));
    }
}
#[derive(Clone)]
struct Choice {
    title: String,
    description: String,
    search: String,
    value: Selection,
}
pub enum Outcome {
    None,
    Cancel,
    Select(Selection),
    Refresh,
    Providers,
}
pub struct Picker {
    title: String,
    query: Text,
    choices: Vec<Choice>,
    selected: usize,
    pub source: String,
    pub note: String,
    pub connection: Option<String>,
    cli: bool,
    current: String,
    loading: bool,
    receiver: Option<Receiver<std::result::Result<Catalog, String>>>,
    job: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Picker {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.abort();
        }
    }
}
impl Picker {
    fn new(title: &str) -> Self {
        Self {
            title: title.into(),
            query: Text::default(),
            choices: vec![],
            selected: 0,
            source: String::new(),
            note: String::new(),
            connection: None,
            cli: false,
            current: String::new(),
            loading: false,
            receiver: None,
            job: None,
        }
    }
    pub fn providers(current: &Provider) -> Self {
        let mut picker = Self::new("Choose provider");
        picker.choices = Provider::ALL
            .into_iter()
            .map(|provider| {
                let description = if provider.is_api() {
                    let (_, key) = provider.defaults();
                    if provider == Provider::Compatible {
                        "Your local or custom API connection".into()
                    } else {
                        format!("Direct API connection · key: {key}")
                    }
                } else {
                    "Use the installed CLI and its signed-in account".into()
                };
                Choice {
                    title: provider.label().into(),
                    description,
                    search: provider.label().to_lowercase(),
                    value: Selection::Provider(provider),
                }
            })
            .collect();
        picker.selected = picker.choices.iter().position(|choice| matches!(&choice.value, Selection::Provider(provider) if provider == current)).unwrap_or(0);
        picker.source = "Connection presets · no provider ID needed".into();
        picker
    }
    pub fn models(agent: &Agent, connection: String, cached: Option<Catalog>) -> Result<Self> {
        let mut picker = Self::new("Choose model");
        picker.cli = !agent.provider.is_api();
        picker.current = agent.model.clone().unwrap_or_default();
        picker.connection = Some(connection);
        if picker.cli {
            picker.add_models(vec![crate::catalog::default_model()]);
        }
        if let Some(catalog) = cached {
            picker.install(&catalog);
        } else {
            let agent = agent.clone();
            let (tx, rx) = channel();
            picker.loading = true;
            picker.source = format!("Loading {} models…", agent.provider.label());
            picker.note =
                "You can search while loading. Esc returns without changing the model.".into();
            picker.receiver = Some(rx);
            picker.job = Some(tokio::runtime::Handle::try_current()?.spawn(async move {
                let result = crate::catalog::discover(&agent)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            }));
        }
        Ok(picker)
    }
    pub fn efforts(efforts: Vec<String>, current: &str) -> Self {
        let mut picker = Self::new("Choose reasoning level");
        picker.choices.push(Choice {
            title: "Automatic".into(),
            description: "Use the model's default reasoning level".into(),
            search: "automatic default".into(),
            value: Selection::Effort(None),
        });
        for effort in efforts {
            let title = match effort.as_str() {
                "low" => "Low",
                "medium" => "Medium",
                "high" => "High",
                "xhigh" => "Extra high",
                "max" => "Maximum",
                "ultra" => "Ultra",
                _ => &effort,
            }
            .to_owned();
            picker.choices.push(Choice {
                search: format!("{title} {effort}").to_lowercase(),
                title,
                description: "Reported by this model's catalog".into(),
                value: Selection::Effort(Some(effort)),
            });
        }
        picker.selected = picker.choices.iter().position(|choice| matches!(&choice.value, Selection::Effort(Some(value)) if value == current)).unwrap_or(0);
        picker.source = "Reasoning options for the selected model".into();
        if picker.choices.len() == 1 {
            picker.note = "The catalog does not report configurable reasoning levels. Automatic leaves the provider default unchanged.".into();
        }
        picker
    }
    fn add_models(&mut self, models: Vec<Model>) {
        self.choices.extend(models.into_iter().map(|model| Choice {
            title: format!(
                "{}{}",
                model.name,
                if model.recommended {
                    " · recommended default"
                } else {
                    ""
                }
            ),
            search: format!("{} {} {}", model.name, model.id, model.description).to_lowercase(),
            description: model.description.clone(),
            value: Selection::Model(model),
        }));
    }
    fn install(&mut self, catalog: &Catalog) {
        self.choices.clear();
        if self.cli {
            self.add_models(vec![crate::catalog::default_model()]);
        }
        self.add_models(catalog.models.clone());
        self.source = catalog.source.clone();
        self.note = catalog.note.clone();
        self.selected = self.filtered().iter().position(|index| matches!(&self.choices[*index].value, Selection::Model(model) if model.id == self.current)).unwrap_or(0);
    }
    pub fn poll(&mut self) -> Option<Catalog> {
        let result = self.receiver.as_ref()?.try_recv().ok()?;
        self.loading = false;
        self.receiver = None;
        match result {
            Ok(catalog) => {
                self.install(&catalog);
                Some(catalog)
            }
            Err(error) => {
                self.source = "Catalog unavailable".into();
                self.note = error;
                None
            }
        }
    }
    fn filtered(&self) -> Vec<usize> {
        let query = self.query.value.to_lowercase();
        self.choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                query
                    .split_whitespace()
                    .all(|word| choice.search.contains(word))
            })
            .map(|(index, _)| index)
            .collect()
    }
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(256usize.saturating_sub(self.query.value.chars().count()))
            .collect();
        self.query.insert(&text);
        self.selected = 0;
    }
    pub fn handle(&mut self, key: KeyEvent) -> Outcome {
        let count = self.filtered().len();
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Tab if self.connection.is_some() => return Outcome::Providers,
            KeyCode::Enter => {
                if let Some(index) = self.filtered().get(self.selected) {
                    return Outcome::Select(self.choices[*index].value.clone());
                }
            }
            KeyCode::Char('r')
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.connection.is_some() =>
            {
                return Outcome::Refresh;
            }
            KeyCode::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => self.selected = (self.selected + 8).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(8),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = count.saturating_sub(1),
            KeyCode::Backspace => {
                self.query.backspace();
                self.selected = 0;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query = Text::default();
                self.selected = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control()
                    && self.query.value.chars().count() < 256 =>
            {
                self.query.insert(&c.to_string());
                self.selected = 0;
            }
            _ => {}
        }
        Outcome::None
    }
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(94);
        let height = area.height.saturating_sub(2).min(27);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let fg = Color::Rgb(218, 220, 226);
        let dim = Color::Rgb(144, 149, 160);
        let accent = Color::Rgb(131, 177, 255);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .style(Style::default().bg(Color::Rgb(18, 19, 22)).fg(fg));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .margin(1)
        .split(inner);
        frame.render_widget(
            Paragraph::new(self.source.as_str()).style(Style::default().fg(dim)),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(format!("Search: {}", self.query.value))
                .style(Style::default().fg(accent)),
            rows[1],
        );
        let indices = self.filtered();
        if indices.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.loading {
                    "Loading model catalog…"
                } else {
                    "No matching models. Clear search, refresh, or check the connection."
                })
                .wrap(Wrap { trim: false }),
                rows[2],
            );
        }
        let items: Vec<_> = indices
            .iter()
            .map(|index| {
                let choice = &self.choices[*index];
                ListItem::new(vec![
                    Line::from(choice.title.clone()),
                    Line::from(Span::styled(
                        choice.description.clone(),
                        Style::default().fg(dim),
                    )),
                ])
            })
            .collect();
        if !items.is_empty() {
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_symbol("› ")
                    .highlight_style(Style::default().fg(accent).add_modifier(Modifier::BOLD)),
                rows[2],
                &mut ListState::default()
                    .with_selected(Some(self.selected.min(indices.len().saturating_sub(1)))),
            );
        }
        frame.render_widget(
            Paragraph::new(self.note.as_str())
                .style(Style::default().fg(dim))
                .wrap(Wrap { trim: false }),
            rows[3],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{} / {} choices · type to search · ↑↓ select\nEnter choose · Esc back{}",
                indices.len(),
                self.choices.len(),
                if self.connection.is_some() {
                    " · Tab providers · Ctrl+R refresh"
                } else {
                    ""
                }
            ))
            .style(Style::default().fg(accent)),
            rows[4],
        );
    }
}
