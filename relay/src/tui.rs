//! Dev-only live dashboard (TUI) over the relay flow (`NIGHTDROP_RELAY_TUI=1`; §11.9). A thin
//! presentation layer on the same `RelayLogger` event stream + a `RelayCore::snapshot()` — it
//! shows metadata only (handles, depths, sizes, ages), **never blob bytes**, and is never built
//! into production use. Press `q`/Esc to quit.

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nightdrop::relay_client::{RelayCore, RelayEvent};
use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

/// Run the dashboard until the user quits (then restores the terminal and exits the process).
pub fn run_tui(onion: String, core: Arc<RelayCore>, rx: Receiver<RelayEvent>, start: Instant) {
    let mut terminal = ratatui::init();
    let mut flow: Vec<String> = Vec::new();
    let mut reaped: u64 = 0;

    loop {
        while let Ok(ev) = rx.try_recv() {
            if ev.op == "REAP" {
                reaped += ev
                    .result
                    .strip_prefix("dropped=")
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
            }
            flow.push(format!("{}  {}", now_hms(), ev.summary()));
            if flow.len() > 500 {
                flow.drain(0..flow.len() - 500);
            }
        }

        let stats = core.snapshot();
        let total_queued: usize = stats.iter().map(|s| s.depth).sum();
        let uptime = start.elapsed().as_secs();

        let _ = terminal.draw(|f| {
            let rows = Layout::vertical([
                Constraint::Length(4),
                Constraint::Min(4),
                Constraint::Min(4),
            ])
            .split(f.area());

            let header = Paragraph::new(vec![
                Line::from(format!(" .onion  {}", trunc(&onion, 30))),
                Line::from(format!(
                    " uptime {}   queued {}   reaped {}   (press q to quit)",
                    fmt_dur(uptime),
                    total_queued,
                    reaped
                )),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Night Drop Relay (dev) — opaque blobs only, no identities "),
            );
            f.render_widget(header, rows[0]);

            let body: Vec<Row> = stats
                .iter()
                .map(|s| {
                    Row::new(vec![
                        Cell::from(trunc(&s.handle, 24)),
                        Cell::from(s.depth.to_string()),
                        Cell::from(fmt_dur(s.oldest_age_secs)),
                        Cell::from(fmt_bytes(s.total_bytes)),
                    ])
                })
                .collect();
            let table = Table::new(
                body,
                [
                    Constraint::Min(26),
                    Constraint::Length(7),
                    Constraint::Length(10),
                    Constraint::Length(10),
                ],
            )
            .header(
                Row::new(vec!["mailbox", "depth", "oldest", "size"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Mailboxes ({}) ", stats.len())),
            );
            f.render_widget(table, rows[1]);

            let visible = rows[2].height.saturating_sub(2) as usize;
            let from = flow.len().saturating_sub(visible);
            let lines: Vec<Line> = flow[from..]
                .iter()
                .map(|l| Line::from(l.as_str()))
                .collect();
            let flow_pane = Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Live flow — src=anonymous(Tor) "),
            );
            f.render_widget(flow_pane, rows[2]);
        });

        // Up to 250ms waiting for a keypress doubles as the render cadence.
        if event::poll(Duration::from_millis(250)).unwrap_or(false) {
            if let Ok(Event::Key(k)) = event::read() {
                if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break;
                }
            }
        }
    }

    ratatui::restore();
    std::process::exit(0);
}

// Char-boundary-safe: mailbox handles are client-supplied, so byte-slicing could panic.
fn trunc(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn fmt_dur(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

fn fmt_bytes(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1}MB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1}kB", n as f64 / (1u64 << 10) as f64)
    } else {
        format!("{n}B")
    }
}

fn now_hms() -> String {
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}
