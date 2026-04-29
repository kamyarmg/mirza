use crate::app::{self, CapturedResponse};
use crate::cli::{Cli, ColorMode, OutputSection, OutputStyle};
use crate::error::AppError;
use clap::{CommandFactory, FromArgMatches};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value as JsonValue;
use std::io::{self, Stdout};
use std::path::PathBuf;
use url::Url;

const INK: Color = Color::Rgb(230, 234, 240);
const MUTED: Color = Color::Rgb(125, 138, 154);
const PANEL: Color = Color::Rgb(42, 53, 67);
const BLUE: Color = Color::Rgb(96, 165, 250);
const GREEN: Color = Color::Rgb(74, 222, 128);
const GOLD: Color = Color::Rgb(250, 204, 21);
const ROSE: Color = Color::Rgb(251, 113, 133);

const HEADER_SUGGESTIONS: [&str; 10] = [
    "Accept: application/json",
    "Authorization: Bearer <token>",
    "Content-Type: application/json",
    "Cookie: name=value",
    "Cache-Control: no-cache",
    "If-None-Match: <etag>",
    "Referer: https://example.com",
    "User-Agent: mirza/0.1.0",
    "X-Request-Id: request-id",
    "X-Trace-Id: trace-id",
];
const DATA_SUGGESTIONS: [&str; 6] = [
    "page=1",
    "limit=20",
    "sort=-created_at",
    "status=active",
    "name=mirza",
    "@payload.txt",
];
const JSON_SUGGESTIONS: [&str; 4] = [
    "{\"name\":\"mirza\"}",
    "{\"page\":1,\"page_size\":20}",
    "{\"enabled\":true}",
    "[{\"id\":1}]",
];
const MULTIPART_SUGGESTIONS: [&str; 4] = [
    "file=@avatar.png",
    "name=mirza",
    "meta=<payload.json",
    "document=@resume.pdf;type=application/pdf",
];
const METHOD_SUGGESTIONS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const OUTPUT_STYLE_SUGGESTIONS: [&str; 5] = ["pretty", "json", "compact", "raw", "auto"];
const COLOR_SUGGESTIONS: [&str; 3] = ["auto", "always", "never"];
const COMMAND_SUGGESTIONS: [&str; 11] = [
    "r",
    "q",
    "w",
    ":run",
    ":w response.json",
    ":tab basic",
    ":tab headers",
    ":tabs horizontal",
    ":tabs vertical",
    ":body json",
    ":body form",
];
const TAB_LAYOUT_SUGGESTIONS: [&str; 2] = ["horizontal", "vertical"];

const BASIC_FIELDS: [BasicField; 7] = [
    BasicField::Url,
    BasicField::Method,
    BasicField::Include,
    BasicField::Location,
    BasicField::Insecure,
    BasicField::Fail,
    BasicField::Compressed,
];

const SETTING_FIELDS: [SettingField; 10] = [
    SettingField::TabLayout,
    SettingField::OutputStyle,
    SettingField::Color,
    SettingField::Retry,
    SettingField::Range,
    SettingField::ContinueAt,
    SettingField::LimitRate,
    SettingField::UserAgent,
    SettingField::Referer,
    SettingField::Proxy,
];

pub fn run(cli: Cli) -> Result<i32, AppError> {
    let mut terminal = setup_terminal()?;
    let result = run_event_loop(&mut terminal, InteractiveState::from_cli(cli));
    restore_terminal(&mut terminal)?;
    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut state: InteractiveState,
) -> Result<i32, AppError> {
    loop {
        terminal
            .draw(|frame| draw(frame, &state))
            .map_err(|error| terminal_error("failed to draw interactive screen", error))?;

        let Event::Key(key) = event::read()
            .map_err(|error| terminal_error("failed to read terminal event", error))?
        else {
            continue;
        };

        if handle_key(&mut state, key)? {
            return Ok(0);
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, AppError> {
    enable_raw_mode().map_err(|error| terminal_error("failed to enable raw mode", error))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|error| terminal_error("failed to enter alternate screen", error))?;
    Terminal::new(CrosstermBackend::new(stdout))
        .map_err(|error| terminal_error("failed to initialize terminal backend", error))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), AppError> {
    disable_raw_mode().map_err(|error| terminal_error("failed to disable raw mode", error))?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(|error| terminal_error("failed to leave alternate screen", error))?;
    terminal
        .show_cursor()
        .map_err(|error| terminal_error("failed to show cursor", error))
}

fn terminal_error(context: &str, error: impl std::fmt::Display) -> AppError {
    AppError::new(1, format!("{context}: {error}"))
}

fn draw(frame: &mut Frame<'_>, state: &InteractiveState) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(frame.area());

    draw_display(frame, sections[0], state);
    draw_command_bar(frame, sections[1], state);
}

fn draw_display(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    match state.tab_layout {
        TabLayout::Horizontal => {
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(5)])
                .split(area);

            let tabs = Line::from(
                Tab::ALL
                    .iter()
                    .flat_map(|tab| {
                        vec![
                            Span::styled(format!(" {} ", tab.label()), tab_style(state, *tab)),
                            Span::raw(" "),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );

            let header = Paragraph::new(tabs).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(PANEL)),
            );
            frame.render_widget(header, sections[0]);
            draw_active_tab(frame, sections[1], state);
        }
        TabLayout::Vertical => {
            let sections = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(16), Constraint::Min(5)])
                .split(area);

            let items = Tab::ALL
                .iter()
                .map(|tab| {
                    ListItem::new(Span::styled(
                        format!(" {} ", tab.label()),
                        tab_style(state, *tab),
                    ))
                })
                .collect::<Vec<_>>();
            let tabs = List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(PANEL))
                    .title(Span::styled("Tabs", Style::default().fg(INK))),
            );
            frame.render_widget(tabs, sections[0]);
            draw_active_tab(frame, sections[1], state);
        }
    }
}

fn draw_active_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    match state.active_tab {
        Tab::Request => draw_request_tab(frame, area, state),
        Tab::Basic => draw_basic_tab(frame, area, state),
        Tab::Headers => draw_collection_tab(
            frame,
            area,
            "Headers",
            &state.headers,
            state.header_index,
            state.active_tab == Tab::Headers,
        ),
        Tab::Body => draw_body_tab(frame, area, state),
        Tab::Params => draw_collection_tab(
            frame,
            area,
            "Query Params",
            &state.params,
            state.param_index,
            state.active_tab == Tab::Params,
        ),
        Tab::Response => draw_response_summary_tab(frame, area, state),
        Tab::Meta => draw_response_meta_tab(frame, area, state),
        Tab::Data => draw_response_data_tab(frame, area, state),
        Tab::Settings => draw_settings_tab(frame, area, state),
    }
}

fn tab_style(state: &InteractiveState, tab: Tab) -> Style {
    if !state.is_tab_enabled(tab) {
        Style::default().fg(Color::DarkGray)
    } else if tab == state.active_tab {
        Style::default()
            .fg(PANEL)
            .bg(BLUE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    }
}

fn draw_request_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let lines = vec![
        Line::from(vec![
            label_span("URL"),
            Span::styled(display_text(&state.url), Style::default().fg(INK)),
        ]),
        Line::from(vec![
            label_span("Method"),
            Span::styled(display_text(&state.method), Style::default().fg(INK)),
        ]),
        Line::from(vec![
            label_span("Headers"),
            Span::styled(
                state.visible_count(&state.headers).to_string(),
                Style::default().fg(INK),
            ),
            Span::raw("    "),
            label_span("Params"),
            Span::styled(
                state.visible_count(&state.params).to_string(),
                Style::default().fg(INK),
            ),
            Span::raw("    "),
            label_span("Body"),
            Span::styled(state.body_mode.label(), Style::default().fg(INK)),
        ]),
        Line::from(vec![
            label_span("Flags"),
            Span::styled(
                format!(
                    "include={} location={} insecure={} fail={} compressed={}",
                    state.include, state.location, state.insecure, state.fail, state.compressed
                ),
                Style::default().fg(INK),
            ),
        ]),
        Line::from(vec![
            label_span("Response"),
            Span::styled(
                state.response_badge(),
                Style::default().fg(state.response_color()),
            ),
        ]),
        Line::from(vec![
            Span::styled("Hint", Style::default().fg(MUTED)),
            Span::raw("  type curl-like options below and press Enter, for example "),
            Span::styled("-H \"Accept: application/json\"", Style::default().fg(INK)),
        ]),
    ];

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(content_block("Request", state.active_tab == Tab::Request));
    frame.render_widget(widget, area);
}

fn draw_basic_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let items = BASIC_FIELDS
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let style = if index == state.basic_index {
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(INK)
            };
            ListItem::new(Line::from(vec![
                label_span(field.label()),
                Span::styled(state.basic_value(*field), style),
            ]))
        })
        .collect::<Vec<_>>();

    let widget = List::new(items).block(content_block("Basic", state.active_tab == Tab::Basic));
    frame.render_widget(widget, area);
}

fn draw_collection_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    items: &[String],
    selected: usize,
    active: bool,
) {
    let rows = if items.is_empty() {
        vec![ListItem::new(Span::styled(
            "<empty>",
            Style::default().fg(MUTED),
        ))]
    } else {
        items
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let style = if index == selected {
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(INK)
                };
                ListItem::new(Span::styled(value.as_str(), style))
            })
            .collect::<Vec<_>>()
    };

    let widget = List::new(rows).block(content_block(title, active));
    frame.render_widget(widget, area);
}

fn draw_body_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(4)])
        .split(area);

    let modes = Line::from(
        BodyMode::ALL
            .iter()
            .flat_map(|mode| {
                let style = if *mode == state.body_mode {
                    Style::default()
                        .fg(PANEL)
                        .bg(GOLD)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED)
                };
                vec![
                    Span::styled(format!(" {} ", mode.label()), style),
                    Span::raw(" "),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let tabs = Paragraph::new(modes).block(
        Block::default()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
            .border_style(Style::default().fg(if state.active_tab == Tab::Body {
                BLUE
            } else {
                PANEL
            }))
            .title(Span::styled("Body", Style::default().fg(INK))),
    );
    frame.render_widget(tabs, sections[0]);

    match state.body_mode {
        BodyMode::Data => draw_collection_tab(
            frame,
            sections[1],
            "Data Fields",
            &state.body_data,
            state.body_data_index,
            true,
        ),
        BodyMode::Form => draw_collection_tab(
            frame,
            sections[1],
            "Multipart Fields",
            &state.body_form,
            state.body_form_index,
            true,
        ),
        BodyMode::Json => draw_text_panel(
            frame,
            sections[1],
            "JSON Payload",
            &display_text(&state.body_json),
            state.active_tab == Tab::Body,
        ),
        BodyMode::Raw => draw_text_panel(
            frame,
            sections[1],
            "Raw Payload",
            &display_text(&state.body_raw),
            state.active_tab == Tab::Body,
        ),
    }
}

fn draw_response_summary_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let text = if let Some(response) = &state.response {
        let header_block = String::from_utf8_lossy(&response.header_block);
        let mut lines = vec![
            format!("Status      : {} {}", response.status, response.reason),
            format!("Method      : {}", response.method),
            format!("URL         : {}", response.url),
            format!("Version     : {}", response.version),
            format!("Headers     : {}", response.headers.len()),
            format!("Body Bytes  : {}", response.body_bytes),
            String::new(),
            String::from("Raw Headers:"),
        ];
        lines.extend(header_block.lines().map(str::to_owned));
        lines.join("\n")
    } else {
        String::from(
            "No response yet. Use `r` or `:run` in the command line to execute the current request.",
        )
    };
    draw_text_panel(
        frame,
        area,
        "Response",
        &text,
        state.active_tab == Tab::Response,
    );
}

fn draw_response_meta_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let text = if let Some(response) = &state.response {
        format_response_meta(response)
    } else {
        String::from("No meta information yet.")
    };
    draw_text_panel(frame, area, "Meta", &text, state.active_tab == Tab::Meta);
}

fn draw_response_data_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let text = if let Some(response) = &state.response {
        response_data_text(response)
    } else {
        String::from("No response body yet.")
    };
    let widget = Paragraph::new(text)
        .scroll((state.response_scroll, 0))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(INK))
        .block(content_block("Data", state.active_tab == Tab::Data));
    frame.render_widget(widget, area);
}

fn draw_settings_tab(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let items = SETTING_FIELDS
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let style = if index == state.settings_index {
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(INK)
            };
            ListItem::new(Line::from(vec![
                label_span(field.label()),
                Span::styled(state.setting_value(*field), style),
            ]))
        })
        .collect::<Vec<_>>();

    let widget =
        List::new(items).block(content_block("Settings", state.active_tab == Tab::Settings));
    frame.render_widget(widget, area);
}

fn draw_text_panel(frame: &mut Frame<'_>, area: Rect, title: &str, text: &str, active: bool) {
    let widget = Paragraph::new(text.to_owned())
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(INK))
        .block(content_block(title, active));
    frame.render_widget(widget, area);
}

fn draw_command_bar(frame: &mut Frame<'_>, area: Rect, state: &InteractiveState) {
    let hint = autocomplete_hint(state);
    let title = if hint.is_empty() {
        String::from("Command")
    } else {
        format!("Command  {hint}")
    };

    let line = if state.command_line.is_empty() {
        Line::from(vec![Span::styled(
            ": ",
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        )])
    } else {
        Line::from(vec![
            Span::styled(": ", Style::default().fg(BLUE).add_modifier(Modifier::BOLD)),
            Span::styled(state.command_line.as_str(), Style::default().fg(INK)),
        ])
    };

    let status = if let Some(error) = &state.last_error {
        Span::styled(error.as_str(), Style::default().fg(ROSE))
    } else {
        Span::styled(state.status_line.as_str(), Style::default().fg(GREEN))
    };

    let widget = Paragraph::new(vec![line, Line::from(status)])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(PANEL))
                .title(Span::styled(title, Style::default().fg(INK))),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

fn content_block(title: &str, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if active { BLUE } else { PANEL }))
        .title(Span::styled(title.to_owned(), Style::default().fg(INK)))
}

fn label_span(label: &str) -> Span<'static> {
    Span::styled(format!("{label:>12}: "), Style::default().fg(MUTED))
}

fn display_text(input: &str) -> String {
    if input.trim().is_empty() {
        String::from("<empty>")
    } else {
        input.to_owned()
    }
}

fn handle_key(state: &mut InteractiveState, key: KeyEvent) -> Result<bool, AppError> {
    match key.code {
        KeyCode::Tab => {
            state.active_tab = state.next_enabled_tab();
            sync_command_from_selection(state);
        }
        KeyCode::BackTab => {
            state.active_tab = state.prev_enabled_tab();
            sync_command_from_selection(state);
        }
        KeyCode::Up => {
            move_selection_up(state);
            sync_command_from_selection(state);
        }
        KeyCode::Down => {
            move_selection_down(state);
            sync_command_from_selection(state);
        }
        KeyCode::Left => {
            if state.active_tab == Tab::Body {
                state.body_mode = state.body_mode.prev();
                sync_command_from_selection(state);
            } else if matches!(state.active_tab, Tab::Response | Tab::Meta | Tab::Data) {
                state.response_scroll = state.response_scroll.saturating_sub(1);
            }
        }
        KeyCode::Right => {
            if state.active_tab == Tab::Body {
                state.body_mode = state.body_mode.next();
                sync_command_from_selection(state);
            } else if matches!(state.active_tab, Tab::Response | Tab::Meta | Tab::Data) {
                state.response_scroll = state.response_scroll.saturating_add(1);
            }
        }
        KeyCode::Home => {
            if matches!(state.active_tab, Tab::Response | Tab::Meta | Tab::Data) {
                state.response_scroll = 0;
            }
        }
        KeyCode::End => {
            if matches!(state.active_tab, Tab::Response | Tab::Meta | Tab::Data) {
                state.response_scroll = response_data_line_count(state) as u16;
            }
        }
        KeyCode::PageUp => {
            state.response_scroll = state.response_scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.response_scroll = state.response_scroll.saturating_add(10);
        }
        KeyCode::Delete => {
            delete_selected_item(state);
            sync_command_from_selection(state);
        }
        KeyCode::Esc => {
            state.command_line.clear();
            state.last_error = None;
        }
        KeyCode::Backspace => {
            state.command_line.pop();
        }
        KeyCode::Enter => {
            return execute_command_line(state);
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.command_line.push(character);
        }
        _ => {}
    }

    Ok(false)
}

fn execute_command_line(state: &mut InteractiveState) -> Result<bool, AppError> {
    let raw = state.command_line.trim().to_owned();
    if raw.is_empty() {
        if let Some(command) = selection_command(state) {
            state.command_line = command;
        }
        return Ok(false);
    }

    let should_quit = if raw.starts_with(':') {
        execute_internal_command(state, raw.trim_start_matches(':').trim())?
    } else {
        match raw.as_str() {
            "q" => true,
            "r" => {
                run_request(state);
                false
            }
            "w" => {
                save_response_data(state, None)?;
                false
            }
            other => {
                apply_input_command(state, other)?;
                false
            }
        }
    };

    if !should_quit {
        sync_command_from_selection(state);
    }
    Ok(should_quit)
}

fn execute_internal_command(state: &mut InteractiveState, command: &str) -> Result<bool, AppError> {
    let mut parts = command.split_whitespace();
    let head = parts.next().unwrap_or_default();
    match head {
        "" => {
            state.command_line.clear();
            Ok(false)
        }
        "q" => Ok(true),
        "run" => {
            run_request(state);
            Ok(false)
        }
        "w" => {
            let path = parts.collect::<Vec<_>>().join(" ");
            let path = if path.trim().is_empty() {
                None
            } else {
                Some(path)
            };
            save_response_data(state, path)?;
            Ok(false)
        }
        "tab" => {
            let value = parts.next().unwrap_or_default();
            if let Some(tab) = Tab::from_name(value) {
                if state.is_tab_enabled(tab) {
                    state.active_tab = tab;
                    sync_command_from_selection(state);
                } else {
                    state.last_error = Some(format!(
                        "tab {} is unavailable before sending a request",
                        tab.label()
                    ));
                }
            } else {
                state.last_error = Some(format!("unknown tab: {value}"));
            }
            Ok(false)
        }
        "body" => {
            let value = parts.next().unwrap_or_default();
            if let Some(mode) = BodyMode::from_name(value) {
                state.body_mode = mode;
                state.active_tab = Tab::Body;
                sync_command_from_selection(state);
            } else {
                state.last_error = Some(format!("unknown body mode: {value}"));
            }
            Ok(false)
        }
        "tabs" => {
            let value = parts.next().unwrap_or_default();
            if let Some(layout) = TabLayout::from_name(value) {
                state.tab_layout = layout;
                state.status_line = format!("tab layout set to {}", layout.label());
                state.last_error = None;
                sync_command_from_selection(state);
            } else {
                state.last_error = Some(format!("unknown tab layout: {value}"));
            }
            Ok(false)
        }
        _ => {
            state.last_error = Some(format!("unknown command: :{command}"));
            Ok(false)
        }
    }
}

fn apply_input_command(state: &mut InteractiveState, raw: &str) -> Result<(), AppError> {
    if let Some(value) = parse_prefixed_value(raw, &["-h", "-H", "--header"]) {
        apply_header_entry(state, value);
        return Ok(());
    }
    if let Some(value) = parse_prefixed_value(raw, &["-d", "--data"]) {
        apply_body_data_entry(state, value);
        return Ok(());
    }
    if let Some(value) = parse_prefixed_value(raw, &["--data-raw"]) {
        state.body_mode = BodyMode::Raw;
        state.body_raw = value;
        state.active_tab = Tab::Body;
        state.status_line = String::from("raw payload updated");
        state.last_error = None;
        return Ok(());
    }
    if let Some(value) = parse_prefixed_value(raw, &["--json"]) {
        state.body_mode = BodyMode::Json;
        state.body_json = value;
        state.active_tab = Tab::Body;
        state.status_line = String::from("json payload updated");
        state.last_error = None;
        return Ok(());
    }
    if let Some(value) = parse_prefixed_value(raw, &["-F", "--form"]) {
        apply_body_form_entry(state, value);
        return Ok(());
    }
    if let Some(value) = parse_prefixed_value(raw, &["-X", "--request"]) {
        state.method = value.to_ascii_uppercase();
        state.active_tab = Tab::Basic;
        state.status_line = format!("method set to {}", state.method);
        state.last_error = None;
        return Ok(());
    }
    if is_url_like(raw) {
        state.apply_url(raw)?;
        state.active_tab = Tab::Basic;
        state.status_line = String::from("request URL updated");
        state.last_error = None;
        return Ok(());
    }
    if state.active_tab == Tab::Params && raw.contains('=') && !raw.starts_with('-') {
        apply_param_entry(state, raw.to_owned());
        return Ok(());
    }

    apply_cli_matches(state, raw)
}

fn apply_cli_matches(state: &mut InteractiveState, raw: &str) -> Result<(), AppError> {
    let tokens = shell_words::split(raw)
        .map_err(|error| AppError::new(2, format!("invalid interactive command: {error}")))?;
    let tokens = normalize_tokens(tokens);
    let mut command = Cli::command();
    let matches = command
        .try_get_matches_from_mut(std::iter::once(String::from("mirza")).chain(tokens.clone()))
        .map_err(|error| AppError::new(2, error.to_string()))?;
    let parsed =
        Cli::from_arg_matches(&matches).map_err(|error| AppError::new(2, error.to_string()))?;

    if matches.contains_id("url")
        && let Some(url) = parsed.url.as_deref()
    {
        state.apply_url(url)?;
        state.active_tab = Tab::Basic;
    }
    if matches.contains_id("request") {
        state.method = parsed.request.unwrap_or_else(|| String::from("GET"));
    }
    if matches.contains_id("headers") {
        for header in parsed.headers {
            apply_header_entry(state, header);
        }
    }
    if matches.contains_id("form") {
        for entry in parsed.form {
            apply_body_form_entry(state, entry);
        }
    }
    if matches.contains_id("json") {
        state.body_mode = BodyMode::Json;
        state.body_json = parsed.json.unwrap_or_default();
        state.active_tab = Tab::Body;
    }
    if matches.contains_id("data_raw") {
        state.body_mode = BodyMode::Raw;
        state.body_raw = parsed.data_raw.join("&");
        state.active_tab = Tab::Body;
    }
    if matches.contains_id("data") {
        if parsed.get {
            for entry in parsed.data {
                apply_param_entry(state, entry);
            }
            state.active_tab = Tab::Params;
        } else {
            for entry in parsed.data {
                apply_body_data_entry(state, entry);
            }
        }
    }
    if matches.contains_id("user_agent") {
        state.user_agent = parsed.user_agent.unwrap_or_default();
    }
    if matches.contains_id("referer") {
        state.referer = parsed.referer.unwrap_or_default();
    }
    if matches.contains_id("proxy") {
        state.proxy = parsed.proxy.unwrap_or_default();
    }
    if matches.contains_id("retry") {
        state.retry = parsed.retry.to_string();
    }
    if matches.contains_id("continue_at") {
        state.continue_at = parsed.continue_at.unwrap_or_default();
    }
    if matches.contains_id("range") {
        state.range = parsed.range.unwrap_or_default();
    }
    if matches.contains_id("limit_rate") {
        state.limit_rate = parsed.limit_rate.unwrap_or_default();
    }
    if matches.contains_id("output_style") {
        state.output_style = format!("{:?}", parsed.output_style).to_ascii_lowercase();
    }
    if matches.contains_id("color") {
        state.color = format!("{:?}", parsed.color).to_ascii_lowercase();
    }
    if matches.contains_id("include") {
        state.include = parsed.include;
    }
    if matches.contains_id("location") {
        state.location = parsed.location;
    }
    if matches.contains_id("insecure") {
        state.insecure = parsed.insecure;
    }
    if matches.contains_id("fail") {
        state.fail = parsed.fail;
    }
    if matches.contains_id("compressed") {
        state.compressed = parsed.compressed;
    }

    state.status_line = format!("applied {}", raw.trim());
    state.last_error = None;
    Ok(())
}

fn parse_prefixed_value(raw: &str, prefixes: &[&str]) -> Option<String> {
    prefixes.iter().find_map(|prefix| {
        raw.strip_prefix(prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(unquote)
    })
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn is_url_like(raw: &str) -> bool {
    let trimmed = raw.trim();
    !trimmed.is_empty()
        && !trimmed.starts_with('-')
        && (trimmed.contains("://") || trimmed.contains('.'))
}

fn normalize_tokens(tokens: Vec<String>) -> Vec<String> {
    tokens
        .into_iter()
        .map(|token| {
            if token == "-h" {
                String::from("-H")
            } else {
                token
            }
        })
        .collect()
}

fn apply_header_entry(state: &mut InteractiveState, value: String) {
    if state.headers.is_empty() {
        state.headers.push(value);
        state.header_index = 0;
    } else if state.active_tab == Tab::Headers {
        let index = state.header_index.min(state.headers.len() - 1);
        state.headers[index] = value;
    } else {
        state.headers.push(value);
        state.header_index = state.headers.len() - 1;
    }
    state.active_tab = Tab::Headers;
    state.status_line = String::from("header updated");
    state.last_error = None;
}

fn apply_body_data_entry(state: &mut InteractiveState, value: String) {
    state.body_mode = BodyMode::Data;
    if state.body_data.is_empty() {
        state.body_data.push(value);
        state.body_data_index = 0;
    } else if state.active_tab == Tab::Body {
        let index = state.body_data_index.min(state.body_data.len() - 1);
        state.body_data[index] = value;
    } else {
        state.body_data.push(value);
        state.body_data_index = state.body_data.len() - 1;
    }
    state.active_tab = Tab::Body;
    state.status_line = String::from("request data updated");
    state.last_error = None;
}

fn apply_body_form_entry(state: &mut InteractiveState, value: String) {
    state.body_mode = BodyMode::Form;
    if state.body_form.is_empty() {
        state.body_form.push(value);
        state.body_form_index = 0;
    } else if state.active_tab == Tab::Body {
        let index = state.body_form_index.min(state.body_form.len() - 1);
        state.body_form[index] = value;
    } else {
        state.body_form.push(value);
        state.body_form_index = state.body_form.len() - 1;
    }
    state.active_tab = Tab::Body;
    state.status_line = String::from("multipart field updated");
    state.last_error = None;
}

fn apply_param_entry(state: &mut InteractiveState, value: String) {
    if state.params.is_empty() {
        state.params.push(value);
        state.param_index = 0;
    } else if state.active_tab == Tab::Params {
        let index = state.param_index.min(state.params.len() - 1);
        state.params[index] = value;
    } else {
        state.params.push(value);
        state.param_index = state.params.len() - 1;
    }
    state.active_tab = Tab::Params;
    state.status_line = String::from("query parameter updated");
    state.last_error = None;
}

fn run_request(state: &mut InteractiveState) {
    match app::execute_capture(&state.build_cli()) {
        Ok(response) => {
            state.status_line = format!("{} {} {}", response.status, response.reason, response.url);
            state.last_error = None;
            state.response = Some(response);
            state.response_scroll = 0;
            state.active_tab = Tab::Response;
        }
        Err(error) => {
            state.last_error = Some(error.message().to_owned());
        }
    }
}

fn save_response_data(state: &mut InteractiveState, path: Option<String>) -> Result<(), AppError> {
    let Some(response) = &state.response else {
        state.last_error = Some(String::from("no response available to save"));
        return Ok(());
    };

    if let Some(path) = path.filter(|value| !value.trim().is_empty()) {
        state.save_path = path;
    }

    let bytes = if let Ok(json) = serde_json::from_slice::<JsonValue>(&response.body) {
        serde_json::to_vec_pretty(&json).map_err(|error| {
            AppError::new(1, format!("failed to serialize JSON response: {error}"))
        })?
    } else {
        response.body.clone()
    };

    std::fs::write(&state.save_path, bytes)
        .map_err(|error| AppError::new(23, format!("failed to save response: {error}")))?;
    state.status_line = format!("saved response to {}", state.save_path);
    state.last_error = None;
    Ok(())
}

fn response_data_text(response: &CapturedResponse) -> String {
    if let Ok(json) = serde_json::from_slice::<JsonValue>(&response.body)
        && let Ok(rendered) = serde_json::to_string_pretty(&json)
    {
        return rendered;
    }

    String::from_utf8_lossy(&response.body).into_owned()
}

fn response_data_line_count(state: &InteractiveState) -> usize {
    match state.active_tab {
        Tab::Response => state
            .response
            .as_ref()
            .map(|response| String::from_utf8_lossy(&response.rendered).lines().count())
            .unwrap_or(0),
        Tab::Meta => state
            .response
            .as_ref()
            .map(|response| format_response_meta(response).lines().count())
            .unwrap_or(0),
        Tab::Data => state
            .response
            .as_ref()
            .map(|response| response_data_text(response).lines().count())
            .unwrap_or(0),
        _ => 0,
    }
}

fn format_response_meta(response: &CapturedResponse) -> String {
    let mut lines = vec![
        format!("Method       : {}", response.method),
        format!("URL          : {}", response.url),
        format!("Status       : {} {}", response.status, response.reason),
        format!("Version      : {}", response.version),
        format!("Duration     : {} ms", response.duration.as_millis()),
        format!("Body Bytes   : {}", response.body_bytes),
        format!(
            "Content-Type : {}",
            response.content_type.as_deref().unwrap_or("<unknown>")
        ),
    ];
    if let Some(summary) = &response.certificate_summary {
        lines.push(format!("TLS          : {summary}"));
    }
    lines.join("\n")
}

fn move_selection_up(state: &mut InteractiveState) {
    match state.active_tab {
        Tab::Basic => state.basic_index = state.basic_index.saturating_sub(1),
        Tab::Headers => state.header_index = state.header_index.saturating_sub(1),
        Tab::Body => match state.body_mode {
            BodyMode::Data => state.body_data_index = state.body_data_index.saturating_sub(1),
            BodyMode::Form => state.body_form_index = state.body_form_index.saturating_sub(1),
            BodyMode::Json | BodyMode::Raw => {}
        },
        Tab::Params => state.param_index = state.param_index.saturating_sub(1),
        Tab::Response | Tab::Meta | Tab::Data => {
            state.response_scroll = state.response_scroll.saturating_sub(1)
        }
        Tab::Settings => state.settings_index = state.settings_index.saturating_sub(1),
        Tab::Request => {}
    }
}

fn move_selection_down(state: &mut InteractiveState) {
    match state.active_tab {
        Tab::Basic => {
            state.basic_index = (state.basic_index + 1).min(BASIC_FIELDS.len().saturating_sub(1));
        }
        Tab::Headers => {
            if !state.headers.is_empty() {
                state.header_index = (state.header_index + 1).min(state.headers.len() - 1);
            }
        }
        Tab::Body => match state.body_mode {
            BodyMode::Data => {
                if !state.body_data.is_empty() {
                    state.body_data_index =
                        (state.body_data_index + 1).min(state.body_data.len() - 1);
                }
            }
            BodyMode::Form => {
                if !state.body_form.is_empty() {
                    state.body_form_index =
                        (state.body_form_index + 1).min(state.body_form.len() - 1);
                }
            }
            BodyMode::Json | BodyMode::Raw => {}
        },
        Tab::Params => {
            if !state.params.is_empty() {
                state.param_index = (state.param_index + 1).min(state.params.len() - 1);
            }
        }
        Tab::Response | Tab::Meta | Tab::Data => {
            state.response_scroll = state.response_scroll.saturating_add(1)
        }
        Tab::Settings => {
            state.settings_index =
                (state.settings_index + 1).min(SETTING_FIELDS.len().saturating_sub(1));
        }
        Tab::Request => {}
    }
}

fn delete_selected_item(state: &mut InteractiveState) {
    match state.active_tab {
        Tab::Headers => remove_selected(&mut state.headers, &mut state.header_index),
        Tab::Params => remove_selected(&mut state.params, &mut state.param_index),
        Tab::Body => match state.body_mode {
            BodyMode::Data => remove_selected(&mut state.body_data, &mut state.body_data_index),
            BodyMode::Form => remove_selected(&mut state.body_form, &mut state.body_form_index),
            BodyMode::Json => state.body_json.clear(),
            BodyMode::Raw => state.body_raw.clear(),
        },
        _ => {}
    }
}

fn remove_selected(items: &mut Vec<String>, index: &mut usize) {
    if items.is_empty() {
        return;
    }
    items.remove(*index);
    if items.is_empty() {
        *index = 0;
    } else {
        *index = (*index).min(items.len() - 1);
    }
}

fn sync_command_from_selection(state: &mut InteractiveState) {
    if let Some(command) = selection_command(state) {
        state.command_line = command;
    }
}

fn selection_command(state: &InteractiveState) -> Option<String> {
    match state.active_tab {
        Tab::Basic => Some(match BASIC_FIELDS[state.basic_index] {
            BasicField::Url => state.url.clone(),
            BasicField::Method => format!("-X {}", state.method),
            BasicField::Include => String::from("--include"),
            BasicField::Location => String::from("--location"),
            BasicField::Insecure => String::from("--insecure"),
            BasicField::Fail => String::from("--fail"),
            BasicField::Compressed => String::from("--compressed"),
        }),
        Tab::Headers => state
            .headers
            .get(state.header_index)
            .map(|value| format!("-H \"{}\"", value.replace('"', "\\\""))),
        Tab::Body => match state.body_mode {
            BodyMode::Data => state
                .body_data
                .get(state.body_data_index)
                .map(|value| format!("-d \"{}\"", value.replace('"', "\\\""))),
            BodyMode::Form => state
                .body_form
                .get(state.body_form_index)
                .map(|value| format!("-F \"{}\"", value.replace('"', "\\\""))),
            BodyMode::Json => Some(format!("--json '{}'", state.body_json.replace('\'', "\\'"))),
            BodyMode::Raw => Some(format!(
                "--data-raw '{}'",
                state.body_raw.replace('\'', "\\'")
            )),
        },
        Tab::Params => state.params.get(state.param_index).cloned(),
        Tab::Settings => Some(match SETTING_FIELDS[state.settings_index] {
            SettingField::TabLayout => format!(":tabs {}", state.tab_layout.label()),
            SettingField::OutputStyle => format!("--output-style {}", state.output_style),
            SettingField::Color => format!("--color {}", state.color),
            SettingField::Retry => format!("--retry {}", state.retry),
            SettingField::Range => format!("--range {}", state.range),
            SettingField::ContinueAt => format!("--continue-at {}", state.continue_at),
            SettingField::LimitRate => format!("--limit-rate {}", state.limit_rate),
            SettingField::UserAgent => format!("--user-agent \"{}\"", state.user_agent),
            SettingField::Referer => format!("--referer \"{}\"", state.referer),
            SettingField::Proxy => format!("--proxy \"{}\"", state.proxy),
        }),
        _ => None,
    }
}

fn autocomplete_hint(state: &InteractiveState) -> String {
    let suggestions = suggestions_for_command(state);
    if suggestions.is_empty() {
        String::new()
    } else {
        format!("suggest: {}", suggestions.join(" | "))
    }
}

fn suggestions_for_command(state: &InteractiveState) -> Vec<String> {
    let input = state.command_line.trim_start();
    if input.is_empty() {
        return COMMAND_SUGGESTIONS
            .iter()
            .take(5)
            .map(|value| (*value).to_owned())
            .collect();
    }

    if input.starts_with(':') {
        if input.trim_start_matches(':').starts_with("tabs") {
            return filter_suggestions(
                last_argument_value(input.trim_start_matches(':')),
                &TAB_LAYOUT_SUGGESTIONS,
            );
        }
        return filter_suggestions(input.trim_start_matches(':'), &COMMAND_SUGGESTIONS);
    }
    if input.starts_with("-h") || input.starts_with("-H") || input.starts_with("--header") {
        return filter_suggestions(last_argument_value(input), &HEADER_SUGGESTIONS);
    }
    if input.starts_with("-d") || input.starts_with("--data") {
        return filter_suggestions(last_argument_value(input), &DATA_SUGGESTIONS);
    }
    if input.starts_with("-F") || input.starts_with("--form") {
        return filter_suggestions(last_argument_value(input), &MULTIPART_SUGGESTIONS);
    }
    if input.starts_with("--json") {
        return filter_suggestions(last_argument_value(input), &JSON_SUGGESTIONS);
    }
    if input.starts_with("-X") || input.starts_with("--request") {
        return filter_suggestions(last_argument_value(input), &METHOD_SUGGESTIONS);
    }
    if input.starts_with("--output-style") {
        return filter_suggestions(last_argument_value(input), &OUTPUT_STYLE_SUGGESTIONS);
    }
    if input.starts_with("--color") {
        return filter_suggestions(last_argument_value(input), &COLOR_SUGGESTIONS);
    }
    if state.active_tab == Tab::Params && !input.starts_with('-') {
        return filter_suggestions(input, &DATA_SUGGESTIONS);
    }

    Vec::new()
}

fn last_argument_value(input: &str) -> &str {
    input
        .split_once(' ')
        .map(|(_, value)| value.trim())
        .unwrap_or_default()
}

fn filter_suggestions(current: &str, suggestions: &[&str]) -> Vec<String> {
    let current = current.trim_matches(|character| character == '\'' || character == '"');
    let needle = current.to_ascii_lowercase();
    suggestions
        .iter()
        .filter(|candidate| {
            needle.is_empty() || candidate.to_ascii_lowercase().starts_with(&needle)
        })
        .take(5)
        .map(|candidate| (*candidate).to_owned())
        .collect()
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Tab {
    Request,
    Basic,
    Headers,
    Body,
    Params,
    Response,
    Meta,
    Data,
    Settings,
}

impl Tab {
    const ALL: [Self; 9] = [
        Self::Request,
        Self::Basic,
        Self::Headers,
        Self::Body,
        Self::Params,
        Self::Response,
        Self::Meta,
        Self::Data,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Request => "Request",
            Self::Basic => "Basic",
            Self::Headers => "Headers",
            Self::Body => "Body",
            Self::Params => "Params",
            Self::Response => "Response",
            Self::Meta => "Meta",
            Self::Data => "Data",
            Self::Settings => "Settings",
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|tab| tab.label().eq_ignore_ascii_case(value))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BodyMode {
    Data,
    Json,
    Raw,
    Form,
}

impl BodyMode {
    const ALL: [Self; 4] = [Self::Data, Self::Json, Self::Raw, Self::Form];

    fn label(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Json => "json",
            Self::Raw => "raw",
            Self::Form => "form",
        }
    }

    fn next(self) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    fn from_name(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.label().eq_ignore_ascii_case(value))
    }
}

#[derive(Copy, Clone)]
enum BasicField {
    Url,
    Method,
    Include,
    Location,
    Insecure,
    Fail,
    Compressed,
}

impl BasicField {
    fn label(self) -> &'static str {
        match self {
            Self::Url => "URL",
            Self::Method => "Method",
            Self::Include => "Include",
            Self::Location => "Location",
            Self::Insecure => "Insecure",
            Self::Fail => "Fail",
            Self::Compressed => "Compressed",
        }
    }
}

#[derive(Copy, Clone)]
enum SettingField {
    TabLayout,
    OutputStyle,
    Color,
    Retry,
    Range,
    ContinueAt,
    LimitRate,
    UserAgent,
    Referer,
    Proxy,
}

impl SettingField {
    fn label(self) -> &'static str {
        match self {
            Self::TabLayout => "TabLayout",
            Self::OutputStyle => "OutputStyle",
            Self::Color => "Color",
            Self::Retry => "Retry",
            Self::Range => "Range",
            Self::ContinueAt => "ContinueAt",
            Self::LimitRate => "LimitRate",
            Self::UserAgent => "User-Agent",
            Self::Referer => "Referer",
            Self::Proxy => "Proxy",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TabLayout {
    Horizontal,
    Vertical,
}

impl TabLayout {
    fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "horizontal" => Some(Self::Horizontal),
            "vertical" | "column" | "vertical-column" => Some(Self::Vertical),
            _ => None,
        }
    }
}

struct InteractiveState {
    active_tab: Tab,
    tab_layout: TabLayout,
    body_mode: BodyMode,
    basic_index: usize,
    header_index: usize,
    body_data_index: usize,
    body_form_index: usize,
    param_index: usize,
    settings_index: usize,
    response_scroll: u16,
    command_line: String,
    status_line: String,
    last_error: Option<String>,
    save_path: String,
    url: String,
    method: String,
    output_style: String,
    color: String,
    retry: String,
    range: String,
    continue_at: String,
    limit_rate: String,
    user_agent: String,
    referer: String,
    proxy: String,
    include: bool,
    location: bool,
    insecure: bool,
    fail: bool,
    compressed: bool,
    headers: Vec<String>,
    body_data: Vec<String>,
    body_form: Vec<String>,
    body_json: String,
    body_raw: String,
    params: Vec<String>,
    response: Option<CapturedResponse>,
}

impl InteractiveState {
    fn from_cli(cli: Cli) -> Self {
        let save_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("response.json")
            .display()
            .to_string();

        let body_mode = if !cli.form.is_empty() {
            BodyMode::Form
        } else if cli.json.is_some() {
            BodyMode::Json
        } else if !cli.data_raw.is_empty() {
            BodyMode::Raw
        } else {
            BodyMode::Data
        };

        let mut state = Self {
            active_tab: Tab::Request,
            tab_layout: TabLayout::Vertical,
            body_mode,
            basic_index: 0,
            header_index: 0,
            body_data_index: 0,
            body_form_index: 0,
            param_index: 0,
            settings_index: 0,
            response_scroll: 0,
            command_line: String::new(),
            status_line: String::from(
                "Interactive mode ready. Use Tab for tabs and type curl options below.",
            ),
            last_error: None,
            save_path,
            url: String::new(),
            method: cli.request.unwrap_or_else(|| String::from("GET")),
            output_style: format!("{:?}", cli.output_style).to_ascii_lowercase(),
            color: format!("{:?}", cli.color).to_ascii_lowercase(),
            retry: cli.retry.to_string(),
            range: cli.range.unwrap_or_default(),
            continue_at: cli.continue_at.unwrap_or_default(),
            limit_rate: cli.limit_rate.unwrap_or_default(),
            user_agent: cli.user_agent.unwrap_or_default(),
            referer: cli.referer.unwrap_or_default(),
            proxy: cli.proxy.unwrap_or_default(),
            include: cli.include,
            location: cli.location,
            insecure: cli.insecure,
            fail: cli.fail,
            compressed: cli.compressed,
            headers: cli.headers,
            body_data: cli.data,
            body_form: cli.form,
            body_json: cli.json.unwrap_or_default(),
            body_raw: cli.data_raw.join("&"),
            params: Vec::new(),
            response: None,
        };

        if let Some(url) = cli.url.as_deref() {
            let _ = state.apply_url(url);
        }

        state
    }

    fn build_cli(&self) -> Cli {
        let url = self.build_url_with_params();
        let (data, data_raw, form, json) = match self.body_mode {
            BodyMode::Data => (cleaned_items(&self.body_data), Vec::new(), Vec::new(), None),
            BodyMode::Json => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                optional_text(&self.body_json),
            ),
            BodyMode::Raw => (
                Vec::new(),
                optional_text(&self.body_raw)
                    .map(|value| vec![value])
                    .unwrap_or_default(),
                Vec::new(),
                None,
            ),
            BodyMode::Form => (Vec::new(), Vec::new(), cleaned_items(&self.body_form), None),
        };

        Cli {
            url,
            interactive: false,
            request: optional_text(&self.method).map(|value| value.to_ascii_uppercase()),
            head: false,
            include: self.include,
            location: self.location,
            insecure: self.insecure,
            verbose: false,
            silent: false,
            show_error: true,
            fail: self.fail,
            compressed: self.compressed,
            get: false,
            headers: cleaned_items(&self.headers),
            data,
            data_raw,
            data_binary: Vec::new(),
            form,
            json,
            upload_file: None,
            user: None,
            user_agent: optional_text(&self.user_agent),
            referer: optional_text(&self.referer),
            proxy: optional_text(&self.proxy),
            connect_timeout: None,
            max_time: None,
            retry: self.retry.trim().parse().unwrap_or(0),
            continue_at: optional_text(&self.continue_at),
            range: optional_text(&self.range),
            limit_rate: optional_text(&self.limit_rate),
            output_style: parse_output_style(&self.output_style),
            show: vec![OutputSection::All],
            color: parse_color_mode(&self.color),
            output: None,
            dump_header: None,
            http1_1: false,
            http2: false,
        }
    }

    fn build_url_with_params(&self) -> Option<String> {
        let url = optional_text(&self.url)?;
        if self.params.is_empty() {
            return Some(url);
        }

        match Url::parse(&url) {
            Ok(mut parsed) => {
                let joined = cleaned_items(&self.params).join("&");
                if joined.is_empty() {
                    Some(url)
                } else {
                    parsed.set_query(Some(&joined));
                    Some(parsed.into())
                }
            }
            Err(_) => Some(url),
        }
    }

    fn apply_url(&mut self, raw: &str) -> Result<(), AppError> {
        let parsed = Url::parse(raw)
            .or_else(|_| Url::parse(&format!("http://{raw}")))
            .map_err(|error| AppError::new(3, format!("invalid URL '{raw}': {error}")))?;
        self.params = parsed
            .query_pairs()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        let mut base = parsed;
        base.set_query(None);
        self.url = base.to_string();
        Ok(())
    }

    fn visible_count(&self, items: &[String]) -> usize {
        items.iter().filter(|item| !item.trim().is_empty()).count()
    }

    fn is_tab_enabled(&self, tab: Tab) -> bool {
        !matches!(tab, Tab::Response | Tab::Meta | Tab::Data) || self.response.is_some()
    }

    fn next_enabled_tab(&self) -> Tab {
        let current_index = Tab::ALL
            .iter()
            .position(|item| *item == self.active_tab)
            .unwrap_or(0);
        for offset in 1..=Tab::ALL.len() {
            let candidate = Tab::ALL[(current_index + offset) % Tab::ALL.len()];
            if self.is_tab_enabled(candidate) {
                return candidate;
            }
        }
        self.active_tab
    }

    fn prev_enabled_tab(&self) -> Tab {
        let current_index = Tab::ALL
            .iter()
            .position(|item| *item == self.active_tab)
            .unwrap_or(0);
        for offset in 1..=Tab::ALL.len() {
            let candidate = Tab::ALL[(current_index + Tab::ALL.len() - offset) % Tab::ALL.len()];
            if self.is_tab_enabled(candidate) {
                return candidate;
            }
        }
        self.active_tab
    }

    fn response_badge(&self) -> String {
        self.response
            .as_ref()
            .map(|response| format!("{} {}", response.status, response.reason))
            .unwrap_or_else(|| String::from("no response"))
    }

    fn response_color(&self) -> Color {
        match self.response.as_ref().map(|response| response.status) {
            Some(status) if (200..300).contains(&status) => GREEN,
            Some(status) if (300..400).contains(&status) => BLUE,
            Some(status) if (400..500).contains(&status) => GOLD,
            Some(_) => ROSE,
            None => MUTED,
        }
    }

    fn basic_value(&self, field: BasicField) -> String {
        match field {
            BasicField::Url => display_text(&self.url),
            BasicField::Method => display_text(&self.method),
            BasicField::Include => self.include.to_string(),
            BasicField::Location => self.location.to_string(),
            BasicField::Insecure => self.insecure.to_string(),
            BasicField::Fail => self.fail.to_string(),
            BasicField::Compressed => self.compressed.to_string(),
        }
    }

    fn setting_value(&self, field: SettingField) -> String {
        match field {
            SettingField::TabLayout => self.tab_layout.label().to_owned(),
            SettingField::OutputStyle => display_text(&self.output_style),
            SettingField::Color => display_text(&self.color),
            SettingField::Retry => display_text(&self.retry),
            SettingField::Range => display_text(&self.range),
            SettingField::ContinueAt => display_text(&self.continue_at),
            SettingField::LimitRate => display_text(&self.limit_rate),
            SettingField::UserAgent => display_text(&self.user_agent),
            SettingField::Referer => display_text(&self.referer),
            SettingField::Proxy => display_text(&self.proxy),
        }
    }
}

fn cleaned_items(items: &[String]) -> Vec<String> {
    items
        .iter()
        .filter(|item| !item.trim().is_empty())
        .cloned()
        .collect()
}

fn optional_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn parse_output_style(value: &str) -> OutputStyle {
    match value.trim().to_ascii_lowercase().as_str() {
        "raw" => OutputStyle::Raw,
        "pretty" => OutputStyle::Pretty,
        "json" => OutputStyle::Json,
        "compact" => OutputStyle::Compact,
        _ => OutputStyle::Auto,
    }
}

fn parse_color_mode(value: &str) -> ColorMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "always" => ColorMode::Always,
        "never" => ColorMode::Never,
        _ => ColorMode::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn apply_header_shortcut_replaces_selected_header() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        state.active_tab = Tab::Headers;
        state.headers = vec![String::from("Accept: */*")];
        state.header_index = 0;
        apply_input_command(&mut state, "-h Content-Type: application/json").unwrap();
        assert_eq!(
            state.headers,
            vec![String::from("Content-Type: application/json")]
        );
    }

    #[test]
    fn apply_url_extracts_query_params() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        state
            .apply_url("https://example.com/items?page=2&limit=10")
            .unwrap();
        assert_eq!(state.url, "https://example.com/items");
        assert_eq!(
            state.params,
            vec![String::from("page=2"), String::from("limit=10")]
        );
    }

    #[test]
    fn build_cli_merges_query_params_into_url() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        state.url = String::from("https://example.com/items");
        state.params = vec![String::from("page=2"), String::from("limit=10")];
        let cli = state.build_cli();
        assert_eq!(
            cli.url.as_deref(),
            Some("https://example.com/items?page=2&limit=10")
        );
    }

    #[test]
    fn selection_command_for_header_uses_curl_flag() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        state.active_tab = Tab::Headers;
        state.headers = vec![String::from("Accept: application/json")];
        assert_eq!(
            selection_command(&state).as_deref(),
            Some("-H \"Accept: application/json\"")
        );
    }

    #[test]
    fn response_tabs_are_disabled_before_first_request() {
        let state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        assert!(!state.is_tab_enabled(Tab::Response));
        assert!(!state.is_tab_enabled(Tab::Meta));
        assert!(!state.is_tab_enabled(Tab::Data));
    }

    #[test]
    fn tab_navigation_skips_disabled_response_tabs() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        state.active_tab = Tab::Params;
        assert_eq!(state.next_enabled_tab(), Tab::Settings);
    }

    #[test]
    fn settings_selection_command_exposes_tab_layout_command() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        state.active_tab = Tab::Settings;
        state.settings_index = 0;
        assert_eq!(selection_command(&state).as_deref(), Some(":tabs vertical"));
    }

    #[test]
    fn internal_tabs_command_switches_to_vertical_layout() {
        let mut state = InteractiveState::from_cli(Cli::parse_from(["mirza", "--interactive"]));
        execute_internal_command(&mut state, "tabs vertical").unwrap();
        assert_eq!(state.tab_layout, TabLayout::Vertical);
    }
}
