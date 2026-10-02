//! Argument substitution.
//!
//! `%1` through `%9` stand for the arguments that follow the script name on the
//! command line. A name between percent signs stands for an environment
//! variable of that exact name, or for one of the values the script language
//! fills in: `date`, `time` and `fn_time`. A name that stands for nothing is
//! left in place unchanged, so a text that only looks like a variable survives.

use std::collections::BTreeMap;

use crate::clock;

/// Where substituted values come from.
#[derive(Debug, Clone)]
pub struct Substitution {
    active: bool,
    args: Vec<String>,
    env: Option<BTreeMap<String, String>>,
    now: i64,
    offset_seconds: i64,
}

impl Default for Substitution {
    fn default() -> Self {
        Self::none()
    }
}

impl Substitution {
    /// Leave every argument exactly as written.
    #[must_use]
    pub fn none() -> Self {
        Self {
            active: false,
            args: Vec::new(),
            env: Some(BTreeMap::new()),
            now: 0,
            offset_seconds: 0,
        }
    }

    /// Substitute from the given arguments, the process environment and the
    /// system clock.
    #[must_use]
    pub fn from_environment(args: Vec<String>) -> Self {
        Self {
            active: true,
            args,
            env: None,
            now: clock::unix_seconds(std::time::SystemTime::now()),
            offset_seconds: 0,
        }
    }

    /// Substitute from values the caller supplies. Tests use this so a run is
    /// repeatable.
    #[must_use]
    pub fn fixed(args: Vec<String>, env: BTreeMap<String, String>, now: i64) -> Self {
        Self {
            active: true,
            args,
            env: Some(env),
            now,
            offset_seconds: 0,
        }
    }

    /// Read the clock values in a zone this many seconds ahead of UTC.
    #[must_use]
    pub fn with_offset_seconds(mut self, offset_seconds: i64) -> Self {
        self.offset_seconds = offset_seconds;
        self
    }

    /// The count of seconds the clock values are taken from.
    #[must_use]
    pub fn now(&self) -> i64 {
        self.now + self.offset_seconds
    }

    fn variable(&self, name: &str) -> Option<String> {
        match name {
            "date" => return Some(clock::format_date(self.now())),
            "time" => return Some(clock::format_time(self.now())),
            "fn_time" => return Some(clock::format_filename_time(self.now())),
            _ => {}
        }
        match &self.env {
            Some(map) => map.get(name).cloned(),
            None => std::env::var(name).ok(),
        }
    }

    /// Replace every reference in one argument.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        if !self.active || !text.contains('%') {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] != '%' {
                out.push(chars[index]);
                index += 1;
                continue;
            }
            if let Some(digit) = chars.get(index + 1).and_then(|c| c.to_digit(10)) {
                if (1..=9).contains(&digit) {
                    let position = digit as usize - 1;
                    out.push_str(self.args.get(position).map_or("", String::as_str));
                    index += 2;
                    continue;
                }
            }
            let close = chars[index + 1..].iter().position(|c| *c == '%');
            if let Some(offset) = close {
                let name: String = chars[index + 1..index + 1 + offset].iter().collect();
                if let Some(value) = self.variable(&name) {
                    out.push_str(&value);
                } else {
                    out.push('%');
                    out.push_str(&name);
                    out.push('%');
                }
                index += offset + 2;
            } else {
                out.push('%');
                index += 1;
            }
        }
        out
    }
}
