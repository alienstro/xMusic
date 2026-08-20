//! Rendering. Reads [`App`] and draws; never mutates anything but list scroll
//! state, which ratatui owns.
//!
//! The interface is built as a readout rather than a set of panels: hairline
//! rules and a fixed left gutter carry the structure, so nothing is boxed. That
//! puts the whole weight of the layout on alignment, which is why the column
//! widths and meter arithmetic below are exact rather than approximate.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};

/// Tungsten amber, the backlight colour of an analogue VU meter, is the only
/// chromatic accent in the interface. Everything else is graded by brightness,
/// which reads correctly whatever colour scheme the terminal is set to.
mod ink {
    use ratatui::style::Color;

    pub const AMBER: Color = Color::Indexed(214);
    pub const EMBER: Color = Color::Indexed(179);
    pub const BONE: Color = Color::Indexed(252);
    pub const ASH: Color = Color::Indexed(245);
    pub const SLATE: Color = Color::Indexed(239);
    pub const ALARM: Color = Color::Indexed(203);
}

/// Width of the selection gutter. Rendered by the list widget as its highlight
/// symbol, and matched by hand in the column header so the two line up.
const GUTTER: &str = "▌ ";
const GUTTER_WIDTH: usize = 2;

/// Cells in the volume fader.
const FADER_CELLS: usize = 8;

/// Eighth-width blocks give the progress meter sub-character resolution, so it
/// advances smoothly at the client's poll rate instead of stepping a whole cell.
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let full = frame.area();
    if full.width < 30 || full.height < 11 {
        frame.render_widget(
            Paragraph::new("Terminal too small — needs 30x11").style(Style::default().fg(ink::ASH)),
            full,
        );
        return;
    }

    // A column of air on each side. Without it the rules run into the terminal
    // edge and the whole thing reads as a box after all.
    let canvas = Rect {
        x: full.x + 1,
        y: full.y,
        width: full.width - 2,
        height: full.height,
    };

    // Chrome is dropped from the least essential end first. The meter is the
    // last thing to go, which is why the now-playing block gives up its
    // subtitle before it gives up a line: an interface that cannot show you
    // where you are in the track has stopped being a music player.
    let show_columns = canvas.height >= 16;
    let show_hints = canvas.height >= 13;
    let show_subtitle = canvas.height >= 12;
    let now_playing_height = if show_subtitle { 3 } else { 2 };

    let mut rows = vec![
        Constraint::Length(1), // identity + connection
        Constraint::Length(1), // rule
        Constraint::Length(1), // search
        Constraint::Length(1), // rule
    ];
    if show_columns {
        rows.push(Constraint::Length(1));
    }
    rows.push(Constraint::Min(2)); // results
    rows.push(Constraint::Length(1)); // rule
    rows.push(Constraint::Length(now_playing_height));
    rows.push(Constraint::Length(1)); // rule
    rows.push(Constraint::Length(1)); // status
    if show_hints {
        rows.push(Constraint::Length(1));
    }

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(rows)
        .split(canvas);

    let mut row = areas.iter().copied();
    let mut next = || row.next().expect("layout produced every row requested");

    draw_header(frame, app, next());
    draw_rule(frame, next());
    draw_search(frame, app, next());
    draw_rule(frame, next());
    let columns = Columns::for_width(canvas.width as usize);
    if show_columns {
        draw_column_header(frame, next(), &columns);
    }
    draw_results(frame, app, next(), &columns);
    draw_rule(frame, next());
    draw_now_playing(frame, app, next(), show_subtitle);
    draw_rule(frame, next());
    draw_status(frame, app, next());
    if show_hints {
        let area = next();
        frame.render_widget(
            Paragraph::new(key_hints(&app.mode, area.width as usize))
                .style(Style::default().fg(ink::SLATE)),
            area,
        );
    }
}

// ------------------------------------------------------------------ chrome ----

fn draw_rule(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(Style::default().fg(ink::SLATE)),
        area,
    );
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let (state, state_style) = if !app.online {
        ("not connected", Style::default().fg(ink::ALARM))
    } else if app.player.logged_in {
        ("signed in", Style::default().fg(ink::ASH))
    } else {
        ("signed out", Style::default().fg(ink::ASH))
    };

    let left_width = 6; // "xmusic"

    let gap = (area.width as usize).saturating_sub(left_width + display_width(state));

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("x", Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD)),
            Span::styled("music", Style::default().fg(ink::BONE).add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(gap)),
            Span::styled(state, state_style),
        ])),
        area,
    );
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    let editing = matches!(app.mode, Mode::Editing);
    let prompt_style = if editing {
        Style::default().fg(ink::AMBER)
    } else {
        Style::default().fg(ink::SLATE)
    };

    let body = if editing {
        Span::styled(app.input.clone(), Style::default().fg(ink::BONE))
    } else {
        Span::styled("Search songs", Style::default().fg(ink::SLATE))
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", prompt_style),
            body,
        ])),
        area,
    );

    if editing {
        // The terminal's own cursor, rather than a drawn stand-in: it blinks the
        // way the user's terminal blinks and needs no glyph of its own.
        let input_width = display_width(&app.input).min(u16::MAX as usize) as u16;
        let x = area.x.saturating_add(2).saturating_add(input_width);
        frame.set_cursor_position((x.min(area.x + area.width.saturating_sub(1)), area.y));
    }
}

// ----------------------------------------------------------------- results ----

/// Column widths for the results table. Album is the first thing to go on a
/// narrow terminal, then artist; the title and the running time always survive.
struct Columns {
    title: usize,
    artist: usize,
    album: usize,
    time: usize,
}

impl Columns {
    const GAP: usize = 2;
    const TIME: usize = 5;

    fn for_width(width: usize) -> Self {
        let available = width.saturating_sub(GUTTER_WIDTH);
        let time = Self::TIME;

        let (artist, album) = if available >= 78 {
            (22, 22)
        } else if available >= 58 {
            (20, 0)
        } else if available >= 40 {
            (16, 0)
        } else {
            (0, 0)
        };

        let occupied = artist
            + album
            + time
            + Self::GAP * [artist, album, time].iter().filter(|w| **w > 0).count();

        Self {
            title: available.saturating_sub(occupied).max(6),
            artist,
            album,
            time,
        }
    }
}

fn draw_column_header(frame: &mut Frame, area: Rect, columns: &Columns) {
    let mut line = " ".repeat(GUTTER_WIDTH);
    line.push_str(&pad("TITLE", columns.title));
    if columns.artist > 0 {
        line.push_str(&" ".repeat(Columns::GAP));
        line.push_str(&pad("ARTIST", columns.artist));
    }
    if columns.album > 0 {
        line.push_str(&" ".repeat(Columns::GAP));
        line.push_str(&pad("ALBUM", columns.album));
    }
    line.push_str(&" ".repeat(Columns::GAP));
    line.push_str(&format!("{:>width$}", "TIME", width = columns.time));

    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(ink::SLATE)),
        area,
    );
}

fn draw_results(frame: &mut Frame, app: &mut App, area: Rect, columns: &Columns) {
    if app.results.is_empty() {
        let message = if app.searching {
            "Searching"
        } else {
            "Search for a song to start playing"
        };
        frame.render_widget(
            Paragraph::new(format!("{}{message}", " ".repeat(GUTTER_WIDTH)))
                .style(Style::default().fg(ink::SLATE)),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|result| {
            // Colour carries what is playing; the gutter bar carries where the
            // selection is. Two separate channels, so neither needs a glyph.
            let playing = !result.video_id.is_empty() && result.video_id == app.player.video_id;
            let title_style = if playing {
                Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ink::BONE)
            };

            let mut spans = vec![Span::styled(pad(&result.title, columns.title), title_style)];
            if columns.artist > 0 {
                spans.push(Span::raw(" ".repeat(Columns::GAP)));
                spans.push(Span::styled(
                    pad(&result.artist, columns.artist),
                    Style::default().fg(ink::ASH),
                ));
            }
            if columns.album > 0 {
                spans.push(Span::raw(" ".repeat(Columns::GAP)));
                spans.push(Span::styled(
                    pad(&result.album, columns.album),
                    Style::default().fg(ink::SLATE),
                ));
            }
            spans.push(Span::raw(" ".repeat(Columns::GAP)));
            spans.push(Span::styled(
                format!("{:>width$}", result.duration, width = columns.time),
                Style::default().fg(ink::SLATE),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol(GUTTER)
            .highlight_style(Style::default().fg(ink::AMBER)),
        area,
        &mut app.list,
    );
}

// ------------------------------------------------------------- now playing ----

fn draw_now_playing(frame: &mut Frame, app: &App, area: Rect, show_subtitle: bool) {
    let player = &app.player;
    let indent = " ".repeat(GUTTER_WIDTH);

    let (marker, marker_style) = if !app.online {
        ("■", Style::default().fg(ink::ALARM))
    } else if player.is_buffering {
        ("◌", Style::default().fg(ink::ASH))
    } else if player.is_playing {
        ("▶", Style::default().fg(ink::AMBER))
    } else {
        ("▶", Style::default().fg(ink::SLATE))
    };

    let headline = if !app.online {
        Span::styled("Daemon not running", Style::default().fg(ink::ALARM))
    } else if player.title.is_empty() {
        Span::styled(
            if player.ready { "Nothing playing" } else { "Loading YouTube Music" },
            Style::default().fg(ink::SLATE),
        )
    } else {
        Span::styled(
            player.title.clone(),
            Style::default().fg(ink::BONE).add_modifier(Modifier::BOLD),
        )
    };

    let subtitle = if player.byline.is_empty() {
        player.artist.clone()
    } else {
        player.byline.clone()
    };

    let mut lines = vec![Line::from(vec![
        Span::styled(marker, marker_style),
        Span::raw(" ".repeat(GUTTER_WIDTH - 1)),
        headline,
    ])];
    if show_subtitle {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(
                truncate(&subtitle, (area.width as usize).saturating_sub(GUTTER_WIDTH)),
                Style::default().fg(ink::ASH),
            ),
        ]));
    }
    lines.push(transport(app, area.width as usize));

    frame.render_widget(Paragraph::new(lines), area);
}

/// The one line the interface is built around: elapsed timecode, a meter with
/// sub-character resolution, total running time, and a fader scale.
fn transport(app: &App, width: usize) -> Line<'static> {
    let player = &app.player;
    let elapsed = format!("{:<5}", clock(player.position));
    let total = format!("{:>5}", clock(player.duration));
    let volume = format!("{:>3}", player.volume);

    // gutter + elapsed + [meter] + gap + total + gap*2 + "vol" + gap + fader + gap + number
    let fixed =
        GUTTER_WIDTH + elapsed.len() + 1 + total.len() + 2 + 3 + 1 + FADER_CELLS + 1 + volume.len();
    let meter_width = width.saturating_sub(fixed);

    let fraction = if player.duration == 0 {
        0.0
    } else {
        // A position past the duration is briefly reported while a track swaps.
        player.position.min(player.duration) as f64 / player.duration as f64
    };
    let (filled, head, empty) = meter(meter_width, fraction);

    let fader_filled = (player.volume as usize * FADER_CELLS + 50) / 100;
    let fader_filled = fader_filled.min(FADER_CELLS);

    Line::from(vec![
        Span::raw(" ".repeat(GUTTER_WIDTH)),
        Span::styled(elapsed, Style::default().fg(ink::ASH)),
        Span::styled(filled, Style::default().fg(ink::AMBER)),
        Span::styled(head, Style::default().fg(ink::AMBER)),
        Span::styled(empty, Style::default().fg(ink::SLATE)),
        Span::raw(" "),
        Span::styled(total, Style::default().fg(ink::ASH)),
        Span::raw("  "),
        Span::styled("vol", Style::default().fg(ink::SLATE)),
        Span::raw(" "),
        Span::styled("█".repeat(fader_filled), Style::default().fg(ink::EMBER)),
        Span::styled(
            "░".repeat(FADER_CELLS - fader_filled),
            Style::default().fg(ink::SLATE),
        ),
        Span::raw(" "),
        Span::styled(volume, Style::default().fg(ink::ASH)),
    ])
}

/// Splits a meter into its filled run, its partial leading cell, and the empty
/// remainder, so the three can be styled separately.
fn meter(width: usize, fraction: f64) -> (String, String, String) {
    if width == 0 {
        return (String::new(), String::new(), String::new());
    }
    let eighths = (fraction.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    let full = (eighths / 8).min(width);
    let partial = if full < width { eighths % 8 } else { 0 };
    let head = EIGHTHS[partial];
    let empty = width - full - usize::from(partial > 0);
    ("█".repeat(full), head.to_string(), "░".repeat(empty))
}

// ------------------------------------------------------------------ status ----

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let style = match app.mode {
        Mode::ConfirmStopDaemon => Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD),
        _ if !app.online => Style::default().fg(ink::ALARM),
        _ => Style::default().fg(ink::ASH),
    };
    // Clipped at the terminal edge a status message loses its last word without
    // saying so; an ellipsis at least admits it was cut.
    let room = (area.width as usize).saturating_sub(GUTTER_WIDTH);
    frame.render_widget(
        Paragraph::new(format!("{}{}", " ".repeat(GUTTER_WIDTH), truncate(&app.status, room)))
            .style(style),
        area,
    );
}

/// Shown while typing a query. Advertising the transport keys here would be a
/// lie: they are all just characters in the search box until Esc or Enter.
const EDIT_HINTS: &[&str] = &["Enter search", "Esc cancel"];

/// Hints in descending order of usefulness, so a narrow terminal drops the
/// least important ones instead of slicing a label in half.
const KEY_HINTS: &[&str] = &[
    "/ search",
    "space play",
    "n/p track",
    "L sign in",
    "←/→ seek",
    "+/- vol",
    "q quit",
    "Q stop daemon",
];

const HINT_SEPARATOR: &str = " · ";

fn key_hints(mode: &Mode, width: usize) -> String {
    let hints: &[&str] = match mode {
        Mode::Editing => EDIT_HINTS,
        _ => KEY_HINTS,
    };
    let mut line = " ".repeat(GUTTER_WIDTH);
    for (index, hint) in hints.iter().enumerate() {
        let extra = display_width(hint)
            + if index == 0 {
                0
            } else {
                display_width(HINT_SEPARATOR)
            };
        if display_width(&line) + extra > width {
            break;
        }
        if index > 0 {
            line.push_str(HINT_SEPARATOR);
        }
        line.push_str(hint);
    }
    line
}

fn clock(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_string();
    }

    let content_width = width.saturating_sub(display_width("…"));
    let mut end = 0;
    for (index, character) in text.char_indices() {
        let next = index + character.len_utf8();
        if display_width(&text[..next]) > content_width {
            break;
        }
        end = next;
    }
    let kept = text[..end].trim_end_matches('\u{200d}');
    format!("{kept}…")
}

fn pad(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let fitted = truncate(text, width);
    let padding = width.saturating_sub(display_width(&fitted));
    format!("{fitted}{}", " ".repeat(padding))
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}
