//! A one-line text field's text and editing, as a Mac text field edits (portable, tested):
//! a caret and a selection, moving by letters and words, Shift to select, deleting a word,
//! select all, paste (newlines dropped), and a secure field that shows bullets and never copies.
//! Positions are counted in characters.

#[derive(Debug, Clone, Default)]
pub struct Field {
    text: String,
    /// the caret, and the other end of the selection (equal: nothing selected)
    caret: usize,
    anchor: usize,
    pub secure: bool,
    pub max: usize,
}

/// A move of the caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
}

impl Field {
    pub fn new(text: &str, secure: bool, max: usize) -> Field {
        let n = text.chars().count();
        Field { text: text.chars().take(max).collect(), caret: n.min(max), anchor: n.min(max), secure, max }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, t: &str) {
        self.text = t.chars().filter(|c| !c.is_control()).take(self.max).collect();
        self.caret = self.len();
        self.anchor = self.caret;
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selection as (start, end) in characters (start == end: none).
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// What is shown: the text, or one bullet a character.
    pub fn shown(&self) -> String {
        if self.secure {
            "•".repeat(self.len())
        } else {
            self.text.clone()
        }
    }

    fn byte(&self, ch: usize) -> usize {
        self.text.char_indices().nth(ch).map_or(self.text.len(), |(i, _)| i)
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        let (ba, bb) = (self.byte(a), self.byte(b));
        self.text.replace_range(ba..bb, "");
        self.caret = a;
        self.anchor = a;
        true
    }

    /// Type or paste `s` over the selection (control characters and newlines dropped, up to
    /// `max` characters in all).
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let room = self.max.saturating_sub(self.len());
        let add: String = s.chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).take(room).collect();
        let at = self.byte(self.caret);
        self.text.insert_str(at, &add);
        self.caret += add.chars().count();
        self.anchor = self.caret;
    }

    /// Where a word starts before `i`, or ends after it (spaces and punctuation between words).
    fn word_left(&self, i: usize) -> usize {
        let c = self.chars();
        let mut j = i;
        while j > 0 && !c[j - 1].is_alphanumeric() {
            j -= 1;
        }
        while j > 0 && c[j - 1].is_alphanumeric() {
            j -= 1;
        }
        j
    }

    fn word_right(&self, i: usize) -> usize {
        let c = self.chars();
        let mut j = i;
        while j < c.len() && !c[j].is_alphanumeric() {
            j += 1;
        }
        while j < c.len() && c[j].is_alphanumeric() {
            j += 1;
        }
        j
    }

    /// Backspace (a word with `word`: Ctrl+Backspace, as Option+Delete on the Mac).
    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() || self.caret == 0 {
            return;
        }
        let from = if word && !self.secure { self.word_left(self.caret) } else { self.caret - 1 };
        self.anchor = from;
        self.delete_selection();
    }

    /// Delete forward.
    pub fn delete(&mut self, word: bool) {
        if self.delete_selection() || self.caret >= self.len() {
            return;
        }
        let to = if word && !self.secure { self.word_right(self.caret) } else { self.caret + 1 };
        self.anchor = to;
        self.delete_selection();
    }

    /// Move the caret (with `select`, the selection grows from where it was).
    pub fn go(&mut self, m: Move, select: bool) {
        let (a, b) = self.selection();
        let to = match m {
            // an arrow with something selected goes to that end of it
            Move::Left if !select && a != b => a,
            Move::Right if !select && a != b => b,
            Move::Left => self.caret.saturating_sub(1),
            Move::Right => (self.caret + 1).min(self.len()),
            Move::WordLeft if self.secure => 0,
            Move::WordRight if self.secure => self.len(),
            Move::WordLeft => self.word_left(self.caret),
            Move::WordRight => self.word_right(self.caret),
            Move::Home => 0,
            Move::End => self.len(),
        };
        self.caret = to;
        if !select {
            self.anchor = to;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.len();
    }

    /// The word at character `i` selected (a double click).
    pub fn select_word(&mut self, i: usize) {
        if self.secure {
            return self.select_all();
        }
        let i = i.min(self.len());
        let c = self.chars();
        let (mut a, mut b) = (i, i);
        while a > 0 && c[a - 1].is_alphanumeric() {
            a -= 1;
        }
        while b < c.len() && c[b].is_alphanumeric() {
            b += 1;
        }
        self.anchor = a;
        self.caret = b;
    }

    /// Put the caret at character `i` (a click; with `select`, Shift-click or a drag).
    pub fn place(&mut self, i: usize, select: bool) {
        self.caret = i.min(self.len());
        if !select {
            self.anchor = self.caret;
        }
    }

    /// What Copy takes (nothing from a secure field).
    pub fn copied(&self) -> Option<String> {
        let (a, b) = self.selection();
        (!self.secure && a != b).then(|| self.text.chars().skip(a).take(b - a).collect())
    }

    /// Cut: what Copy takes, and it goes.
    pub fn cut(&mut self) -> Option<String> {
        let c = self.copied()?;
        self.delete_selection();
        Some(c)
    }
}

/// The character position nearest `x` among caret stops `xs` (one per position, 0..=len).
pub fn nearest(xs: &[f32], x: f32) -> usize {
    let mut best = 0;
    for (i, &p) in xs.iter().enumerate() {
        if (p - x).abs() < (xs[best] - x).abs() {
            best = i;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_moving_and_selecting() {
        let mut f = Field::new("", false, 64);
        f.insert("hello world");
        assert_eq!((f.text(), f.caret()), ("hello world", 11));
        f.go(Move::WordLeft, false);
        assert_eq!(f.caret(), 6);
        f.go(Move::WordRight, true);
        assert_eq!(f.selection(), (6, 11));
        assert_eq!(f.copied().as_deref(), Some("world"));
        f.insert("Mac");
        assert_eq!(f.text(), "hello Mac");
        f.go(Move::Home, false);
        f.go(Move::Right, true);
        f.go(Move::Right, true);
        assert_eq!(f.cut().as_deref(), Some("he"));
        assert_eq!(f.text(), "llo Mac");
        f.go(Move::End, false);
        f.backspace(true);
        assert_eq!(f.text(), "llo ");
        f.select_all();
        f.backspace(false);
        assert!(f.is_empty());
    }

    #[test]
    fn vietnamese_and_paste() {
        let mut f = Field::new("", false, 20);
        f.insert("máy Mac\nmới");
        assert_eq!(f.text(), "máy Mac mới");
        f.go(Move::Left, false);
        f.backspace(false);
        assert_eq!(f.text(), "máy Mac mi");
        f.place(1, false);
        f.select_word(1);
        assert_eq!(f.copied().as_deref(), Some("máy"));
        // no more than max
        f.select_all();
        f.insert(&"x".repeat(50));
        assert_eq!(f.len(), 20);
    }

    #[test]
    fn a_secure_field_shows_bullets_and_never_copies() {
        let mut f = Field::new("", true, 64);
        f.insert("s3cret pass");
        assert_eq!(f.shown(), "•".repeat(11));
        f.select_all();
        assert_eq!(f.copied(), None);
        assert_eq!(f.cut(), None);
        assert_eq!(f.text(), "s3cret pass");
        f.go(Move::End, false);
        f.backspace(true);
        assert_eq!(f.text(), "s3cret pas", "a word is not deleted at once: the letters are hidden");
        f.go(Move::WordLeft, false);
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn clicks_find_the_nearest_place() {
        let xs = [0.0, 8.0, 15.0, 24.0];
        assert_eq!(nearest(&xs, -3.0), 0);
        assert_eq!(nearest(&xs, 10.0), 1);
        assert_eq!(nearest(&xs, 13.0), 2);
        assert_eq!(nearest(&xs, 100.0), 3);
    }
}
