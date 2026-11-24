//! Multi-line text editor state for the chatbox.
use unicode_width::UnicodeWidthChar;

#[derive(Default, Clone)]
pub struct Input {
    chars: Vec<char>,
    cur: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
}

impl Input {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cur = 0;
        self.hist_pos = None;
    }
    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cur = self.chars.len();
    }
    /// Take the text, record it in history and clear the editor.
    pub fn submit(&mut self) -> String {
        let t = self.text();
        if !t.trim().is_empty() && self.history.last() != Some(&t) {
            self.history.push(t.clone());
        }
        self.clear();
        t
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cur, c);
        self.cur += 1;
    }
    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() {
            if c == '\t' {
                self.insert(' ');
                self.insert(' ');
            } else if c == '\n' || !c.is_control() {
                self.insert(c);
            }
        }
    }
    pub fn newline(&mut self) {
        self.insert('\n');
    }
    pub fn backspace(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
            self.chars.remove(self.cur);
        }
    }
    pub fn delete(&mut self) {
        if self.cur < self.chars.len() {
            self.chars.remove(self.cur);
        }
    }
    pub fn left(&mut self) {
        self.cur = self.cur.saturating_sub(1);
    }
    pub fn right(&mut self) {
        self.cur = (self.cur + 1).min(self.chars.len());
    }
    fn line_start(&self, from: usize) -> usize {
        let mut i = from;
        while i > 0 && self.chars[i - 1] != '\n' {
            i -= 1;
        }
        i
    }
    fn line_end(&self, from: usize) -> usize {
        let mut i = from;
        while i < self.chars.len() && self.chars[i] != '\n' {
            i += 1;
        }
        i
    }
    pub fn home(&mut self) {
        self.cur = self.line_start(self.cur);
    }
    pub fn end(&mut self) {
        self.cur = self.line_end(self.cur);
    }
    pub fn word_left(&mut self) {
        while self.cur > 0 && self.chars[self.cur - 1].is_whitespace() {
            self.cur -= 1;
        }
        while self.cur > 0 && !self.chars[self.cur - 1].is_whitespace() {
            self.cur -= 1;
        }
    }
    pub fn word_right(&mut self) {
        let n = self.chars.len();
        while self.cur < n && self.chars[self.cur].is_whitespace() {
            self.cur += 1;
        }
        while self.cur < n && !self.chars[self.cur].is_whitespace() {
            self.cur += 1;
        }
    }
    pub fn kill_word(&mut self) {
        let end = self.cur;
        self.word_left();
        self.chars.drain(self.cur..end);
    }
    pub fn kill_to_start(&mut self) {
        let s = self.line_start(self.cur);
        self.chars.drain(s..self.cur);
        self.cur = s;
    }
    pub fn kill_to_end(&mut self) {
        let e = self.line_end(self.cur);
        self.chars.drain(self.cur..e);
    }

    fn is_multiline(&self) -> bool {
        self.chars.contains(&'\n')
    }
    /// Up: move within a multi-line buffer, otherwise walk history.
    pub fn up(&mut self) {
        if self.is_multiline() && self.line_start(self.cur) > 0 {
            let ls = self.line_start(self.cur);
            let col = self.cur - ls;
            let ps = self.line_start(ls - 1);
            self.cur = (ps + col).min(ls - 1);
            return;
        }
        if self.history.is_empty() {
            return;
        }
        let pos = match self.hist_pos {
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
            Some(p) => p.saturating_sub(1),
        };
        self.hist_pos = Some(pos);
        let h = self.history[pos].clone();
        self.set(&h);
    }
    pub fn down(&mut self) {
        if self.is_multiline() && self.line_end(self.cur) < self.chars.len() {
            let ls = self.line_start(self.cur);
            let col = self.cur - ls;
            let ns = self.line_end(self.cur) + 1;
            let ne = self.line_end(ns);
            self.cur = (ns + col).min(ne);
            return;
        }
        if let Some(p) = self.hist_pos {
            if p + 1 < self.history.len() {
                self.hist_pos = Some(p + 1);
                let h = self.history[p + 1].clone();
                self.set(&h);
            } else {
                self.hist_pos = None;
                let d = std::mem::take(&mut self.draft);
                self.set(&d);
            }
        }
    }

    /// Hard-wrap into visual lines of `width` cells. Returns the lines and the
    /// cursor's (row, col) in that layout.
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut lines = vec![String::new()];
        let mut col = 0usize;
        let mut cursor = (0usize, 0usize);
        for i in 0..=self.chars.len() {
            if i == self.cur {
                cursor = (lines.len() - 1, col);
            }
            if i == self.chars.len() {
                break;
            }
            let c = self.chars[i];
            if c == '\n' {
                lines.push(String::new());
                col = 0;
                continue;
            }
            let w = c.width().unwrap_or(0);
            if col + w > width {
                lines.push(String::new());
                col = 0;
                if i == self.cur {
                    cursor = (lines.len() - 1, 0);
                }
            }
            lines.last_mut().unwrap().push(c);
            col += w;
        }
        (lines, cursor)
    }
}
