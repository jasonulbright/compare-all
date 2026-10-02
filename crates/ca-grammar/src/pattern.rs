//! The regular expression dialect grammars and rules are written in.
//!
//! The documented dialect is a subset of PCRE with three deviations from what
//! the default engine accepts, all handled by [`translate`]:
//!
//! - `\s` and `\S` mean space or tab only, not the full Unicode whitespace set.
//! - `\e` is the escape character, 0x1B.
//! - back references and lookaround are part of the dialect, so a pattern that
//!   uses them is routed to a backtracking engine with a bounded budget.
//!
//! Patterns reach the engines through [`CompiledPattern::compile`], which caps
//! the compiled program size. Together with the backtracking budget that keeps
//! a hostile or merely careless user grammar from making a comparison hang.

use crate::GrammarError;

/// Upper bound on the compiled size of one pattern, in bytes.
const PROGRAM_SIZE_LIMIT: usize = 1 << 20;

/// Upper bound on backtracking steps for one match attempt.
const BACKTRACK_LIMIT: usize = 200_000;

/// A pattern compiled for one of the two engines.
///
/// The linear-time engine handles everything it can; the backtracking engine
/// is used only for patterns whose features require it.
#[derive(Debug, Clone)]
pub enum CompiledPattern {
    /// Compiled for the linear-time engine.
    Linear(Box<regex::Regex>),
    /// Compiled for the bounded backtracking engine.
    Backtracking(Box<fancy_regex::Regex>, String),
}

impl CompiledPattern {
    /// Compile `pattern` in the documented dialect.
    ///
    /// `case_sensitive` maps onto the engine's own case flag rather than being
    /// spliced into the pattern text, so an inline `(?i)` in the pattern still
    /// overrides it for the part of the pattern that follows.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError::Pattern`] when neither engine accepts the
    /// translated pattern, or when it exceeds the program size limit.
    pub fn compile(pattern: &str, case_sensitive: bool) -> Result<Self, GrammarError> {
        let translated = translate(pattern);
        if !needs_backtracking(&translated) {
            let built = regex::RegexBuilder::new(&translated)
                .case_insensitive(!case_sensitive)
                .size_limit(PROGRAM_SIZE_LIMIT)
                .dfa_size_limit(PROGRAM_SIZE_LIMIT)
                .build();
            if let Ok(re) = built {
                return Ok(Self::Linear(Box::new(re)));
            }
        }
        let prefixed = if case_sensitive {
            translated
        } else {
            format!("(?i){translated}")
        };
        let re = fancy_regex::RegexBuilder::new(&prefixed)
            .backtrack_limit(BACKTRACK_LIMIT)
            .build()
            .map_err(|e| GrammarError::Pattern {
                pattern: pattern.to_owned(),
                message: e.to_string(),
            })?;
        Ok(Self::Backtracking(Box::new(re), pattern.to_owned()))
    }

    /// The leftmost match starting at or after `start`, as a byte range.
    ///
    /// The whole of `haystack` is passed to the engine rather than a slice from
    /// `start`, so `^`, `$` and lookbehind keep their line-wide meaning no
    /// matter where the scan resumes.
    ///
    /// # Errors
    ///
    /// Returns [`GrammarError::Backtrack`] when the backtracking engine runs
    /// out of budget.
    pub fn find_at(
        &self,
        haystack: &str,
        start: usize,
    ) -> Result<Option<(usize, usize)>, GrammarError> {
        match self {
            Self::Linear(re) => Ok(re.find_at(haystack, start).map(|m| (m.start(), m.end()))),
            Self::Backtracking(re, source) => match re.find_from_pos(haystack, start) {
                Ok(found) => Ok(found.map(|m| (m.start(), m.end()))),
                Err(fancy_regex::Error::RuntimeError(_)) => Err(GrammarError::Backtrack {
                    pattern: source.clone(),
                }),
                Err(e) => Err(GrammarError::Pattern {
                    pattern: source.clone(),
                    message: e.to_string(),
                }),
            },
        }
    }

    /// Whether this pattern went to the bounded backtracking engine.
    pub fn is_backtracking(&self) -> bool {
        matches!(self, Self::Backtracking(..))
    }
}

/// Rewrite the documented dialect into what the engines accept.
///
/// The scan tracks whether it is inside a bracket class, because the expansion
/// of `\s` differs: a class wants bare members, everything else wants a class.
#[must_use]
pub fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    // A `]` immediately after `[` or `[^` is a literal member, not the close.
    let mut class_start_distance = 0_u8;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(next) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match next {
                    's' if in_class => out.push_str(" \\t"),
                    's' => out.push_str("[ \\t]"),
                    // A negated shorthand stays a class even inside another
                    // class: `^` anywhere but the first position of a class is
                    // an ordinary member, so splicing the members in would
                    // change the pattern's meaning. Both engines accept a
                    // nested class.
                    'S' => out.push_str("[^ \\t]"),
                    'e' => out.push_str("\\x1B"),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                class_start_distance = 0;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    out.push('^');
                    let _ = chars.next();
                }
                continue;
            }
            ']' if in_class && class_start_distance > 0 => {
                in_class = false;
                out.push(']');
            }
            other => out.push(other),
        }
        if in_class {
            class_start_distance = class_start_distance.saturating_add(1);
        }
    }
    out
}

/// Whether a translated pattern uses a feature only the backtracking engine has.
fn needs_backtracking(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    let mut in_class = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                if !in_class && bytes[i + 1].is_ascii_digit() && bytes[i + 1] != b'0' {
                    return true;
                }
                i += 2;
                continue;
            }
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'(' if !in_class && pattern[i..].starts_with("(?") => {
                let rest = &pattern[i + 2..];
                if rest.starts_with('=')
                    || rest.starts_with('!')
                    || rest.starts_with("<=")
                    || rest.starts_with("<!")
                {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_shorthand_is_space_or_tab_only() {
        assert_eq!(translate(r"\s+"), "[ \\t]+");
        assert_eq!(translate(r"[\sA]"), "[ \\tA]");
        assert_eq!(translate(r"\S"), "[^ \\t]");
        let re = CompiledPattern::compile(r"\s", true).unwrap();
        assert_eq!(re.find_at(" ", 0).unwrap(), Some((0, 1)));
        assert_eq!(re.find_at("\u{a0}", 0).unwrap(), None);
    }

    #[test]
    fn a_negated_shorthand_inside_a_class_keeps_its_meaning() {
        assert_eq!(translate(r"[a\Sb]"), "[a[^ \\t]b]");
        let re = CompiledPattern::compile(r"[a\Sb]", true).unwrap();
        // Every non-blank character is a member, and a blank is not one.
        assert_eq!(re.find_at("z", 0).unwrap(), Some((0, 1)));
        assert_eq!(re.find_at("a", 0).unwrap(), Some((0, 1)));
        assert_eq!(re.find_at(" ", 0).unwrap(), None);
        assert_eq!(re.find_at("\t", 0).unwrap(), None);
    }

    #[test]
    fn a_negated_shorthand_inside_a_class_reaches_the_backtracking_engine_too() {
        let re = CompiledPattern::compile(r"(?=x)[x\Sy]", true).unwrap();
        assert!(re.is_backtracking());
        assert_eq!(re.find_at("x", 0).unwrap(), Some((0, 1)));
    }

    #[test]
    fn a_negated_class_holding_a_negated_shorthand_still_compiles() {
        let re = CompiledPattern::compile(r"[^\S]", true).unwrap();
        assert_eq!(re.find_at(" ", 0).unwrap(), Some((0, 1)));
        assert_eq!(re.find_at("z", 0).unwrap(), None);
    }

    #[test]
    fn the_engines_own_negated_shorthands_pass_through_a_class_unchanged() {
        assert_eq!(translate(r"[a\Db]"), r"[a\Db]");
        assert_eq!(translate(r"[a\Wb]"), r"[a\Wb]");
        let re = CompiledPattern::compile(r"[a\D]", true).unwrap();
        assert_eq!(re.find_at("5", 0).unwrap(), None);
        assert_eq!(re.find_at("z", 0).unwrap(), Some((0, 1)));
        let re = CompiledPattern::compile(r"[1\W]", true).unwrap();
        assert_eq!(re.find_at("q", 0).unwrap(), None);
        assert_eq!(re.find_at("-", 0).unwrap(), Some((0, 1)));
    }

    #[test]
    fn escape_shorthand_maps_to_its_code_point() {
        let re = CompiledPattern::compile(r"\e", true).unwrap();
        assert_eq!(re.find_at("\u{1b}", 0).unwrap(), Some((0, 1)));
    }

    #[test]
    fn hex_escapes_survive_translation() {
        let re = CompiledPattern::compile(r"\x0C|\x{00A7}", true).unwrap();
        assert_eq!(re.find_at("\u{c}", 0).unwrap(), Some((0, 1)));
        assert_eq!(re.find_at("\u{a7}", 0).unwrap(), Some((0, 2)));
    }

    #[test]
    fn back_references_route_to_the_backtracking_engine() {
        let re = CompiledPattern::compile(r"b(.)\1n", true).unwrap();
        assert!(re.is_backtracking());
        assert_eq!(re.find_at("been", 0).unwrap(), Some((0, 4)));
        assert_eq!(re.find_at("bean", 0).unwrap(), None);
    }

    #[test]
    fn lookaround_routes_to_the_backtracking_engine() {
        let re = CompiledPattern::compile(r"foo(?=bar)", true).unwrap();
        assert!(re.is_backtracking());
        assert_eq!(re.find_at("foobar", 0).unwrap(), Some((0, 3)));
    }

    #[test]
    fn a_class_holding_a_digit_escape_is_not_mistaken_for_a_back_reference() {
        let re = CompiledPattern::compile(r"[\d]+", true).unwrap();
        assert!(!re.is_backtracking());
    }

    #[test]
    fn case_flag_applies_and_inline_modifier_overrides_it() {
        let re = CompiledPattern::compile("abc", false).unwrap();
        assert_eq!(re.find_at("ABC", 0).unwrap(), Some((0, 3)));
        let re = CompiledPattern::compile("(?-i)abc", false).unwrap();
        assert_eq!(re.find_at("ABC", 0).unwrap(), None);
    }

    #[test]
    fn line_anchors_keep_their_meaning_when_the_scan_resumes_mid_line() {
        let re = CompiledPattern::compile("^ab", true).unwrap();
        assert_eq!(re.find_at("xxab", 2).unwrap(), None);
        assert_eq!(re.find_at("abab", 1).unwrap(), None);
    }

    #[test]
    fn a_runaway_backtracking_pattern_reports_rather_than_hangs() {
        let re = CompiledPattern::compile(r"(a+)+$\1", true).unwrap();
        let hay = "a".repeat(60);
        let outcome = re.find_at(&hay, 0);
        match outcome {
            Err(GrammarError::Backtrack { .. }) | Ok(None) => {}
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn a_pattern_that_neither_engine_accepts_is_reported() {
        let err = CompiledPattern::compile("(unclosed", true).unwrap_err();
        assert!(matches!(err, GrammarError::Pattern { .. }));
    }
}
