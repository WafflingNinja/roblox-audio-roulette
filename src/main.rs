mod api;
mod audio;
mod cookie;
mod store;

use api::{Api, Filter, Sound};
use audio::Player;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::{execute, terminal};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use std::io::{self, Stdout};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;
use store::SavedSound;

/// Results coming back from background worker threads.
enum Msg {
    Batch(Result<Vec<Sound>, String>),
    Audio(u64, String, Result<Vec<u8>, String>),
}

const MAX_COUNT: usize = 1000; // store only returns 1000 distinct ids

enum Screen {
    FirstRun,
    Browse,
    Saved,
}

struct App {
    screen: Screen,
    api: Api,
    player: Player,
    cookie_profile: Option<String>,
    filter: Filter,
    cyrillic: bool,
    count_input: String,
    browse: Vec<Sound>,
    browse_sel: usize,
    saved: Vec<SavedSound>,
    saved_sel: usize,
    status: String,
    busy: bool,
    spin: usize,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    quit: bool,
}

fn first_run_marker() -> std::path::PathBuf {
    store::config_dir().join(".welcomed")
}

impl App {
    fn new() -> Self {
        let found = cookie::find();
        let (cookie_val, profile) = match found {
            Some(f) => (Some(f.value), Some(f.profile)),
            None => (None, None),
        };
        let screen = if first_run_marker().exists() {
            Screen::Browse
        } else {
            Screen::FirstRun
        };
        let (tx, rx) = mpsc::channel();
        App {
            screen,
            api: Api::new(cookie_val),
            player: Player::new(),
            cookie_profile: profile,
            filter: Filter::Both,
            cyrillic: false,
            count_input: "100".into(),
            browse: Vec::new(),
            browse_sel: 0,
            saved: store::load(),
            saved_sel: 0,
            status: String::new(),
            busy: false,
            spin: 0,
            tx,
            rx,
            quit: false,
        }
    }

    fn count(&self) -> usize {
        self.count_input.parse::<usize>().unwrap_or(0).clamp(1, MAX_COUNT)
    }

    fn browse_rows(&self) -> Vec<RowData> {
        self.browse
            .iter()
            .map(|s| (s.name.clone(), s.duration, s.kind.clone(), s.id, false))
            .collect()
    }
    fn saved_rows(&self) -> Vec<RowData> {
        self.saved
            .iter()
            .map(|s| (s.name.clone(), s.duration, s.kind.clone(), s.id, s.deleted))
            .collect()
    }

    fn selected_browse(&self) -> Option<&Sound> {
        self.browse.get(self.browse_sel)
    }
    fn selected_saved(&self) -> Option<&SavedSound> {
        self.saved.get(self.saved_sel)
    }

    fn roll(&mut self) {
        if self.busy {
            return;
        }
        self.busy = true;
        let cyr = if self.cyrillic { " (Cyrillic)" } else { "" };
        self.status = format!("Rolling {} {}{cyr}...", self.count(), self.filter.label());
        let (api, filter, count, cyrillic, tx) =
            (self.api.clone(), self.filter, self.count(), self.cyrillic, self.tx.clone());
        std::thread::spawn(move || {
            let r = api.random_batch(filter, count, cyrillic);
            let _ = tx.send(Msg::Batch(r));
        });
    }

    fn play_id(&mut self, id: u64, label: &str) {
        if !self.api.has_cookie() {
            self.status = "Not logged in — open Roblox in Firefox first.".into();
            return;
        }
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = format!("Loading {label}...");
        let (api, tx, name) = (self.api.clone(), self.tx.clone(), label.to_string());
        std::thread::spawn(move || {
            let r = api.download(id);
            let _ = tx.send(Msg::Audio(id, name, r));
        });
    }

    /// Handle one finished background job.
    fn on_msg(&mut self, msg: Msg) {
        self.busy = false;
        match msg {
            Msg::Batch(Ok(mut list)) => {
                list.sort_by_key(|s| s.name.to_lowercase());
                self.browse = list;
                self.browse_sel = 0;
                let note = if self.browse.is_empty() {
                    if self.cyrillic {
                        "No Cyrillic sounds found — try again or turn off [y].".into()
                    } else {
                        "Nothing found — try again.".into()
                    }
                } else {
                    format!("{} sounds loaded.", self.browse.len())
                };
                self.status = note;
            }
            Msg::Batch(Err(e)) => self.status = format!("Roll error: {e}"),
            Msg::Audio(_id, name, Ok(bytes)) => match self.player.play(&bytes) {
                Ok(()) => self.status = format!("♪ Playing: {name}   (p=stop)"),
                Err(e) => self.status = format!("Play error: {e}"),
            },
            Msg::Audio(_id, name, Err(e)) => {
                self.status = format!("Can't play {name}: {e}");
            }
        }
    }

    fn copy(&mut self, id: u64) {
        match Command::new("wl-copy").arg(id.to_string()).status() {
            Ok(s) if s.success() => self.status = format!("Copied {id} to clipboard"),
            _ => self.status = "wl-copy failed".into(),
        }
    }

    fn save_current(&mut self) {
        let Some(s) = self.selected_browse() else { return };
        let entry = SavedSound {
            id: s.id,
            name: s.name.clone(),
            creator: s.creator.clone(),
            duration: s.duration,
            kind: s.kind.clone(),
            deleted: false,
        };
        if self.saved.iter().any(|x| x.id == entry.id) {
            self.status = format!("{} already saved", entry.id);
            return;
        }
        let id = entry.id;
        self.saved.push(entry);
        store::save(&self.saved);
        self.status = format!("Saved {} ({} total)", id, self.saved.len());
    }

    fn remove_saved(&mut self) {
        if self.saved_sel >= self.saved.len() {
            return;
        }
        let gone = self.saved.remove(self.saved_sel);
        if self.saved_sel >= self.saved.len() && self.saved_sel > 0 {
            self.saved_sel -= 1;
        }
        store::save(&self.saved);
        self.status = format!("Removed {}", gone.id);
    }

    /// Re-check every saved id against the store; flag the dead ones.
    fn refresh_saved(&mut self) {
        if self.saved.is_empty() {
            return;
        }
        self.status = "Checking saved sounds...".into();
        let ids: Vec<u64> = self.saved.iter().map(|s| s.id).collect();
        match self.api.existing_ids(&ids) {
            Ok(alive) => {
                for s in &mut self.saved {
                    s.deleted = !alive.contains(&s.id);
                }
                store::save(&self.saved);
                let dead = self.saved.iter().filter(|s| s.deleted).count();
                self.status = format!("{} saved, {} deleted", self.saved.len(), dead);
            }
            Err(e) => self.status = format!("Check failed: {e}"),
        }
    }
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--selftest") {
        return selftest();
    }
    let mut term = setup_terminal()?;
    let mut app = App::new();
    // Flag dead saved sounds on launch, as asked.
    if !matches!(app.screen, Screen::FirstRun) {
        app.refresh_saved();
    }

    while !app.quit {
        term.draw(|f| ui(f, &app))?;

        // Poll so the UI stays live while background jobs run.
        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    handle_key(&mut app, k.code);
                }
            }
        }

        // Drain any finished background jobs.
        while let Ok(msg) = app.rx.try_recv() {
            app.on_msg(msg);
        }

        if app.busy {
            app.spin = (app.spin + 1) % SPINNER.len();
        }
    }

    restore_terminal(&mut term)?;
    Ok(())
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// Headless check of the non-UI logic: cookie, roll, download, deleted-detection.
fn selftest() -> io::Result<()> {
    let found = cookie::find();
    match &found {
        Some(f) => println!("cookie: OK (len {}, from {})", f.value.len(), f.profile),
        None => println!("cookie: NONE (log into Roblox in Firefox to enable playback)"),
    }
    let api = Api::new(found.map(|f| f.value));

    print!("roll 5 music... ");
    let batch = api.random_batch(Filter::Music, 5, false).unwrap_or_default();
    println!("got {}", batch.len());
    assert!(!batch.is_empty(), "roll returned nothing");
    for s in &batch {
        println!("  {} | {}s | {} | {}", s.name, s.duration, s.kind, s.id);
    }

    print!("deleted-detection (real id + fake id)... ");
    let real = batch[0].id;
    let alive = api.existing_ids(&[real, 99_999_999_999_999]).unwrap_or_default();
    assert!(alive.contains(&real), "real id reported missing");
    assert!(!alive.contains(&99_999_999_999_999), "fake id reported alive");
    println!("OK");

    if api.has_cookie() {
        print!("download {}... ", real);
        let bytes = api.download(real).expect("download failed");
        assert!(bytes.len() > 1000, "audio too small: {} bytes", bytes.len());
        assert_ne!(&bytes[..2], &[0x1f, 0x8b], "bytes still gzip-compressed");
        assert_eq!(&bytes[..4], b"OggS", "not Ogg audio");
        println!("OK ({} bytes, OggS verified)", bytes.len());
    } else {
        println!("download: skipped (no cookie)");
    }
    println!("\nALL CHECKS PASSED");
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode) {
    match app.screen {
        Screen::FirstRun => {
            if matches!(code, KeyCode::Enter | KeyCode::Char(' ')) {
                let _ = std::fs::write(first_run_marker(), "ok");
                app.screen = Screen::Browse;
                app.refresh_saved();
            } else if matches!(code, KeyCode::Char('q') | KeyCode::Esc) {
                app.quit = true;
            }
        }
        Screen::Browse => handle_browse(app, code),
        Screen::Saved => handle_saved(app, code),
    }
}

fn handle_browse(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        KeyCode::Tab => {
            app.screen = Screen::Saved;
            app.refresh_saved();
        }
        KeyCode::Char('m') => {
            app.filter = match app.filter {
                Filter::Both => Filter::Music,
                Filter::Music => Filter::Sfx,
                Filter::Sfx => Filter::Both,
            };
            app.status = format!("Filter: {}", app.filter.label());
        }
        KeyCode::Char('y') => {
            app.cyrillic = !app.cyrillic;
            app.status = if app.cyrillic {
                "Cyrillic-only: ON (press r to roll)".into()
            } else {
                "Cyrillic-only: OFF".into()
            };
        }
        KeyCode::Char('r') => app.roll(),
        KeyCode::Char('p') => app.player.stop(),
        KeyCode::Up | KeyCode::Char('k') => {
            app.browse_sel = app.browse_sel.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.browse_sel + 1 < app.browse.len() {
                app.browse_sel += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(s) = app.selected_browse() {
                let (id, name) = (s.id, s.name.clone());
                app.play_id(id, &name);
            }
        }
        KeyCode::Char('c') => {
            if let Some(s) = app.selected_browse() {
                let id = s.id;
                app.copy(id);
            }
        }
        KeyCode::Char('s') => app.save_current(),
        KeyCode::Char(d @ '0'..='9') => {
            if app.count_input.len() < 4 {
                if app.count_input == "0" {
                    app.count_input.clear();
                }
                app.count_input.push(d);
            }
        }
        KeyCode::Backspace => {
            app.count_input.pop();
            if app.count_input.is_empty() {
                app.count_input.push('0');
            }
        }
        _ => {}
    }
}

fn handle_saved(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        KeyCode::Tab => app.screen = Screen::Browse,
        KeyCode::Char('p') => app.player.stop(),
        KeyCode::Up | KeyCode::Char('k') => {
            app.saved_sel = app.saved_sel.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.saved_sel + 1 < app.saved.len() {
                app.saved_sel += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(s) = app.selected_saved() {
                let (id, name, dead) = (s.id, s.name.clone(), s.deleted);
                if dead {
                    app.status = "That sound is deleted — won't play.".into();
                } else {
                    app.play_id(id, &name);
                }
            }
        }
        KeyCode::Char('c') => {
            if let Some(s) = app.selected_saved() {
                let id = s.id;
                app.copy(id);
            }
        }
        KeyCode::Char('x') => app.remove_saved(),
        KeyCode::Char('r') => app.refresh_saved(),
        _ => {}
    }
}

// ---------- rendering ----------

const ACCENT: Color = Color::Rgb(74, 246, 158); // proto-green
const DIM: Color = Color::Rgb(90, 104, 96);
const MUTED: Color = Color::Rgb(150, 160, 152);

fn panel(title: &str) -> Block {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn row_style(selected: bool, deleted: bool) -> Style {
    if selected {
        Style::default().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else if deleted {
        Style::default().fg(Color::Rgb(230, 90, 90))
    } else {
        Style::default().fg(Color::Rgb(215, 222, 216))
    }
}

fn ui(f: &mut Frame, app: &App) {
    if let Screen::FirstRun = app.screen {
        draw_first_run(f, app);
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Length(3), // tabs
        Constraint::Length(3), // info bar
        Constraint::Min(3),    // table
        Constraint::Length(1), // status
        Constraint::Length(1), // keybar
    ])
    .split(f.area());

    let tab_idx = if matches!(app.screen, Screen::Browse) { 0 } else { 1 };
    let tabs = Tabs::new(vec![" ♫ Browse ", " ★ Saved "])
        .select(tab_idx)
        .block(panel("Roblox Audio Roulette"))
        .style(Style::default().fg(MUTED))
        .highlight_style(Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD))
        .divider(Span::styled("│", Style::default().fg(DIM)));
    f.render_widget(tabs, chunks[0]);

    match app.screen {
        Screen::Browse => {
            draw_info_bar(f, app, chunks[1]);
            draw_table(f, chunks[2], " Browse ", &app.browse_rows(), app.browse_sel, app.browse.len());
        }
        Screen::Saved => {
            let logged = if app.api.has_cookie() { "logged in" } else { "NOT logged in" };
            let dead = app.saved.iter().filter(|s| s.deleted).count();
            let bar = info_line(vec![
                ("Saved", format!("{}", app.saved.len()), ACCENT),
                ("Deleted", format!("{dead}"), Color::Rgb(230, 90, 90)),
                ("Login", logged.into(), if app.api.has_cookie() { ACCENT } else { Color::Rgb(230,90,90) }),
            ]);
            f.render_widget(Paragraph::new(bar).block(panel("Status")), chunks[1]);
            draw_table(f, chunks[2], " Saved ", &app.saved_rows(), app.saved_sel, app.saved.len());
        }
        Screen::FirstRun => {}
    }

    // status line (spinner while a background job runs)
    let lead = if app.busy {
        format!(" {} ", SPINNER[app.spin])
    } else {
        " › ".into()
    };
    let status = Paragraph::new(Line::from(vec![
        Span::styled(lead, Style::default().fg(ACCENT)),
        Span::styled(app.status.clone(), Style::default().fg(Color::Rgb(225, 230, 225))),
    ]));
    f.render_widget(status, chunks[3]);

    // keybar
    let keys = match app.screen {
        Screen::Browse => vec![
            ("↑↓", "move"), ("enter", "play"), ("p", "stop"), ("c", "copy"),
            ("s", "save"), ("r", "roll"), ("m", "filter"), ("y", "cyrillic"),
            ("0-9", "count"), ("tab", "saved"), ("q", "quit"),
        ],
        _ => vec![
            ("↑↓", "move"), ("enter", "play"), ("p", "stop"), ("c", "copy"),
            ("x", "remove"), ("r", "re-check"), ("tab", "browse"), ("q", "quit"),
        ],
    };
    f.render_widget(Paragraph::new(keybar(keys)), chunks[4]);
}

fn info_line(fields: Vec<(&str, String, Color)>) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, v, c)) in fields.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   •   ", Style::default().fg(DIM)));
        }
        spans.push(Span::styled(format!("{k}: "), Style::default().fg(MUTED)));
        spans.push(Span::styled(v, Style::default().fg(c).add_modifier(Modifier::BOLD)));
    }
    Line::from(spans)
}

fn draw_info_bar(f: &mut Frame, app: &App, area: Rect) {
    let logged = app.api.has_cookie();
    let line = info_line(vec![
        ("Filter", app.filter.label().into(), ACCENT),
        ("Cyrillic", if app.cyrillic { "ON" } else { "off" }.into(),
            if app.cyrillic { ACCENT } else { MUTED }),
        ("Count", app.count_input.clone(), ACCENT),
        ("Loaded", format!("{}", app.browse.len()), MUTED),
        ("Login", if logged { "yes" } else { "no" }.into(),
            if logged { ACCENT } else { Color::Rgb(230, 90, 90) }),
    ]);
    f.render_widget(Paragraph::new(line).block(panel("Browse random audio")), area);
}

fn keybar(keys: Vec<(&str, &str)>) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, label) in keys {
        spans.push(Span::styled(format!(" {k} "), Style::default().fg(Color::Black).bg(ACCENT)));
        spans.push(Span::styled(format!(" {label}  "), Style::default().fg(MUTED)));
    }
    Line::from(spans)
}

/// A rendered table row: (name, dur, kind, id, deleted).
type RowData = (String, u32, String, u64, bool);

fn draw_table(f: &mut Frame, area: Rect, title: &str, rows: &[RowData], sel: usize, total: usize) {
    let header = Row::new(vec![
        Cell::from("NAME"),
        Cell::from("LEN"),
        Cell::from("TYPE"),
        Cell::from("ID"),
    ])
    .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
    .bottom_margin(0);

    let body: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, (name, dur, kind, id, deleted))| {
            let name_cell = if *deleted {
                format!("{name}  [DELETED]")
            } else {
                name.clone()
            };
            Row::new(vec![
                Cell::from(name_cell),
                Cell::from(format!("{dur}s")),
                Cell::from(short_kind(kind).to_string()),
                Cell::from(id.to_string()),
            ])
            .style(row_style(i == sel, *deleted))
        })
        .collect();

    let title = format!("{title}({total})");
    let table = Table::new(
        body,
        [
            Constraint::Min(20),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(16),
        ],
    )
    .header(header)
    .block(panel(&title))
    .row_highlight_style(Style::default()); // row styling handled per-row

    let mut state = TableState::default();
    if total > 0 {
        state.select(Some(sel));
    }
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_first_run(f: &mut Frame, app: &App) {
    let profile = app
        .cookie_profile
        .clone()
        .unwrap_or_else(|| "none — log into roblox.com in Firefox first".into());
    let a = |s: &str| Span::styled(s.to_string(), Style::default().fg(ACCENT));
    let t = |s: &str| Span::styled(s.to_string(), Style::default().fg(Color::Rgb(215, 222, 216)));
    let d = |s: &str| Span::styled(s.to_string(), Style::default().fg(MUTED));

    let lines = vec![
        Line::from(Span::styled("ROBLOX AUDIO ROULETTE", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(d("random sounds from the Creator Store")),
        Line::from(""),
        Line::from(vec![a("▌ "), t("Roll random music & sound effects, copy the ID, save the good ones.")]),
        Line::from(vec![a("▌ "), t("To play a sound, Roblox needs you logged in — here's how that works:")]),
        Line::from(""),
        Line::from(vec![d("  • "), t("Reads your existing "), a("Firefox"), t(" login. No password typed, nothing")]),
        Line::from(vec![d("    "), t("sent anywhere except Roblox's own servers when playing a sound.")]),
        Line::from(vec![d("  • cookie: "), t(&profile)]),
        Line::from(vec![d("  • files:  "), t(&store::config_dir().display().to_string())]),
        Line::from(vec![d("  • logout: "), t("log out of Roblox in Firefox, or delete that folder.")]),
        Line::from(""),
        Line::from(d("  No email, password, or Robux is ever touched.")),
        Line::from(""),
        Line::from(vec![
            Span::styled(" ENTER ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
            d("  continue      "),
            Span::styled(" Q ", Style::default().fg(Color::Black).bg(DIM)),
            d("  quit"),
        ]),
    ];
    let p = Paragraph::new(lines)
        .block(panel("Welcome"))
        .wrap(Wrap { trim: false });
    f.render_widget(p, f.area());
}

fn short_kind(k: &str) -> &str {
    match k {
        "Music" => "Music",
        "SoundEffect" => "SFX",
        _ => "?",
    }
}

// ---------- terminal plumbing ----------

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(term: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(term.backend_mut(), terminal::LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}
