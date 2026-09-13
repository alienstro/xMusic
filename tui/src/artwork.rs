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
use ratatui::layout::Rect;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

/// Largest thumbnail worth reading, so a wrong or hostile URL cannot pull an
/// unbounded response into memory. A YouTube Music cover is far under this.
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// How long to wait on a cover before giving up. Artwork is cosmetic, so a slow
/// CDN should drop the image, never the interface.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The cover's shape: 16 by 9, which is the shape a YouTube thumbnail arrives
/// in. Drawing it at that shape shows the whole frame and crops nothing away,
/// where a square box would discard nearly half of it.
const ASPECT_WIDTH: u16 = 16;
const ASPECT_HEIGHT: u16 = 9;

/// Rows the now-playing card grows to when it has room to show a cover.
///
/// Fourteen rows almost doubles the half-block resolution of the old box.
/// A cell is one pixel wide and two tall, so a 16:9 cover at fourteen rows is
/// 49 cells wide. The source is scaled to that larger pixel target.
const ART_ROWS: u16 = 14;

/// Rows the card takes when it shows no cover, matching the height the
/// interface used before artwork existed.
const PLAIN_ROWS: u16 = 3;
const PLAIN_ROWS_NO_SUBTITLE: u16 = 2;

/// Terminal height below which the cover is not worth its rows, and is dropped.
const ART_MIN_CANVAS_HEIGHT: u16 = 25;

/// Cells of width of the cover. The floor keeps a short card from drawing a
/// sliver, and the ceiling keeps the cover from crowding out the title beside it.
const MIN_ART_WIDTH: u16 = 8;
const MAX_ART_WIDTH: u16 = 49;

/// Columns the title beside the cover needs before the cover is worth showing.
/// Below this the two would fight over the same cells, and the title is the one
/// that has to stay readable.
const ART_MIN_TEXT_WIDTH: u16 = 24;

/// Whether this terminal and this window have room to show a cover.
pub fn shows_art(art_available: bool, canvas_width: u16, canvas_height: u16) -> bool {
    if !art_available || canvas_height < ART_MIN_CANVAS_HEIGHT {
        return false;
    }
    // The cover's own column, its gap, and the least room the title may keep.
    canvas_width >= art_width(ART_ROWS) + 2 + ART_MIN_TEXT_WIDTH
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

/// Width in cells that draws the cover at its own 16:9 in a box this many rows
/// tall. A cell is two pixels tall, so the width is twice what a plain ratio
/// would give.
pub fn art_width(band_rows: u16) -> u16 {
    let wide = band_rows.saturating_mul(2).saturating_mul(ASPECT_WIDTH) / ASPECT_HEIGHT;
    wide.clamp(MIN_ART_WIDTH, MAX_ART_WIDTH)
}

/// Crops a cover to the centred rectangle of the given shape, so a source of
/// another shape fills the box instead of being squashed into it. The centre is
/// kept, which is where the artwork is.
pub fn crop_to_aspect(image: DynamicImage, aspect_width: u16, aspect_height: u16) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    let (aspect_width, aspect_height) = (aspect_width as u32, aspect_height as u32);
    // The largest rectangle of the wanted shape that fits inside the source.
    let (crop_width, crop_height) = if width * aspect_height > height * aspect_width {
        (height * aspect_width / aspect_height, height)
    } else {
        (width, width * aspect_height / aspect_width)
    };
    image.crop_imm(
        (width - crop_width) / 2,
        (height - crop_height) / 2,
        crop_width,
        crop_height,
    )
}

/// The pixels ratatui-image will ask for in an area of this size, at this
/// picker's font size. Handing it a source already at these dimensions is what
/// keeps the image sharp: the crate resamples internally with a nearest
/// filter, which mangles a cover at the small sizes a terminal draws.
pub fn target_pixels(font_size: (u16, u16), area: Rect) -> (u32, u32) {
    (
        area.width as u32 * font_size.0 as u32,
        area.height as u32 * font_size.1 as u32,
    )
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
    /// The URL the card should be showing, which is the player's current one.
    wanted: String,
    /// The built cover, with the URL and the area it was built for. Rebuilt when
    /// either changes, because the pixels depend on both.
    shown: Option<(String, Rect, StatefulProtocol)>,
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
        self.wanted = url.to_string();
        let have = self.cache.contains_key(url);
        self.plan.sync(url, have)
    }

    /// Files a decoded cover. It is drawn on the next frame if it is still the
    /// track the player is on.
    pub fn deliver(&mut self, url: &str, image: DynamicImage) {
        self.cache.insert(url.to_string(), image);
        self.plan.accept(url);
    }

    /// The cover to draw in this area, building it if it is not already built
    /// for that area. `None` when there is no cover, or the terminal cannot
    /// draw one, which is what leaves the card as plain text.
    ///
    /// The image is cropped to the area's shape and scaled to exactly the
    /// pixels the widget will ask for. Both matter: the crop stops a source of
    /// another shape from being squashed, and the pre-scale stops the crate's
    /// own nearest-neighbour resample from mangling a small cover.
    pub fn protocol(&mut self, area: Rect) -> Option<&mut StatefulProtocol> {
        let picker = self.picker.as_ref()?;
        let image = self.cache.get(&self.wanted)?;

        let stale = match &self.shown {
            Some((url, built, _)) => url != &self.wanted || *built != area,
            None => true,
        };
        if stale {
            let (width, height) = target_pixels(picker.font_size(), area);
            let prepared = crop_to_aspect(image.clone(), ASPECT_WIDTH, ASPECT_HEIGHT)
                .resize_exact(width, height, image::imageops::FilterType::Lanczos3);
            let protocol = picker.new_resize_protocol(prepared);
            self.shown = Some((self.wanted.clone(), area, protocol));
        }
        self.shown.as_mut().map(|(_, _, protocol)| protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_is_shown_only_on_an_image_terminal_with_the_room_for_it() {
        // 49-wide cover + 2 gap + 24 of title = 75 columns is the threshold.
        assert!(shows_art(true, 80, 25));
        assert!(shows_art(true, 75, 25));
        assert!(!shows_art(true, 74, 25));
        assert!(!shows_art(true, 80, 24));
        assert!(!shows_art(false, 200, 50));
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
    fn the_cover_draws_at_sixteen_by_nine() {
        // Fourteen rows is two pixels per row, so a 16:9 cover is 49 cells wide.
        assert_eq!(art_width(14), 49);
    }

    #[test]
    fn fourteen_rows_asks_for_the_larger_pixel_target() {
        // The larger box draws with more cells than the source thumbnail.
        let target = target_pixels((10, 20), Rect::new(0, 0, 49, 14));
        assert_eq!(target, (490, 280));
    }

    #[test]
    fn cover_width_is_held_between_a_floor_and_a_ceiling() {
        assert_eq!(art_width(2), MIN_ART_WIDTH);
        assert_eq!(art_width(30), MAX_ART_WIDTH);
    }

    #[test]
    fn a_wide_cover_is_cropped_to_the_shape_asked_for() {
        // 320x180 into 16:9 is already the right shape, so nothing is removed.
        let cropped = crop_to_aspect(image::DynamicImage::new_rgb8(320, 180), 16, 9);
        assert_eq!((cropped.width(), cropped.height()), (320, 180));
    }

    #[test]
    fn a_square_cover_is_cropped_to_sixteen_by_nine() {
        // 180x180 into 16:9 takes the full width and the middle 101 rows.
        let cropped = crop_to_aspect(image::DynamicImage::new_rgb8(180, 180), 16, 9);
        assert_eq!((cropped.width(), cropped.height()), (180, 101));
    }

    #[test]
    fn a_tall_cover_is_cropped_to_sixteen_by_nine() {
        // 180x360 into 16:9 takes the full width and the middle 101 rows.
        let cropped = crop_to_aspect(image::DynamicImage::new_rgb8(180, 360), 16, 9);
        assert_eq!((cropped.width(), cropped.height()), (180, 101));
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
