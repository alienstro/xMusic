//! Rendering: reads [`App`] and draws it as a readout rather than a set of panels, where hairline rules and a fixed gutter carry the structure, which puts the whole weight of the layout on alignment and is why the widths and meter arithmetic below are exact.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::{Mode, Model};
use crate::panes::Pane;

/// Tungsten amber, the backlight of an analogue VU meter, is the only chromatic accent; everything else is graded by brightness so it reads under any terminal scheme.
mod ink {
    use ratatui::style::Color;

    pub const AMBER: Color = Color::Indexed(214);
    pub const EMBER: Color = Color::Indexed(179);
    pub const BONE: Color = Color::Indexed(252);
    pub const ASH: Color = Color::Indexed(245);
    pub const SLATE: Color = Color::Indexed(239);
    pub const ALARM: Color = Color::Indexed(203);
}

/// Width of the selection gutter, drawn by the list widget and matched by hand in the column header so the two line up.
const GUTTER: &str = "▌ ";
const GUTTER_WIDTH: usize = 2;

/// Cells in the volume fader.
const FADER_CELLS: usize = 8;

/// Eighth-width blocks give the meter sub-character resolution, so it advances smoothly instead of stepping a whole cell.
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Braille cells for the loading spinner: single-width, centred, and the only animation in the interface.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_FRAME_MS: u128 = 90;

/// Picks the spinner cell by elapsed time rather than frame number, so it turns at one speed however often the interface redraws.
fn spinner(elapsed: std::time::Duration) -> &'static str {
    SPINNER[(elapsed.as_millis() / SPINNER_FRAME_MS) as usize % SPINNER.len()]
}

pub fn draw(frame: &mut Frame, app: &mut Model) {
    let full = frame.area();
    if full.width < 30 || full.height < 11 {
        frame.render_widget(
            Paragraph::new("Terminal too small — needs 30x11").style(Style::default().fg(ink::ASH)),
            full,
        );
        return;
    }

    // A column of air each side, or the rules run into the terminal edge and it reads as a box after all.
    let canvas = Rect {
        x: full.x + 1,
        y: full.y,
        width: full.width - 2,
        height: full.height,
    };

    // Chrome goes from the least essential end first, and the meter goes last: an interface that cannot show where you are in the track has stopped being a music player.
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
    draw_navigation(frame, app, next());
    draw_rule(frame, next());
    let columns = Columns::for_width(canvas.width as usize, app.panes.showing_grid());
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

fn draw_header(frame: &mut Frame, app: &Model, area: Rect) {
    let (state, state_style) = if !app.online {
        ("not connected", Style::default().fg(ink::ALARM))
    } else if app.player.logged_in {
        ("signed in", Style::default().fg(ink::ASH))
    } else {
        ("signed out", Style::default().fg(ink::ASH))
    };

    let left_width = 6; // "xMusic"

    let gap = (area.width as usize).saturating_sub(left_width + display_width(state));

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("x", Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD)),
            Span::styled("Music", Style::default().fg(ink::BONE).add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(gap)),
            Span::styled(state, state_style),
        ])),
        area,
    );
}

/// One row carries all three of these because they are never wanted at once:
/// typing a query replaces the tabs, and a drill-down replaces them with the
/// trail that led into it, so there is never any question which list is on screen.
fn draw_navigation(frame: &mut Frame, app: &Model, area: Rect) {
    if matches!(app.mode, Mode::Editing) {
        draw_search(frame, app, area);
        return;
    }
    match app.panes.breadcrumb() {
        Some(trail) => frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(GUTTER_WIDTH)),
                Span::styled(
                    truncate(&trail, (area.width as usize).saturating_sub(GUTTER_WIDTH)),
                    Style::default().fg(ink::EMBER),
                ),
            ])),
            area,
        ),
        None => draw_tabs(frame, app, area),
    }
}

/// Amber is the only chromatic accent in the interface, so it is what marks the active tab; the rest are graded by brightness.
fn draw_tabs(frame: &mut Frame, app: &Model, area: Rect) {
    let mut spans = vec![Span::raw(" ".repeat(GUTTER_WIDTH))];
    for (index, pane) in Pane::ALL.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(ink::SLATE)));
        }
        let style = if *pane == app.panes.active {
            Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ink::ASH)
        };
        spans.push(Span::styled(pane.title(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_search(frame: &mut Frame, app: &Model, area: Rect) {
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
        // The terminal's own cursor rather than a drawn stand-in, so it blinks the way the user's terminal blinks.
        let input_width = display_width(&app.input).min(u16::MAX as usize) as u16;
        let x = area.x.saturating_add(2).saturating_add(input_width);
        frame.set_cursor_position((x.min(area.x + area.width.saturating_sub(1)), area.y));
    }
}

// ----------------------------------------------------------------- results ----

/// Column widths for the list: album goes first on a narrow terminal, then
/// artist, while title and running time always survive. A grid pane has neither
/// a duration nor a like, so its rows are a name and one line about it.
struct Columns {
    /// Zero on a grid pane, where a playlist has no like state to show.
    heart: usize,
    title: usize,
    artist: usize,
    album: usize,
    time: usize,
    grid: bool,
}

impl Columns {
    const GAP: usize = 2;
    const TIME: usize = 5;
    /// The glyph and the space after it, matched by hand in the column header.
    const HEART: usize = 2;

    fn for_width(width: usize, grid: bool) -> Self {
        let heart = if grid { 0 } else { Self::HEART };
        let available = width.saturating_sub(GUTTER_WIDTH + heart);
        let time = if grid { 0 } else { Self::TIME };

        let (artist, album) = match (grid, available) {
            // One long subtitle rather than two columns: "Owner • 12 tracks".
            (true, available) if available >= 58 => (30, 0),
            (true, available) if available >= 40 => (22, 0),
            (true, _) => (0, 0),
            (false, available) if available >= 78 => (22, 22),
            (false, available) if available >= 58 => (20, 0),
            (false, available) if available >= 40 => (16, 0),
            (false, _) => (0, 0),
        };

        let occupied = artist
            + album
            + time
            + Self::GAP * [artist, album, time].iter().filter(|w| **w > 0).count();

        Self {
            heart,
            title: available.saturating_sub(occupied).max(6),
            artist,
            album,
            time,
            grid,
        }
    }
}

fn draw_column_header(frame: &mut Frame, area: Rect, columns: &Columns) {
    let mut line = " ".repeat(GUTTER_WIDTH + columns.heart);
    line.push_str(&pad(if columns.grid { "NAME" } else { "TITLE" }, columns.title));
    if columns.artist > 0 {
        line.push_str(&" ".repeat(Columns::GAP));
        line.push_str(&pad(
            if columns.grid { "DETAIL" } else { "ARTIST" },
            columns.artist,
        ));
    }
    if columns.album > 0 {
        line.push_str(&" ".repeat(Columns::GAP));
        line.push_str(&pad("ALBUM", columns.album));
    }
    if columns.time > 0 {
        line.push_str(&" ".repeat(Columns::GAP));
        line.push_str(&format!("{:>width$}", "TIME", width = columns.time));
    }

    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(ink::SLATE)),
        area,
    );
}

/// A filled heart for liked, a hollow one for known-not-liked, and nothing at
/// all where the response never said — which is every search result, and is not
/// the same as not liked.
fn heart(liked: Option<bool>) -> Span<'static> {
    match liked {
        Some(true) => Span::styled("♥ ", Style::default().fg(ink::AMBER)),
        Some(false) => Span::styled("♡ ", Style::default().fg(ink::SLATE)),
        None => Span::raw("  "),
    }
}

fn draw_results(frame: &mut Frame, app: &mut Model, area: Rect, columns: &Columns) {
    if app.panes.visible().rows.is_empty() {
        let (message, style) = if app.loading_list {
            (
                format!("{} Loading", spinner(app.started.elapsed())),
                Style::default().fg(ink::EMBER),
            )
        } else {
            (empty_hint(app), Style::default().fg(ink::SLATE))
        };
        frame.render_widget(
            Paragraph::new(format!("{}{message}", " ".repeat(GUTTER_WIDTH))).style(style),
            area,
        );
        return;
    }

    let playing_id = app.player.video_id.clone();
    let items: Vec<ListItem> = app
        .panes
        .visible()
        .rows
        .iter()
        .map(|result| {
            // Colour carries what is playing and the gutter carries the selection, so neither needs a glyph.
            let playing = !result.video_id.is_empty() && result.video_id == playing_id;
            let title_style = if playing {
                Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ink::BONE)
            };

            let mut spans = Vec::new();
            if columns.heart > 0 {
                spans.push(heart(result.liked));
            }
            spans.push(Span::styled(pad(&result.title, columns.title), title_style));
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
            if columns.time > 0 {
                spans.push(Span::raw(" ".repeat(Columns::GAP)));
                spans.push(Span::styled(
                    format!("{:>width$}", result.duration, width = columns.time),
                    Style::default().fg(ink::SLATE),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol(GUTTER)
            .highlight_style(Style::default().fg(ink::AMBER)),
        area,
        &mut app.panes.visible_mut().cursor,
    );
}

/// What an empty list says for itself, which differs by why it is empty.
fn empty_hint(app: &Model) -> String {
    if app.panes.breadcrumb().is_some() {
        return "Nothing in this list".into();
    }
    match app.panes.active {
        Pane::Search => "Search for a song to start playing".into(),
        pane => format!("Press r to load {}", pane.title()),
    }
}

// ------------------------------------------------------------- now playing ----

fn draw_now_playing(frame: &mut Frame, app: &Model, area: Rect, show_subtitle: bool) {
    let player = &app.player;
    let indent = " ".repeat(GUTTER_WIDTH);

    let (marker, marker_style) = if !app.online {
        ("■", Style::default().fg(ink::ALARM))
    } else if app.is_loading() {
        (spinner(app.started.elapsed()), Style::default().fg(ink::EMBER))
    } else if player.is_playing {
        ("▶", Style::default().fg(ink::AMBER))
    } else {
        ("▶", Style::default().fg(ink::SLATE))
    };

    let headline = if !app.online {
        Span::styled("Daemon not running", Style::default().fg(ink::ALARM))
    } else if player.title.is_empty() {
        Span::styled(
            match () {
                _ if player.hibernating => "Idle - page unloaded",
                _ if player.ready => "Nothing playing",
                _ => "Loading YouTube Music",
            },
            Style::default().fg(ink::SLATE),
        )
    } else if let Some(title) = app.loading_title() {
        Span::styled(
            title.to_string(),
            Style::default().fg(ink::EMBER).add_modifier(Modifier::BOLD),
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

    // The heart sits after the title rather than before it, because here the
    // marker column already belongs to the transport state.
    let mut headline_spans = vec![
        Span::styled(marker, marker_style),
        Span::raw(" ".repeat(GUTTER_WIDTH - 1)),
        headline,
    ];
    if !player.title.is_empty() && player.liked.is_some() {
        headline_spans.push(Span::raw("   "));
        headline_spans.push(heart(player.liked));
    }
    let mut lines = vec![Line::from(headline_spans)];
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

/// The line the interface is built around: elapsed timecode, meter, running time, fader.
fn transport(app: &Model, width: usize) -> Line<'static> {
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

/// Splits a meter into filled run, partial leading cell and empty remainder, so the three can be styled separately.
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

fn draw_status(frame: &mut Frame, app: &Model, area: Rect) {
    let style = match app.mode {
        Mode::ConfirmStopDaemon => Style::default().fg(ink::AMBER).add_modifier(Modifier::BOLD),
        _ if !app.online => Style::default().fg(ink::ALARM),
        _ => Style::default().fg(ink::ASH),
    };
    // Clipped at the terminal edge a message loses its last word silently; an ellipsis at least admits the cut.
    let room = (area.width as usize).saturating_sub(GUTTER_WIDTH);
    frame.render_widget(
        Paragraph::new(format!("{}{}", " ".repeat(GUTTER_WIDTH), truncate(&app.status, room)))
            .style(style),
        area,
    );
}

/// Shown while typing a query, where advertising the transport keys would be a lie: they are characters in the search box until Esc or Enter.
const EDIT_HINTS: &[&str] = &["Enter search", "Esc cancel"];

/// Hints in descending order of usefulness, so a narrow terminal drops the least important instead of slicing a label.
const KEY_HINTS: &[&str] = &[
    "1-5 pane",
    "/ search",
    "f like",
    "space play",
    "n/p track",
    "Esc back",
    "r reload",
    "L sign in",
    "←/→ seek",
    "+/- vol",
    "q quit",
    "W window",
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
