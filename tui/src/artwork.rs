//! Album art for the now-playing card.
//!
//! The thumbnail arrives as a URL on [`PlayerState`], and the interface draws
//! it through ratatui-image, which falls back to half-blocks on any terminal
//! that speaks no image protocol. The fetch and the decode run on the effect
//! runner; only the resize and the encode happen at render time, because a
//! half-block cover is small and the alternative stalls the render loop.
//!
//! The fetch policy is a plain struct with no terminal in it, so the part that
//! is easy to get wrong — fetch once per track, and drop an answer that arrives
//! after the track has moved on — is testable on its own.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use image::DynamicImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

/// Largest thumbnail worth reading, so a wrong or hostile URL cannot pull an
/// unbounded response into memory. A YouTube Music cover is far under this.
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// How long to wait on a cover before giving up. Artwork is cosmetic, so a slow
/// CDN should drop the image, never the interface.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Rows the now-playing card grows to when it has room to show a cover. A cell
/// is about twice as tall as it is wide, so eight rows draw a sixteen-pixel
/// square, which is the smallest cover that still reads as one.
const ART_ROWS: u16 = 8;

/// Rows the card takes when it shows no cover, matching the height the
/// interface used before artwork existed.
const PLAIN_ROWS: u16 = 3;
const PLAIN_ROWS_NO_SUBTITLE: u16 = 2;

/// Terminal height below which the cover is not worth its rows, and is dropped.
const ART_MIN_CANVAS_HEIGHT: u16 = 24;

/// Cells of width of a square cover. Two cells per row of height keeps the
/// image square; the floor keeps a short card from drawing a sliver, and the
/// ceiling keeps the cover from crowding out the title beside it.
const MIN_ART_WIDTH: u16 = 8;
const MAX_ART_WIDTH: u16 = 20;

/// Whether this terminal and this window have room to show a cover.
pub fn shows_art(art_available: bool, canvas_height: u16) -> bool {
    art_available && canvas_height >= ART_MIN_CANVAS_HEIGHT
}

/// Height in rows of the now-playing card, cover included.
pub fn band_height(shows_art: bool, show_subtitle: bool) -> u16 {
    if shows_art {
        ART_ROWS
    } else if show_subtitle {
        PLAIN_ROWS
    } else {
        PLAIN_ROWS_NO_SUBTITLE
    }
}

/// Width in cells of a square cover drawn in a card this many rows tall.
pub fn art_width(band_height: u16) -> u16 {
    (band_height * 2).clamp(MIN_ART_WIDTH, MAX_ART_WIDTH)
}

/// Crops a cover to the centred square the card reserves, so a wide thumbnail
/// fills the box instead of being letterboxed inside it. A square covers the
/// centre, which is where the artwork is; the edges a crop removes are the
/// least of the image.
pub fn square(image: DynamicImage) -> DynamicImage {
    let side = image.width().min(image.height());
    let x = (image.width() - side) / 2;
    let y = (image.height() - side) / 2;
    image.crop_imm(x, y, side, side)
}

/// When to fetch a cover, and what to do with one that arrives.
///
/// Holds no decoded image: the caller keeps those, and asks this only whether
/// the bytes are needed and whether an arrival is still the current track.
#[derive(Default, Debug, PartialEq)]
pub struct Plan {
    /// The URL last seen on the player, which is the track the card is showing.
    want: String,
    /// A URL whose bytes are on their way, so one track asks only once.
    in_flight: String,
}

impl Plan {
    /// Records the now-playing URL, and reports whether its bytes are needed.
    ///
    /// `have` is whether the caller already holds a decoded cover for this URL.
    pub fn sync(&mut self, url: &str, have: bool) -> bool {
        if url != self.want {
            self.want = url.to_string();
            self.in_flight.clear();
        }
        if url.is_empty() || have || url == self.in_flight {
            return false;
        }
        self.in_flight = url.to_string();
        true
    }

    /// Files an arrival, and reports whether it is still the wanted cover.
    ///
    /// False when the track changed while the bytes were in flight, so the
    /// caller discards them rather than drawing a stale cover.
    pub fn accept(&mut self, url: &str) -> bool {
        if url.is_empty() || url != self.want {
            return false;
        }
        self.in_flight.clear();
        true
    }
}

/// Fetches a cover and decodes it, on the effect runner. Returns `None` on any
/// failure: artwork is never worth a status line or a retry.
pub fn fetch(url: &str) -> Option<DynamicImage> {
    let agent = ureq::AgentBuilder::new().timeout(FETCH_TIMEOUT).build();
    let response = agent.get(url).call().ok()?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_IMAGE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    image::load_from_memory(&bytes).ok()
}

/// The terminal image backend, and the covers drawn so far.
///
/// `query` must run before the event loop starts: it writes a query to the
/// terminal and reads the answer from stdin, and would otherwise eat a key.
#[derive(Default)]
pub struct Artwork {
    picker: Option<Picker>,
    /// Decoded covers by URL, kept so a track played again draws with no fetch.
    cache: HashMap<String, DynamicImage>,
    /// The cover being drawn, and the URL it belongs to.
    shown: Option<(String, StatefulProtocol)>,
    plan: Plan,
}

impl Artwork {
    /// Watches the terminal for an image protocol. Falls back to half-blocks on
    /// any failure, which is every terminal without one.
    ///
    /// The crate allows two seconds for the terminal to answer its query, and
    /// that timeout is not configurable in this version. Any terminal that
    /// answers at all answers in well under a millisecond, so the two seconds
    /// are paid once at startup only where the terminal stays silent — a
    /// pseudo-terminal, a pipe, a serial console.
    pub fn query() -> Self {
        Self {
            picker: Picker::from_query_stdio().ok(),
            ..Self::default()
        }
    }

    /// Whether this terminal can draw a cover at all.
    pub fn available(&self) -> bool {
        self.picker.is_some()
    }

    /// Records the now-playing cover, and reports whether its bytes are needed.
    pub fn want(&mut self, url: &str) -> bool {
        let have = self.cache.contains_key(url);
        let needed = self.plan.sync(url, have);
        if !needed {
            self.ensure_shown(url);
        }
        needed
    }

    /// Files a decoded cover and draws it if it is still the current track.
    pub fn deliver(&mut self, url: &str, image: DynamicImage) {
        self.cache.insert(url.to_string(), image);
        if self.plan.accept(url) {
            self.ensure_shown(url);
        }
    }

    /// Points the drawn cover at a URL, building it from the cache if it is
    /// there and clearing it when it is not, so a track with no art shows none.
    fn ensure_shown(&mut self, url: &str) {
        if let Some((shown, _)) = &self.shown {
            if shown == url {
                return;
            }
        }
        self.shown = None;
        let (Some(picker), Some(image)) = (self.picker.as_ref(), self.cache.get(url)) else {
            return;
        };
        let protocol = picker.new_resize_protocol(square(image.clone()));
        self.shown = Some((url.to_string(), protocol));
    }

    /// The cover to draw, as state that `StatefulImage` renders.
    pub fn protocol(&mut self) -> Option<&mut StatefulProtocol> {
        self.shown.as_mut().map(|(_, protocol)| protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_is_shown_only_on_an_image_terminal_with_the_height_for_it() {
        assert!(shows_art(true, 24));
        assert!(shows_art(true, 40));
        assert!(!shows_art(true, 23));
        assert!(!shows_art(false, 40));
    }

    #[test]
    fn card_grows_for_a_cover_only_when_one_is_shown() {
        assert_eq!(band_height(true, true), ART_ROWS);
        assert_eq!(band_height(false, true), PLAIN_ROWS);
    }

    #[test]
    fn card_keeps_its_plain_height_without_a_subtitle_row() {
        assert_eq!(band_height(false, false), PLAIN_ROWS_NO_SUBTITLE);
    }

    #[test]
    fn cover_is_square_across_two_cells_per_row() {
        assert_eq!(art_width(8), 16);
    }

    #[test]
    fn cover_width_is_held_between_a_floor_and_a_ceiling() {
        assert_eq!(art_width(2), MIN_ART_WIDTH);
        assert_eq!(art_width(12), MAX_ART_WIDTH);
    }

    #[test]
    fn a_wide_cover_is_cropped_to_a_centred_square() {
        // 320x180: a square crop takes the full height and centres the width.
        let cropped = square(image::DynamicImage::new_rgb8(320, 180));
        assert_eq!((cropped.width(), cropped.height()), (180, 180));
    }

    #[test]
    fn a_tall_cover_is_cropped_to_a_centred_square() {
        // 180x320: a square crop takes the full width and centres the height.
        let cropped = square(image::DynamicImage::new_rgb8(180, 320));
        assert_eq!((cropped.width(), cropped.height()), (180, 180));
    }

    #[test]
    fn a_square_cover_is_left_alone() {
        let cropped = square(image::DynamicImage::new_rgb8(200, 200));
        assert_eq!((cropped.width(), cropped.height()), (200, 200));
    }

    #[test]
    fn one_track_asks_for_its_cover_once() {
        let mut plan = Plan::default();
        assert!(plan.sync("https://a", false));
        assert!(!plan.sync("https://a", false));
    }

    #[test]
    fn a_cached_cover_is_not_fetched_again() {
        let mut plan = Plan::default();
        assert!(!plan.sync("https://a", true));
    }

    #[test]
    fn an_empty_url_is_never_fetched() {
        let mut plan = Plan::default();
        assert!(!plan.sync("", false));
    }

    #[test]
    fn a_cover_that_arrives_after_the_track_changed_is_dropped() {
        let mut plan = Plan::default();
        assert!(plan.sync("https://a", false));
        assert!(plan.sync("https://b", false));
        assert!(!plan.accept("https://a"));
        assert!(plan.accept("https://b"));
    }

    #[test]
    fn an_unrequested_arrival_is_dropped() {
        let mut plan = Plan::default();
        assert!(!plan.accept("https://a"));
    }
}
