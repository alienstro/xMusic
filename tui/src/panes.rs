//! Which list is on screen, what each pane last loaded, and where the cursor sits in it.
//!
//! Part of the model, kept apart from the rest of it because per-pane list state
//! is exactly the kind of thing that would double the size of `model.rs`, and
//! none of it needs to know about keys, HTTP or drawing.

use ratatui::widgets::ListState as Cursor;

use xmusic_protocol::{Feed, ListItem, ListState, Source};

/// The tabs across the top, in the order they are drawn and the order `1`-`5` select them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Search,
    Liked,
    Playlists,
    Albums,
    History,
}

impl Pane {
    pub const ALL: [Pane; 5] = [
        Pane::Search,
        Pane::Liked,
        Pane::Playlists,
        Pane::Albums,
        Pane::History,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Search => "Search",
            Pane::Liked => "Liked",
            Pane::Playlists => "Playlists",
            Pane::Albums => "Albums",
            Pane::History => "History",
        }
    }

    /// The feed the daemon knows this pane by, or `None` for search, which is asked for by query rather than by name.
    pub fn feed(self) -> Option<Feed> {
        match self {
            Pane::Search => None,
            Pane::Liked => Some(Feed::Liked),
            Pane::Playlists => Some(Feed::Playlists),
            Pane::Albums => Some(Feed::Albums),
            Pane::History => Some(Feed::History),
        }
    }

    /// Whether rows here are playlists and albums to open rather than songs to play. Decides both the columns drawn and what Enter means.
    pub fn is_grid(self) -> bool {
        self.feed().is_some_and(Feed::is_grid)
    }

    fn index(self) -> usize {
        Pane::ALL
            .iter()
            .position(|pane| *pane == self)
            .expect("every pane is in ALL")
    }
}

/// One list and the cursor in it. Each pane keeps its own, so moving between tabs does not refetch and does not lose the user's place.
#[derive(Default)]
pub struct Slot {
    pub rows: Vec<ListItem>,
    pub cursor: Cursor,
    /// What to call this list on screen: the query for a search, the playlist's name for a drill-down.
    pub label: String,
    /// The daemon stopped at its page cap, so this is part of the feed rather than all of it.
    pub truncated: bool,
    /// Whether this pane has ever been asked for. A pane loads on first visit and on an explicit reload, not on every switch.
    pub visited: bool,
    /// Sequence number of the list shown here, so a reply that has been overtaken cannot overwrite a newer one.
    pub seq: u64,
}

impl Slot {
    pub fn selected(&self) -> Option<&ListItem> {
        self.cursor.selected().and_then(|index| self.rows.get(index))
    }

    /// Every track in this list, in the order it is drawn: what plays on from
    /// the row the user chose. Rows that are lists to open rather than tracks
    /// carry no id and are not part of it.
    pub fn queue(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| !row.video_id.is_empty())
            .map(|row| row.video_id.clone())
            .collect()
    }

    fn fill(&mut self, list: &ListState) {
        self.rows = list.items.clone();
        self.label = list.label.clone();
        self.truncated = list.truncated;
        self.seq = list.seq;
        self.cursor.select((!self.rows.is_empty()).then_some(0));
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        let current = self.cursor.selected().unwrap_or(0) as isize;
        self.cursor.select(Some((current + delta).clamp(0, last) as usize));
    }
}

/// A playlist or album opened from a grid row, remembered so leaving it puts the grid back exactly as it was.
pub struct Drill {
    /// The id the daemon was asked for, which is also how its reply is recognised.
    pub browse_id: String,
    /// The name from the row that opened it, which is what the breadcrumb says.
    pub title: String,
    pub slot: Slot,
}

pub struct Panes {
    pub active: Pane,
    slots: [Slot; 5],
    /// Drill-downs, innermost last. Empty means a pane is on screen.
    stack: Vec<Drill>,
}

impl Default for Panes {
    fn default() -> Self {
        Self {
            active: Pane::Search,
            slots: Default::default(),
            stack: Vec::new(),
        }
    }
}

impl Panes {
    /// The list on screen: the innermost drill-down, or the active pane.
    pub fn visible(&self) -> &Slot {
        match self.stack.last() {
            Some(drill) => &drill.slot,
            None => &self.slots[self.active.index()],
        }
    }

    pub fn visible_mut(&mut self) -> &mut Slot {
        match self.stack.last_mut() {
            Some(drill) => &mut drill.slot,
            None => &mut self.slots[self.active.index()],
        }
    }

    /// Whether rows on screen are cards to open rather than songs to play. A drill-down is always songs, whichever pane it was opened from.
    pub fn showing_grid(&self) -> bool {
        self.stack.is_empty() && self.active.is_grid()
    }

    /// `Playlists › Ambient`, or nothing when a pane rather than a drill-down is on screen.
    pub fn breadcrumb(&self) -> Option<String> {
        if self.stack.is_empty() {
            return None;
        }
        let mut trail = vec![self.active.title().to_string()];
        trail.extend(self.stack.iter().map(|drill| drill.title.clone()));
        Some(trail.join(" › "))
    }

    /// What to call the list on screen: the innermost drill-down's name, or the pane's.
    pub fn visible_title(&self) -> &str {
        match self.stack.last() {
            Some(drill) => &drill.title,
            None => self.active.title(),
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        self.visible_mut().move_cursor(delta);
    }

    /// Switches panes, leaving any drill-down. Answers whether the new pane still needs loading.
    pub fn switch(&mut self, pane: Pane) -> bool {
        self.stack.clear();
        self.active = pane;
        !self.slots[pane.index()].visited
    }

    /// Steps `delta` tabs along, wrapping, for Tab and Shift-Tab.
    pub fn step(&mut self, delta: isize) -> bool {
        let count = Pane::ALL.len() as isize;
        let next = (self.active.index() as isize + delta).rem_euclid(count);
        self.switch(Pane::ALL[next as usize])
    }

    /// Marks the visible list as asked for, so it is not asked for again on the next visit.
    pub fn mark_visited(&mut self) {
        self.visible_mut().visited = true;
    }

    /// Opens a playlist or album on top of the current pane.
    pub fn drill(&mut self, browse_id: String, title: String) {
        self.stack.push(Drill {
            browse_id,
            title,
            slot: Slot {
                visited: true,
                ..Slot::default()
            },
        });
    }

    /// Leaves the innermost drill-down. Answers whether there was one to leave.
    pub fn leave(&mut self) -> bool {
        self.stack.pop().is_some()
    }

    /// The id of the list on screen, for reloading it: `None` where it is a pane rather than a drill-down.
    pub fn drilled_id(&self) -> Option<&str> {
        self.stack.last().map(|drill| drill.browse_id.as_str())
    }

    /// Paints a like everywhere that track shows: any pane that has loaded it, and any drill-down.
    pub fn paint_like(&mut self, video_id: &str, liked: Option<bool>) {
        let slots = self
            .slots
            .iter_mut()
            .chain(self.stack.iter_mut().map(|drill| &mut drill.slot));
        for slot in slots {
            for row in slot.rows.iter_mut().filter(|row| row.video_id == video_id) {
                row.liked = liked;
            }
        }
    }

    /// Files a list the daemon reported by what produced it, rather than by what
    /// is on screen: a reply that arrives after the user has moved on belongs to
    /// the pane that asked for it, and is worth keeping there. A reply overtaken
    /// by a newer one for the same slot is dropped by sequence number.
    ///
    /// Answers whether the list landed where the user is looking.
    pub fn accept(&mut self, list: &ListState) -> bool {
        let visible = self.visible_key();
        let Some(key) = self.key_of(list) else {
            return false;
        };
        let slot = match key {
            Key::Pane(pane) => &mut self.slots[pane.index()],
            Key::Drill(depth) => &mut self.stack[depth].slot,
        };
        // The daemon holds one list slot and serves it on every poll, so a reply
        // is new only when its sequence number is. Refilling on every poll would
        // reset the cursor five times a second and undo an optimistic like.
        if list.seq <= slot.seq {
            return false;
        }
        slot.fill(list);
        visible == Some(key)
    }

    /// Whether a list still loading is the one the user is waiting on, as opposed to a pane they have left.
    pub fn awaiting(&self, list: &ListState) -> bool {
        self.key_of(list) == self.visible_key() && self.key_of(list).is_some()
    }

    fn visible_key(&self) -> Option<Key> {
        match self.stack.is_empty() {
            true => Some(Key::Pane(self.active)),
            false => Some(Key::Drill(self.stack.len() - 1)),
        }
    }

    /// Which slot a reported list belongs to, read from what the daemon says produced it.
    fn key_of(&self, list: &ListState) -> Option<Key> {
        match &list.source {
            Source::Search => Some(Key::Pane(Pane::Search)),
            Source::Feed(feed) => Pane::ALL
                .iter()
                .find(|pane| pane.feed() == Some(*feed))
                .copied()
                .map(Key::Pane),
            // A drill-down reply names the id it was asked for, so the right
            // level of the stack is the one that asked for it.
            Source::Playlist(browse_id) => self
                .stack
                .iter()
                .rposition(|drill| drill.browse_id == *browse_id)
                .map(Key::Drill),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Pane(Pane),
    Drill(usize),
}
