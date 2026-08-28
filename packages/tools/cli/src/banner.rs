//! The startup banner, and the colour policy that governs it.
//!
//! Everything here writes to **stderr**, so that `taladb export … | jq` and
//! friends keep a clean stdout. Colour is decided once, at the point of
//! writing, from the environment rather than from a flag — the conventions
//! (`NO_COLOR`, `TERM=dumb`, "not a terminal") are what tools like `jq` and
//! `ripgrep` already follow, and a user who has set them has said what they
//! want.

use std::io::IsTerminal;

/// The studio's accent, matching `--accent` in `studio.html` so the terminal
/// and the browser tab look like the same product.
const ACCENT: (u8, u8, u8) = (0xB5, 0x4B, 0x31);
const ACCENT_DIM: (u8, u8, u8) = (0x8A, 0x39, 0x25);

/// Whether to emit ANSI escapes on stderr.
///
/// Deliberately not cached in a `OnceLock`: this runs a handful of times at
/// startup, and a static would make the tests here order-dependent on each
/// other's environment mutation.
fn use_colour() -> bool {
    // https://no-color.org — set to anything at all, including empty.
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    // Before CLICOLOR_FORCE: a dumb terminal cannot render escapes at all, so
    // forcing them there produces literal `[38;2;…m` in the output. "Force" is
    // about capability the environment failed to advertise, not about
    // overriding a terminal that has said it has none.
    if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
        return false;
    }
    // The counterpart, for CI logs that render escapes but are not terminals.
    if std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
        return true;
    }
    std::io::stderr().is_terminal()
}

/// Wrap `text` in a truecolour foreground, or return it unchanged.
fn paint(text: &str, (r, g, b): (u8, u8, u8), bold: bool) -> String {
    if !use_colour() {
        return text.to_owned();
    }
    let weight = if bold { "1;" } else { "" };
    format!("\x1b[{weight}38;2;{r};{g};{b}m{text}\x1b[0m")
}

fn dim(text: &str) -> String {
    if !use_colour() {
        return text.to_owned();
    }
    format!("\x1b[2m{text}\x1b[0m")
}

/// The wordmark: full blocks with a box-drawing shadow ("ANSI Shadow").
///
/// Every glyph here is single-width in every font that has it, so the rows
/// cannot shear relative to each other. When the font *lacks* them the failure
/// is uniform — a column of replacement boxes rather than a wordmark that is
/// subtly one cell out of alignment — and `unicode_ok` steers away from that
/// case entirely.
const WORDMARK: [&str; 5] = [
    "████████╗ █████╗ ██╗      █████╗ ██████╗ ██████╗ ",
    "╚══██╔══╝██╔══██╗██║     ██╔══██╗██╔══██╗██╔══██╗",
    "   ██║   ███████║██║     ███████║██║  ██║██████╔╝",
    "   ██║   ██╔══██║██║     ██╔══██║██║  ██║██╔══██╗",
    "   ██║   ██║  ██║███████╗██║  ██║██████╔╝██████╔╝",
];

/// A plain-text wordmark for terminals that cannot be trusted with the blocks.
const WORDMARK_ASCII: &str = "T A L A D B";

/// True when the terminal is likely to render the block glyphs correctly.
///
/// The check is the encoding, not the terminal: a non-UTF-8 locale is the case
/// where these characters come out as mojibake rather than as a missing glyph.
fn unicode_ok() -> bool {
    if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
        return false;
    }
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        // No locale set at all is the common container case; UTF-8 is the safe
        // assumption there, since the alternative is a banner nobody can read.
        .is_none_or(|v| {
            let v = v.to_uppercase();
            v.contains("UTF-8") || v.contains("UTF8")
        })
}

/// The banner shown when `taladb studio` starts.
///
/// Returned as a `String` rather than printed so it can be asserted on in
/// tests without capturing stderr.
pub(crate) fn studio_banner(version: &str) -> String {
    let mut out = String::from("\n");

    if unicode_ok() {
        for line in WORDMARK {
            out.push_str("  ");
            out.push_str(&paint(line, ACCENT, false));
            out.push('\n');
        }
    } else {
        out.push_str("  ");
        out.push_str(&paint(WORDMARK_ASCII, ACCENT, true));
        out.push('\n');
    }

    // Subtitle: letter-spaced to sit under the wordmark rather than compete
    // with it, with the version trailing so both read as one line of metadata.
    let subtitle = paint("S T U D I O", ACCENT_DIM, true);
    out.push_str(&format!(
        "  {subtitle}  {}\n\n",
        dim(&format!("v{version}"))
    ));
    out
}

/// One `label : value` row of the startup summary.
pub(crate) fn field(label: &str, value: &str) -> String {
    format!("  {} {}\n", dim(&format!("{label:<9}:")), value)
}

/// A warning block, in the same accent so it reads as part of the banner.
pub(crate) fn warn(lines: &[String]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let prefix = if i == 0 { "  ⚠  " } else { "     " };
        out.push_str(&paint(&format!("{prefix}{line}"), ACCENT, i == 0));
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Environment mutation is process-global, so these run under one lock and
    /// restore what they changed. Without it, a parallel test observing
    /// `NO_COLOR` set by another would fail intermittently — the kind of
    /// flake that gets "fixed" by rerunning CI.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = keys
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect::<Vec<_>>();
            for (k, _) in &saved {
                unsafe { std::env::remove_var(k) };
            }
            Self { saved, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(value) => unsafe { std::env::set_var(k, value) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    #[test]
    fn no_color_strips_every_escape() {
        let _guard = EnvGuard::new(&["NO_COLOR", "CLICOLOR_FORCE", "TERM", "LANG"]);
        unsafe { std::env::set_var("NO_COLOR", "1") };

        let banner = studio_banner("0.11.0");
        assert!(
            !banner.contains('\x1b'),
            "NO_COLOR must suppress escapes entirely, got: {banner:?}"
        );
        // The content still has to be there — suppressing colour is not
        // suppressing the banner.
        assert!(banner.contains("S T U D I O"));
        assert!(banner.contains("v0.11.0"));
    }

    #[test]
    fn no_color_wins_over_clicolor_force() {
        let _guard = EnvGuard::new(&["NO_COLOR", "CLICOLOR_FORCE", "TERM", "LANG"]);
        unsafe { std::env::set_var("NO_COLOR", "1") };
        unsafe { std::env::set_var("CLICOLOR_FORCE", "1") };

        // The spec is explicit that NO_COLOR is the stronger signal, and a user
        // who has set both most likely inherited one from their shell profile.
        assert!(!studio_banner("0.11.0").contains('\x1b'));
    }

    #[test]
    fn clicolor_force_paints_without_a_terminal() {
        let _guard = EnvGuard::new(&["NO_COLOR", "CLICOLOR_FORCE", "TERM", "LANG"]);
        unsafe { std::env::set_var("CLICOLOR_FORCE", "1") };

        // Tests capture stderr, so `is_terminal()` is false here — which is
        // exactly the CI case this variable exists for.
        assert!(studio_banner("0.11.0").contains("\x1b[38;2;181;75;49m"));
    }

    #[test]
    fn dumb_terminals_get_neither_colour_nor_blocks() {
        let _guard = EnvGuard::new(&["NO_COLOR", "CLICOLOR_FORCE", "TERM", "LANG"]);
        unsafe { std::env::set_var("TERM", "dumb") };
        // Even forced: a dumb terminal renders escapes as literal text, so
        // "force colour" there makes the banner worse, not better.
        unsafe { std::env::set_var("CLICOLOR_FORCE", "1") };

        let banner = studio_banner("0.11.0");
        assert!(!banner.contains('\x1b'));
        assert!(
            banner.contains("T A L A D B"),
            "expected the ASCII wordmark"
        );
        assert!(!banner.contains('█'));
    }

    #[test]
    fn non_utf8_locale_falls_back_to_ascii() {
        let _guard = EnvGuard::new(&[
            "NO_COLOR",
            "CLICOLOR_FORCE",
            "TERM",
            "LC_ALL",
            "LC_CTYPE",
            "LANG",
        ]);
        unsafe { std::env::set_var("LANG", "C") };

        // Mojibake is worse than a plain wordmark.
        let banner = studio_banner("0.11.0");
        assert!(banner.contains("T A L A D B"));
        assert!(!banner.contains('█'));
    }

    #[test]
    fn utf8_locale_gets_the_wordmark() {
        let _guard = EnvGuard::new(&[
            "NO_COLOR",
            "CLICOLOR_FORCE",
            "TERM",
            "LC_ALL",
            "LC_CTYPE",
            "LANG",
        ]);
        unsafe { std::env::set_var("LANG", "en_US.UTF-8") };

        assert!(studio_banner("0.11.0").contains('█'));
    }

    #[test]
    fn fields_align_on_the_colon() {
        let _guard = EnvGuard::new(&["NO_COLOR", "CLICOLOR_FORCE", "TERM", "LANG"]);
        unsafe { std::env::set_var("NO_COLOR", "1") };

        let db = field("Database", "app.db");
        let url = field("URL", "http://localhost:4321");
        let colon = |s: &str| s.find(':').expect("a field has a colon");
        assert_eq!(colon(&db), colon(&url), "labels must pad to a common width");
    }
}
