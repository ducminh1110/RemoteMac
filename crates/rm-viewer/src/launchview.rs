//! The launcher's model and layout, apart from drawing (portable, tested): which apps show for
//! the search, where each tile is in the grid, what is under the pointer, where the keyboard
//! selection goes. The window itself (launcher.rs) draws this after MobileLab's Xcode 26 look:
//! a tinted window, a floating rounded panel, a large title, a search field and a grid of icons.

/// One app in the launcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub id: String,
    pub name: String,
    /// installed on the Mac (an app the Mac lists but does not have is shown, dimmed)
    pub available: bool,
    /// a window of it is open on this PC
    pub open: bool,
}

/// Grid geometry (DIPs; the window scales them).
pub const CELL_W: f32 = 108.0;
pub const CELL_H: f32 = 112.0;
pub const ICON: f32 = 64.0;
/// panel margin in the window, panel corner radius, the toolbar above the panel
pub const GAP: f32 = 8.0;
pub const RADIUS: f32 = 14.0;
pub const TOOLBAR: f32 = 52.0;
/// inside the panel: side padding, the header (title, search), the footer strip
pub const PAD: f32 = 28.0;
pub const HEADER: f32 = 96.0;
pub const FOOTER: f32 = 30.0;

/// The tiles shown for `query`: every word of it in the name (or id), any case, in the order
/// the Mac lists them (the Mac Desktop first). An empty query shows everything.
pub fn filter(tiles: &[Tile], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let hay = format!("{} {}", t.name.to_lowercase(), t.id.to_lowercase());
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Where the grid's tiles go: as many columns as fit the width, centred.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// left edge of the first column, top of the first row (before scrolling)
    pub x: f32,
    pub y: f32,
    pub cell_w: f32,
    pub cell_h: f32,
}

impl Grid {
    /// `n` tiles in an area starting at (`x`, `y`), `w` wide, at `scale` pixels per DIP.
    pub fn new(n: usize, x: f32, y: f32, w: f32, scale: f32) -> Grid {
        let (cw, ch) = (CELL_W * scale, CELL_H * scale);
        let cols = ((w / cw).floor() as usize).max(1);
        let rows = n.div_ceil(cols);
        // the columns centred in the width (a short last row starts at the left, as rows do)
        let x0 = (x + (w - cols as f32 * cw) / 2.0).max(x);
        Grid { cols, rows, x: x0, y, cell_w: cw, cell_h: ch }
    }

    /// The whole grid's height.
    pub fn height(&self) -> f32 {
        self.rows as f32 * self.cell_h
    }

    /// Tile `k` (position in the shown list): (x, y, w, h), `scroll` pixels up.
    pub fn cell(&self, k: usize, scroll: f32) -> (f32, f32, f32, f32) {
        let (c, r) = (k % self.cols, k / self.cols);
        (self.x + c as f32 * self.cell_w, self.y + r as f32 * self.cell_h - scroll, self.cell_w, self.cell_h)
    }

    /// The shown tile under (`x`, `y`) (window pixels), if any, of `n`.
    pub fn at(&self, n: usize, x: f32, y: f32, scroll: f32) -> Option<usize> {
        if x < self.x || y < self.y - scroll {
            return None;
        }
        let (c, r) = (((x - self.x) / self.cell_w) as usize, ((y - self.y + scroll) / self.cell_h) as usize);
        let k = r * self.cols + c;
        (c < self.cols && k < n).then_some(k)
    }

    /// How far it can scroll for a view `view_h` high.
    pub fn max_scroll(&self, view_h: f32) -> f32 {
        (self.height() - view_h).max(0.0)
    }

    /// The scroll that shows tile `k` whole in a view `view_h` high, from `scroll`.
    pub fn reveal(&self, k: usize, scroll: f32, view_h: f32) -> f32 {
        let top = (k / self.cols) as f32 * self.cell_h;
        if top < scroll {
            top
        } else if top + self.cell_h > scroll + view_h {
            (top + self.cell_h - view_h).max(0.0)
        } else {
            scroll
        }
    }
}

/// Keyboard moves in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// The selection after `nav` from `sel` among `n` tiles in `cols` columns (none yet: the first).
pub fn step(sel: Option<usize>, n: usize, cols: usize, nav: Nav) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let cols = cols.max(1);
    let Some(s) = sel.filter(|s| *s < n) else { return Some(0) };
    Some(match nav {
        Nav::Left => s.saturating_sub(1),
        Nav::Right => (s + 1).min(n - 1),
        Nav::Up => if s >= cols { s - cols } else { s },
        Nav::Down => if s + cols < n { s + cols } else { s },
        Nav::Home => 0,
        Nav::End => n - 1,
    })
}

/// The footer line: what is connected and how, then counts, as MobileLab's status strip
/// ("Ready | 6 targets, 0 running").
pub fn footer(state: &str, route: &str, tiles: &[Tile]) -> (String, String) {
    let apps = tiles.iter().filter(|t| t.available && t.id != "desktop").count();
    let open = tiles.iter().filter(|t| t.open).count();
    let mut detail = Vec::new();
    if !route.is_empty() {
        detail.push(route.to_string());
    }
    detail.push(format!("{apps} app{}", if apps == 1 { "" } else { "s" }));
    if open > 0 {
        detail.push(format!("{open} open"));
    }
    (state.to_string(), detail.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, name: &str) -> Tile {
        Tile { id: id.into(), name: name.into(), available: true, open: false }
    }

    #[test]
    fn search_keeps_the_macs_order_and_matches_every_word() {
        let tiles = vec![t("desktop", "Mac Desktop"), t("com.apple.safari", "Safari"), t("com.apple.textedit", "TextEdit"), t("xcode", "Xcode"), t("com.apple.notes", "Notes")];
        assert_eq!(filter(&tiles, ""), vec![0, 1, 2, 3, 4]);
        assert_eq!(filter(&tiles, "te"), vec![2, 4], "TextEdit, Notes");
        assert_eq!(filter(&tiles, "TEXT edit"), vec![2]);
        assert_eq!(filter(&tiles, "apple"), vec![1, 2, 4], "the id counts too");
        assert!(filter(&tiles, "zzz").is_empty());
    }

    #[test]
    fn the_grid_fits_the_width_and_finds_tiles() {
        // 600 DIPs at 1x: 5 columns of 108
        let g = Grid::new(12, 0.0, 100.0, 600.0, 1.0);
        assert_eq!((g.cols, g.rows), (5, 3));
        assert_eq!(g.cell(0, 0.0).0, 30.0, "centred");
        assert_eq!(g.cell(6, 0.0), (30.0 + 108.0, 212.0, 108.0, 112.0));
        assert_eq!(g.at(12, 30.0 + 108.0 + 5.0, 215.0, 0.0), Some(6));
        assert_eq!(g.at(12, 30.0 + 2.0 * 108.0 + 1.0, 100.0 + 2.0 * 112.0 + 1.0, 0.0), None, "past the last tile");
        assert_eq!(g.at(12, 10.0, 150.0, 0.0), None, "left of the grid");
        // scrolled by a row: the first visible row is the second
        assert_eq!(g.at(12, 35.0, 101.0, 112.0), Some(5));
        // at 1.5x: wider cells, fewer columns
        assert_eq!(Grid::new(12, 0.0, 0.0, 600.0, 1.5).cols, 3);
        // scrolling: the last row whole, a tile revealed
        assert_eq!(g.max_scroll(200.0), 336.0 - 200.0);
        assert_eq!(g.reveal(11, 0.0, 200.0), 336.0 - 200.0);
        assert_eq!(g.reveal(0, 100.0, 200.0), 0.0);
        assert_eq!(g.reveal(5, 100.0, 200.0), 100.0, "already whole");
    }

    #[test]
    fn the_keyboard_walks_the_grid() {
        assert_eq!(step(None, 7, 3, Nav::Right), Some(0));
        assert_eq!(step(Some(0), 7, 3, Nav::Right), Some(1));
        assert_eq!(step(Some(0), 7, 3, Nav::Left), Some(0));
        assert_eq!(step(Some(1), 7, 3, Nav::Down), Some(4));
        assert_eq!(step(Some(4), 7, 3, Nav::Down), Some(4), "no tile below in the last row");
        assert_eq!(step(Some(4), 7, 3, Nav::Up), Some(1));
        assert_eq!(step(Some(2), 7, 3, Nav::End), Some(6));
        assert_eq!(step(Some(9), 7, 3, Nav::Home), Some(0), "a stale selection starts over");
        assert_eq!(step(Some(0), 0, 3, Nav::Down), None);
    }

    #[test]
    fn the_footer_says_what_is_connected_and_counts() {
        let mut tiles = vec![t("desktop", "Mac Desktop"), t("a", "A"), t("b", "B")];
        tiles[1].open = true;
        assert_eq!(footer("Connected", "This network", &tiles), ("Connected".into(), "This network, 2 apps, 1 open".into()));
        assert_eq!(footer("Connecting", "", &tiles[..1]).1, "0 apps");
    }
}
