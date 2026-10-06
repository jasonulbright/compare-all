//! Exit codes and what each one means.

/// One exit code with the text that explains it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitMeaning {
    /// The code itself.
    pub code: u8,
    /// What the code says.
    pub meaning: &'static str,
}

/// The run did what it was asked.
pub const SUCCESS: u8 = 0;
/// A byte comparison found the two files identical.
pub const BINARY_SAME: u8 = 1;
/// A rules based comparison found the two files identical.
pub const RULES_SAME: u8 = 2;
/// A byte comparison found differences.
pub const BINARY_DIFFERENT: u8 = 11;
/// The two files differ, but only in text the rules call unimportant.
pub const SIMILAR: u8 = 12;
/// A rules based comparison found differences that matter.
pub const RULES_DIFFERENT: u8 = 13;
/// A merge finished with conflicts, or folder merge items that need a merge
/// by hand, left in it.
pub const CONFLICTS: u8 = 14;
/// Something failed that no other code names, or a folder merge step did not
/// complete.
pub const UNKNOWN_ERROR: u8 = 100;
/// A merge left conflicts, or folder merge items that need a merge by hand,
/// and wrote nothing to the output.
pub const CONFLICTS_NO_OUTPUT: u8 = 101;
/// A waiting launcher could not wait for the comparison.
pub const LAUNCHER_CANNOT_WAIT: u8 = 102;
/// A waiting launcher could not find the program to run.
pub const LAUNCHER_NOT_FOUND: u8 = 103;
/// The licence period ended. Reserved; this program never returns it.
pub const LICENCE_EXPIRED: u8 = 104;
/// The script file could not be read.
pub const SCRIPT_NOT_LOADED: u8 = 105;
/// The script file does not parse.
pub const SCRIPT_SYNTAX: u8 = 106;
/// A script or a quick comparison could not open what it names.
pub const SCRIPT_PATHS: u8 = 107;

/// Every exit code, in order, with its meaning.
pub const TABLE: &[ExitMeaning] = &[
    ExitMeaning {
        code: SUCCESS,
        meaning: "success",
    },
    ExitMeaning {
        code: BINARY_SAME,
        meaning: "binary comparison: the files are identical",
    },
    ExitMeaning {
        code: RULES_SAME,
        meaning: "rules based comparison: the files are identical",
    },
    ExitMeaning {
        code: BINARY_DIFFERENT,
        meaning: "binary comparison: the files differ",
    },
    ExitMeaning {
        code: SIMILAR,
        meaning: "the files differ only in text the rules call unimportant",
    },
    ExitMeaning {
        code: RULES_DIFFERENT,
        meaning: "rules based comparison: the files differ",
    },
    ExitMeaning {
        code: CONFLICTS,
        meaning: "the merge left conflicts, or folder merge items that need a merge by hand",
    },
    ExitMeaning {
        code: UNKNOWN_ERROR,
        meaning: "an error no other code names, or a folder merge step that did not complete",
    },
    ExitMeaning {
        code: CONFLICTS_NO_OUTPUT,
        meaning: "the merge left conflicts, or folder merge items that need a merge by hand, \
                  and wrote nothing to the output",
    },
    ExitMeaning {
        code: LAUNCHER_CANNOT_WAIT,
        meaning: "a waiting launcher could not wait for the comparison",
    },
    ExitMeaning {
        code: LAUNCHER_NOT_FOUND,
        meaning: "a waiting launcher could not find the program",
    },
    ExitMeaning {
        code: LICENCE_EXPIRED,
        meaning: "the licence period ended; reserved, never returned here",
    },
    ExitMeaning {
        code: SCRIPT_NOT_LOADED,
        meaning: "the script file could not be read",
    },
    ExitMeaning {
        code: SCRIPT_SYNTAX,
        meaning: "the script file has a syntax error",
    },
    ExitMeaning {
        code: SCRIPT_PATHS,
        meaning: "a script or a quick comparison could not open what it names",
    },
];
