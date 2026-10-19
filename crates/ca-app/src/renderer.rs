//! Choice of the graphics back end that draws the window.
//!
//! The OpenGL back end starts first. A machine without OpenGL 2.0 makes it
//! refuse to start, and the Direct3D 12 back end (software rasterizer when
//! no graphics device exists) starts instead.

use std::ffi::OsString;
use std::fmt;
use std::sync::OnceLock;

/// The switch that picks a back end, as `--renderer=<name>`.
pub const SWITCH: &str = "--renderer=";

/// The environment variable that picks a back end. The switch wins.
pub const ENVIRONMENT: &str = "COMPARE_ALL_RENDERER";

/// A graphics back end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    /// OpenGL through glow.
    Glow,
    /// wgpu: Direct3D 12 on Windows, Vulkan or OpenGL on Linux, Metal on macOS.
    Wgpu,
}

impl Renderer {
    /// The name shown in support lines.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Renderer::Glow => "OpenGL (glow)",
            Renderer::Wgpu => "wgpu",
        }
    }
}

/// Which back ends the start may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preference {
    /// OpenGL first, wgpu when OpenGL does not start.
    #[default]
    Auto,
    /// OpenGL only.
    Glow,
    /// wgpu only.
    Software,
}

impl Preference {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "glow" | "opengl" => Some(Self::Glow),
            "software" | "wgpu" => Some(Self::Software),
            _ => None,
        }
    }

    fn order(self) -> &'static [Renderer] {
        match self {
            Self::Auto => &[Renderer::Glow, Renderer::Wgpu],
            Self::Glow => &[Renderer::Glow],
            Self::Software => &[Renderer::Wgpu],
        }
    }
}

/// Remove `--renderer=<name>` from the arguments and return the preference.
///
/// A value that names no back end stays in the list, so the command line
/// parser refuses it. The environment variable applies when no switch is
/// present.
#[must_use]
pub fn take_preference(
    arguments: Vec<OsString>,
    environment: Option<&str>,
) -> (Preference, Vec<OsString>) {
    let mut preference = None;
    let mut rest = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let value = argument.to_str().and_then(|text| {
            let head = text.get(..SWITCH.len())?;
            if head.eq_ignore_ascii_case(SWITCH) {
                Preference::parse(text.get(SWITCH.len()..)?)
            } else {
                None
            }
        });
        match value {
            Some(found) => preference = Some(found),
            None => rest.push(argument),
        }
    }
    let chosen = preference
        .or_else(|| environment.and_then(Preference::parse))
        .unwrap_or_default();
    (chosen, rest)
}

/// Why one start attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    /// The error text.
    pub message: String,
    /// True when the window already ran the application. Another back end
    /// then cannot take over, because the application state is gone.
    pub application_ran: bool,
}

/// A back end that did not start, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The back end that refused.
    pub renderer: Renderer,
    /// The error text.
    pub message: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.renderer.name(), self.message)
    }
}

/// The result of the start sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The window ran on this back end and ended normally.
    Finished(Renderer),
    /// The window ran on this back end and ended with an error.
    Failed(Renderer, String),
    /// No back end started.
    Unavailable(Vec<Refusal>),
}

/// Run `start` for each permitted back end in order until one starts.
///
/// A failure that happened after the application ran stops the sequence.
pub fn start_in_order(
    preference: Preference,
    start: &mut dyn FnMut(Renderer) -> Result<(), StartFailure>,
) -> Outcome {
    let mut refusals = Vec::new();
    for &renderer in preference.order() {
        match start(renderer) {
            Ok(()) => return Outcome::Finished(renderer),
            Err(failure) if failure.application_ran => {
                return Outcome::Failed(renderer, failure.message)
            }
            Err(failure) => refusals.push(Refusal {
                renderer,
                message: failure.message,
            }),
        }
    }
    Outcome::Unavailable(refusals)
}

/// The text of the message box when no back end starts.
#[must_use]
pub fn unavailable_message(refusals: &[Refusal]) -> String {
    let mut text = String::from(
        "Compare All cannot open its window because no graphics back end started.\n\n",
    );
    for refusal in refusals {
        text.push_str(&refusal.to_string());
        text.push('\n');
    }
    text.push_str(
        "\nUpdate the graphics driver, or run the program on a machine with Direct3D 12 or OpenGL 2.0.",
    );
    text
}

/// The text of the message box when the program panics.
#[must_use]
pub fn panic_message(info: &str) -> String {
    format!("Compare All stopped because of an internal error.\n\n{info}")
}

static ACTIVE: OnceLock<Renderer> = OnceLock::new();

/// Record the back end that runs the window.
pub fn record_active(renderer: Renderer) {
    let _ = ACTIVE.set(renderer);
}

/// The back end that runs the window, once recorded.
#[must_use]
pub fn active() -> Option<Renderer> {
    ACTIVE.get().copied()
}

/// One support line naming the version and the back end.
#[must_use]
pub fn support_line() -> String {
    match active() {
        Some(renderer) => format!("{}, renderer {}", crate::product_title(), renderer.name()),
        None => crate::product_title(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refuse(message: &str) -> Result<(), StartFailure> {
        Err(StartFailure {
            message: message.to_owned(),
            application_ran: false,
        })
    }

    fn arguments(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn glow_that_starts_ends_the_sequence() {
        let mut tried = Vec::new();
        let outcome = start_in_order(Preference::Auto, &mut |renderer| {
            tried.push(renderer);
            Ok(())
        });
        assert_eq!(outcome, Outcome::Finished(Renderer::Glow));
        assert_eq!(tried, [Renderer::Glow]);
    }

    #[test]
    fn a_glow_error_starts_wgpu() {
        let mut tried = Vec::new();
        let outcome = start_in_order(Preference::Auto, &mut |renderer| {
            tried.push(renderer);
            match renderer {
                Renderer::Glow => refuse("opengl 2.0 required"),
                Renderer::Wgpu => Ok(()),
            }
        });
        assert_eq!(outcome, Outcome::Finished(Renderer::Wgpu));
        assert_eq!(tried, [Renderer::Glow, Renderer::Wgpu]);
    }

    #[test]
    fn two_errors_report_both_reasons() {
        let outcome = start_in_order(Preference::Auto, &mut |renderer| match renderer {
            Renderer::Glow => refuse("no opengl"),
            Renderer::Wgpu => refuse("no adapter"),
        });
        let Outcome::Unavailable(refusals) = outcome else {
            unreachable!("both back ends refused");
        };
        assert_eq!(refusals.len(), 2);
        let text = unavailable_message(&refusals);
        assert!(text.contains("no opengl"));
        assert!(text.contains("no adapter"));
    }

    #[test]
    fn an_error_after_the_application_ran_does_not_start_wgpu() {
        let mut tried = Vec::new();
        let outcome = start_in_order(Preference::Auto, &mut |renderer| {
            tried.push(renderer);
            Err(StartFailure {
                message: "lost".to_owned(),
                application_ran: true,
            })
        });
        assert_eq!(outcome, Outcome::Failed(Renderer::Glow, "lost".to_owned()));
        assert_eq!(tried, [Renderer::Glow]);
    }

    #[test]
    fn software_preference_skips_glow() {
        let mut tried = Vec::new();
        let outcome = start_in_order(Preference::Software, &mut |renderer| {
            tried.push(renderer);
            Ok(())
        });
        assert_eq!(outcome, Outcome::Finished(Renderer::Wgpu));
        assert_eq!(tried, [Renderer::Wgpu]);
    }

    #[test]
    fn glow_preference_never_falls_back() {
        let mut tried = Vec::new();
        let outcome = start_in_order(Preference::Glow, &mut |renderer| {
            tried.push(renderer);
            refuse("no opengl")
        });
        assert!(matches!(outcome, Outcome::Unavailable(ref list) if list.len() == 1));
        assert_eq!(tried, [Renderer::Glow]);
    }

    #[test]
    fn the_switch_is_removed_and_wins_over_the_environment() {
        let (preference, rest) = take_preference(
            arguments(&["a.txt", "--renderer=software", "b.txt"]),
            Some("glow"),
        );
        assert_eq!(preference, Preference::Software);
        assert_eq!(rest, arguments(&["a.txt", "b.txt"]));
    }

    #[test]
    fn the_environment_applies_without_a_switch() {
        let (preference, rest) = take_preference(arguments(&["a.txt"]), Some("Software"));
        assert_eq!(preference, Preference::Software);
        assert_eq!(rest, arguments(&["a.txt"]));
    }

    #[test]
    fn an_unknown_value_stays_for_the_command_line_parser() {
        let (preference, rest) = take_preference(arguments(&["--renderer=vulkan"]), None);
        assert_eq!(preference, Preference::Auto);
        assert_eq!(rest, arguments(&["--renderer=vulkan"]));
    }
}
