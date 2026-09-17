//! Full-screen terminal chat for `agos-proxy chat --tui`.
//!
//! A ratatui front end over the exact same routing path as the line-mode
//! chat: the profile/proxy/route wizards run first, then the conversation
//! moves into an alternate-screen UI with a scrolling transcript, an input
//! box and a thinking spinner while the failover chain works. Client-side
//! commands (`/help`, `/clear`, `/quit`) behave like line mode but render
//! inside the transcript.
//!
//! Turns run as tasks on a shared tokio runtime so the UI never blocks:
//! each submit spawns a turn that posts its outcome back over a std
//! channel, polled on every UI tick.

use std::io::{self, Stdout};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand as _;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::runtime::Handle;

use crate::cli::chat::send_turn;
use crate::domain::Profile;
use crate::router::RoutingState;
use crate::storage::Store;
use crate::translator::Message;

/// Everything the wizards collected before the TUI opens.
pub struct Session {
    /// Opened store, shared with the background turn threads.
    pub store: Arc<Store>,
    /// Selected profile (id + display name).
    pub profile: Profile,
    /// `proxy/route` model string passed to the translator.
    pub model: String,
    /// HTTP client shared by every turn.
    pub client: reqwest::Client,
    /// Number of models in the fallback chain (display only).
    pub chain_len: usize,
}

/// Run `agos-proxy chat --tui`. Terminal modes are always restored, including
/// on error.
pub fn run(session: Session) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting async runtime")?;

    enable_raw_mode().context("enabling raw mode")?;
    // Restores raw mode + the main screen even when the loop errors or panics.
    let _guard = ScreenGuard;
    io::stdout()
        .execute(EnterAlternateScreen)
        .context("entering alternate screen")?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("creating terminal")?;

    let mut app = ChatApp::new(session);
    let (tx, rx) = mpsc::channel::<TurnResult>();
    let outcome = event_loop(&mut terminal, runtime.handle(), &mut app, tx, rx);

    drop(_guard);
    outcome
}

/// Drive the UI loop until the user quits. Events are drained between frames;
/// finished turns are polled each tick so the UI never blocks on providers.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    runtime: &Handle,
    app: &mut ChatApp,
    tx: mpsc::Sender<TurnResult>,
    rx: Receiver<TurnResult>,
) -> Result<()> {
    loop {
        if app.quit {
            break;
        }
        while event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(app, runtime, tx.clone(), key);
                }
            }
            if app.quit {
                break;
            }
        }
        if app.quit {
            break;
        }

        app.tick += 1;
        app.drain_results(&rx);
        terminal
            .draw(|frame| draw(frame, app))
            .context("drawing frame")?;
    }
    Ok(())
}

/// Classify an input line that starts with `/` (leading slash already
/// stripped by the caller).
#[derive(Debug, PartialEq, Eq)]
pub enum TuiCommand {
    /// `/help` — show the command list.
    Help,
    /// `/clear` — forget the conversation.
    Clear,
    /// `/quit` — leave the session.
    Quit,
    /// Anything else starting with `/`.
    Unknown(String),
}

/// Map a slash-command body to its action.
pub fn tui_command(rest: &str) -> TuiCommand {
    match rest.trim() {
        "help" => TuiCommand::Help,
        "clear" | "reset" => TuiCommand::Clear,
        "quit" | "exit" | "bye" => TuiCommand::Quit,
        other => TuiCommand::Unknown(other.to_string()),
    }
}

/// A rendered transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// Who produced this entry.
    pub speaker: Speaker,
    /// Entry text (multi-line capable).
    pub text: String,
    /// Wall-clock milliseconds for assistant turns.
    pub latency_ms: Option<u128>,
}

/// Transcript speakers, each rendered with its own prefix and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// What the user typed.
    User,
    /// What the model answered.
    Assistant,
    /// A failed turn.
    Error,
    /// Client-side notices (help text, clear confirmation).
    Info,
}

impl Speaker {
    /// Prompt prefix shown on the first line of a turn.
    fn prefix(self) -> &'static str {
        match self {
            Speaker::User => "you› ",
            Speaker::Assistant => "assistant› ",
            Speaker::Error => "! ",
            Speaker::Info => "· ",
        }
    }

    /// Spaces applied to continuation lines.
    fn indent(self) -> usize {
        match self {
            Speaker::User => 5,
            Speaker::Assistant => 11,
            Speaker::Error | Speaker::Info => 2,
        }
    }

    /// Text colour for the prefix.
    fn color(self) -> Color {
        match self {
            Speaker::User => Color::Cyan,
            Speaker::Assistant => Color::Green,
            Speaker::Error => Color::Red,
            Speaker::Info => Color::DarkGray,
        }
    }
}

/// Outcome of one background turn.
pub enum TurnResult {
    /// The provider answered: `(reply text, latency ms)`.
    Reply(String, u128),
    /// Every target failed.
    Failed(String),
}

/// Whole UI state for one chat session.
pub struct ChatApp {
    session: Session,
    routing: RoutingState,
    /// Real conversation history sent with every turn.
    history: Vec<Message>,
    /// Rendered transcript.
    transcript: Vec<Turn>,
    /// Current input line.
    input: String,
    /// Cursor position as a char index into `input`.
    cursor: usize,
    /// Rows scrolled up from the bottom; 0 pins to the newest turn.
    scroll_up: usize,
    /// True while a background turn is in flight.
    busy: bool,
    /// Incremented per loop iteration; drives the spinner.
    tick: usize,
    /// Previously submitted input lines, oldest first.
    input_history: Vec<String>,
    /// Position while browsing `input_history`; `None` when not browsing.
    input_history_idx: Option<usize>,
    /// Input preserved while browsing history.
    input_draft: String,
    /// Set when the session should end.
    quit: bool,
}

impl ChatApp {
    /// Build a fresh session with a greeting in the transcript.
    pub fn new(session: Session) -> Self {
        let model = session.model.clone();
        let chain = session.chain_len;
        let transcript = vec![Turn {
            speaker: Speaker::Info,
            text: format!(
                "chatting on {model} — {chain} model(s) in the fallback chain; \
                 /help for commands, Esc to leave"
            ),
            latency_ms: None,
        }];
        Self {
            session,
            routing: RoutingState::default(),
            history: Vec::new(),
            transcript,
            input: String::new(),
            cursor: 0,
            scroll_up: 0,
            busy: false,
            tick: 0,
            input_history: Vec::new(),
            input_history_idx: None,
            input_draft: String::new(),
            quit: false,
        }
    }

    /// Submit the current input line: run slash commands, otherwise start a
    /// background turn.
    pub fn submit(&mut self, runtime: &Handle, tx: mpsc::Sender<TurnResult>) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            match tui_command(rest) {
                TuiCommand::Help => self.push_info(
                    "/help   show this help\n/clear  forget the conversation\n/quit   leave the session",
                ),
                TuiCommand::Clear => {
                    self.history.clear();
                    self.transcript.clear();
                    self.push_info("conversation cleared");
                }
                TuiCommand::Quit => self.quit = true,
                TuiCommand::Unknown(other) => self.transcript.push(Turn {
                    speaker: Speaker::Error,
                    text: format!("unknown command /{other}; try /help"),
                    latency_ms: None,
                }),
            }
            self.input_history.push(text);
            self.reset_input();
            return;
        }

        self.history.push(Message::text("user", text.clone()));
        self.transcript.push(Turn {
            speaker: Speaker::User,
            text,
            latency_ms: None,
        });
        self.input_history.push(self.input.trim().to_string());
        self.reset_input();

        self.busy = true;
        spawn_turn(
            runtime,
            &self.session,
            self.history.clone(),
            self.routing.clone(),
            tx,
        );
    }

    /// Poll finished background turns and fold them into the transcript.
    pub fn drain_results(&mut self, rx: &Receiver<TurnResult>) {
        loop {
            match rx.try_recv() {
                Ok(TurnResult::Reply(reply, ms)) => {
                    self.history.push(Message::text("assistant", reply.clone()));
                    self.transcript.push(Turn {
                        speaker: Speaker::Assistant,
                        text: reply,
                        latency_ms: Some(ms),
                    });
                    self.busy = false;
                }
                Ok(TurnResult::Failed(reason)) => {
                    self.transcript.push(Turn {
                        speaker: Speaker::Error,
                        text: format!("no reply — {reason}; /clear resets the context"),
                        latency_ms: None,
                    });
                    self.busy = false;
                }
                Err(_) => break,
            }
        }
    }

    /// Move the cursor left by one character.
    pub fn cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move the cursor right by one character.
    pub fn cursor_right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
        }
    }

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        let byte = self.byte_at(self.cursor);
        self.input.insert(byte, c);
        self.cursor += 1;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_at(self.cursor);
        let start = self.byte_at(self.cursor - 1);
        self.input.drain(start..end);
        self.cursor -= 1;
    }

    /// Delete the character under the cursor.
    pub fn delete_at_cursor(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.input.drain(start..end);
    }

    /// Recall the previous submitted line.
    pub fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.input_history_idx {
            None => {
                self.input_draft = self.input.clone();
                self.input_history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.input_history_idx = Some(idx);
        self.input = self.input_history[idx].clone();
        self.cursor = self.input.chars().count();
    }

    /// Move forward through submitted lines (or back to the draft).
    pub fn history_down(&mut self) {
        let Some(idx) = self.input_history_idx else {
            return;
        };
        if idx + 1 >= self.input_history.len() {
            self.input_history_idx = None;
            self.input = std::mem::take(&mut self.input_draft);
        } else {
            self.input_history_idx = Some(idx + 1);
            self.input = self.input_history[idx + 1].clone();
        }
        self.cursor = self.input.chars().count();
    }

    /// Scroll the transcript up by `rows`.
    pub fn scroll_up_by(&mut self, rows: u16) {
        self.scroll_up += rows as usize;
    }

    /// Scroll the transcript down by `rows` (0 = pinned to the newest turn).
    pub fn scroll_down_by(&mut self, rows: u16) {
        self.scroll_up = self.scroll_up.saturating_sub(rows as usize);
    }

    /// Scroll offset for a transcript of `total` rendered lines with `visible`
    /// rows on screen.
    pub fn offset(&self, total: usize, visible: usize) -> usize {
        total.saturating_sub(visible).saturating_sub(self.scroll_up)
    }

    fn reset_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.input_history_idx = None;
        self.input_draft.clear();
        self.scroll_up = 0;
    }

    fn push_info(&mut self, text: &str) {
        self.transcript.push(Turn {
            speaker: Speaker::Info,
            text: text.to_string(),
            latency_ms: None,
        });
    }

    /// Byte offset of the `n`-th char in `input`.
    fn byte_at(&self, n: usize) -> usize {
        self.input
            .char_indices()
            .nth(n)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }
}

/// Handle one key press.
pub fn handle_key(
    app: &mut ChatApp,
    runtime: &Handle,
    tx: mpsc::Sender<TurnResult>,
    key: KeyEvent,
) {
    match key.code {
        KeyCode::Esc => app.quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.quit = true,
        KeyCode::Enter => app.submit(runtime, tx),
        KeyCode::Char(c) => app.insert_char(c),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete_at_cursor(),
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Up => app.history_up(),
        KeyCode::Down => app.history_down(),
        KeyCode::PageUp => app.scroll_up_by(10),
        KeyCode::PageDown => app.scroll_down_by(10),
        _ => {}
    }
}

/// Greedy word wrap used to pre-wrap transcript turns. Long words are
/// hard-split at the boundary.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            if word.chars().count() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let mut buf = String::new();
                for ch in word.chars() {
                    buf.push(ch);
                    if buf.chars().count() == width {
                        out.push(std::mem::take(&mut buf));
                    }
                }
                line = buf;
                continue;
            }
            if line.is_empty() {
                line = word.to_string();
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Render the whole frame: header, transcript, input.
fn draw(frame: &mut Frame, app: &mut ChatApp) {
    let area = frame.area();
    let [header, body, input] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(area);

    let header_line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.session.model),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " profile {:?} · {} model(s) in chain",
                app.session.profile.name, app.session.chain_len
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header_line).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" agos-proxy chat "),
        ),
        header,
    );

    let lines = transcript_lines(app, body.width.saturating_sub(2) as usize);
    let visible = body.height.saturating_sub(2) as usize;
    let mut state = ListState::default();
    *state.offset_mut() = app.offset(lines.len(), visible);
    frame.render_stateful_widget(
        List::new(lines.into_iter().map(ListItem::new)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" transcript (PageUp/PageDown scrolls) "),
        ),
        body,
        &mut state,
    );

    frame.render_widget(
        Paragraph::new(app.input.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" you (Enter to send) "),
        ),
        input,
    );
    // Place the block cursor inside the input box at the logical position.
    let cursor_col = input.x + 1 + app.cursor.min(app.input.chars().count()) as u16;
    frame.set_cursor_position((cursor_col, input.y + 1));
}

/// Build the styled transcript lines for the current app state.
fn transcript_lines(app: &ChatApp, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    for turn in &app.transcript {
        let text_width = width.saturating_sub(turn.speaker.indent());
        for (i, seg) in wrap_text(&turn.text, text_width).into_iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(
                        turn.speaker.prefix().to_string(),
                        Style::default().fg(turn.speaker.color()),
                    ),
                    Span::raw(seg),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(turn.speaker.indent())),
                    Span::raw(seg),
                ]));
            }
        }
        if let Some(ms) = turn.latency_ms {
            lines.push(Line::from(Span::styled(
                format!("{}· {ms} ms", " ".repeat(turn.speaker.indent())),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    if app.busy {
        const SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
        lines.push(Line::from(Span::styled(
            format!("  {} thinking…", SPINNER[app.tick % SPINNER.len()]),
            Style::default().fg(Color::Yellow),
        )));
    }
    lines
}

/// Send one turn on the shared tokio runtime so the UI keeps repainting.
fn spawn_turn(
    runtime: &Handle,
    session: &Session,
    messages: Vec<Message>,
    routing: RoutingState,
    tx: mpsc::Sender<TurnResult>,
) {
    let store = session.store.clone();
    let client = session.client.clone();
    let profile_id = session.profile.id.clone();
    let model = session.model.clone();
    runtime.spawn(async move {
        let started = std::time::Instant::now();
        let result = send_turn(
            &store,
            &client,
            &routing,
            profile_id.as_str(),
            model.as_str(),
            messages,
            std::time::Duration::from_secs(60),
        )
        .await;
        let ms = started.elapsed().as_millis();
        let _ = tx.send(match result {
            Ok(reply) => TurnResult::Reply(reply, ms),
            Err(e) => TurnResult::Failed(format!("{e:#}")),
        });
    });
}

/// Restore terminal modes; runs on drop even after an error or panic.
struct ScreenGuard;

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::time::Duration;

    use crate::domain::{ProviderKind, RoutingStrategy};
    use crate::storage::{NewProvider, Store};

    #[test]
    fn tui_command_maps_slash_commands() {
        assert_eq!(tui_command("help"), TuiCommand::Help);
        assert_eq!(tui_command("clear"), TuiCommand::Clear);
        assert_eq!(tui_command("reset"), TuiCommand::Clear);
        assert_eq!(tui_command("quit"), TuiCommand::Quit);
        assert_eq!(tui_command("exit"), TuiCommand::Quit);
        assert_eq!(tui_command("wat"), TuiCommand::Unknown("wat".to_string()));
    }

    #[test]
    fn wrap_text_wraps_words_and_hard_splits_long_words() {
        assert_eq!(wrap_text("short line", 40), vec!["short line"]);
        assert_eq!(wrap_text("aa bb cc dd", 5), vec!["aa bb", "cc dd"]);
        assert_eq!(wrap_text("aaaabbb", 3), vec!["aaa", "abb", "b"]);
        assert_eq!(wrap_text("one\ntwo", 10), vec!["one", "two"]);
    }

    #[test]
    fn offset_pins_to_bottom_and_honors_scroll_up() {
        let mut session = test_session();
        session.model = "prog/r1".into();
        let app = ChatApp::new(session);
        // 2 rendered lines (greeting), 1 visible row.
        assert_eq!(app.offset(2, 1), 1);
        assert_eq!(app.offset(10, 4), 6);
        assert_eq!(app.offset(3, 5), 0);
    }

    #[test]
    fn input_editing_and_history_round_trip() {
        let mut app = ChatApp::new(test_session());
        for c in "hello".chars() {
            app.insert_char(c);
        }
        assert_eq!(app.cursor, 5);
        app.cursor_left();
        app.insert_char('!');
        assert_eq!(app.input, "hell!o");
        app.backspace();
        assert_eq!(app.input, "hello");
        app.delete_at_cursor();
        assert_eq!(app.input, "hell");

        // History: submit a line, browse back to it, then back to the draft.
        app.input = "sent line".into();
        app.input_history.push("sent line".into());
        app.input.clear();
        app.input.push('d');
        app.input_draft.clear();
        app.history_up();
        assert_eq!(app.input, "sent line");
        app.history_down();
        assert_eq!(app.input, "d");
    }

    #[tokio::test]
    async fn submit_and_drain_round_trip_through_mock_upstream() {
        let port = 19889;
        let base = format!("http://127.0.0.1:{port}");
        let _mock = mock_upstream(port).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let store = Store::open_in_memory().expect("open store");
        let profile = store.create_profile("coder1", None, None).expect("profile");
        store
            .create_provider(
                &profile.id,
                NewProvider {
                    name: "mock".into(),
                    description: None,
                    base_url: base,
                    auth_token: "sk-mock".into(),
                    kind: ProviderKind::OpenAI,
                    extra_headers: BTreeMap::new(),
                    masking_server_id: None,
                },
            )
            .expect("provider");
        let provider = store.list_providers(&profile.id).expect("list")[0].clone();
        let proxy = store
            .create_proxy(&profile.id, "prog", None)
            .expect("proxy");
        let route = store
            .create_route(proxy.id, "r1", None, RoutingStrategy::Priority, None)
            .expect("route");
        store
            .add_route_entry(route.id, provider.id, "m1", 1, 1.0, Default::default())
            .expect("entry");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client");
        let handle = tokio::runtime::Handle::current();
        let session = Session {
            store: Arc::new(store),
            profile,
            model: "prog/r1".into(),
            client,
            chain_len: 1,
        };
        let mut app = ChatApp::new(session);

        for c in "hi".chars() {
            app.insert_char(c);
        }
        let (tx, rx) = mpsc::channel::<TurnResult>();
        app.submit(&handle, tx);
        assert!(app.busy);

        // Poll like the UI loop would until the background turn lands.
        let mut replied = false;
        for _ in 0..100 {
            app.drain_results(&rx);
            if !app.busy {
                replied = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(replied, "turn never completed");
        let last = app.transcript.last().expect("transcript non-empty");
        assert_eq!(last.speaker, Speaker::Assistant);
        assert_eq!(last.text, "hello from chat");
        assert!(last.latency_ms.is_some());
        // The reply is now part of the real history too.
        assert_eq!(app.history.len(), 2);
    }

    #[tokio::test]
    async fn slash_commands_stay_client_side() {
        // Slash commands never spawn work, so the #[tokio::test] runtime's
        // own handle is enough.
        let handle = tokio::runtime::Handle::current();
        let mut app = ChatApp::new(test_session());
        let (tx, rx) = mpsc::channel::<TurnResult>();

        app.input = "/help".into();
        app.submit(&handle, tx.clone());
        assert!(!app.busy);
        assert_eq!(app.transcript.last().unwrap().speaker, Speaker::Info);

        app.input = "/wat".into();
        app.submit(&handle, tx.clone());
        assert_eq!(app.transcript.last().unwrap().speaker, Speaker::Error);

        app.input = "/quit".into();
        app.submit(&handle, tx);
        assert!(app.quit);
        // Nothing was ever sent to a provider.
        assert!(rx.try_recv().is_err());
        assert!(app.history.is_empty());
    }

    fn test_session() -> Session {
        Session {
            store: Arc::new(Store::open_in_memory().expect("store")),
            profile: crate::domain::Profile {
                id: "p1".into(),
                name: "tester".into(),
                description: None,
                password_hash: None,
                created_at: 0,
                updated_at: 0,
                rpm_limit: 0,
                default_masking_server_id: None,
            },
            model: "prog/r1".into(),
            client: reqwest::Client::new(),
            chain_len: 1,
        }
    }

    /// A mock upstream that returns a fixed assistant reply.
    async fn mock_upstream(port: u16) -> tokio::task::JoinHandle<()> {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                let body = serde_json::json!({
                    "id": "mock-1",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "hello from chat" },
                        "finish_reason": "stop"
                    }]
                });
                (axum::http::StatusCode::OK, axum::Json(body))
            }),
        );
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .expect("bind mock upstream");
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })
    }
}
