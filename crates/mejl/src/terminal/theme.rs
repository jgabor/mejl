//! Live theme reports (mode 2031).
//!
//! With mode 2031 on, the terminal sends `CSI ? 997 ; 1 n` when its colour
//! scheme turns dark and `CSI ? 997 ; 2 n` when it turns light, so mejl can
//! switch themes without a restart. The probe's `CSI ? 996 n` query tells
//! whether the terminal does this
//! ([`Capabilities::theme_reports`](super::probe::Capabilities::theme_reports)).
//!
//! Not switched on yet. crossterm 0.29 cannot parse a `CSI ? ... n`
//! sequence: its parser waits for a `u` or `c` final byte, so after a report
//! it holds, and then drops, every key up to the next `u` or `c`, and its
//! blocking read stalls the input thread meanwhile (seen in a pty: after
//! `ESC [ ? 997 ; 1 n`, the keys `j` and `k` never arrived). Turning the
//! mode on safely needs mejl to read the terminal's bytes before crossterm
//! does. [`parse_reports`] and [`crate::events::TerminalReport`] are ready
//! for that reader.

use std::fmt;

use crossterm::Command;

use super::probe::Background;
use crate::events::TerminalReport;

/// Turns theme reports on: `CSI ? 2031 h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnableThemeReports;

/// Turns theme reports off: `CSI ? 2031 l`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisableThemeReports;

impl Command for EnableThemeReports {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?2031h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

impl Command for DisableThemeReports {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?2031l")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

/// Every theme report in `bytes`, in order. Other sequences and stray
/// bytes are skipped, and so is a report with an unknown value. An
/// unfinished report at the end is left for the next read.
pub fn parse_reports(bytes: &[u8]) -> Vec<TerminalReport> {
    const PREFIX: &[u8] = b"\x1b[?997;";
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some(at) = find(rest, PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let digits = after.iter().take_while(|b| b.is_ascii_digit()).count();
        match after.get(digits) {
            Some(b'n') => {
                let theme = match &after[..digits] {
                    b"1" => Some(Background::Dark),
                    b"2" => Some(Background::Light),
                    _ => None,
                };
                if let Some(theme) = theme {
                    out.push(TerminalReport::Theme(theme));
                }
                rest = &after[digits + 1..];
            }
            // Cut off: nothing more to find.
            None => break,
            // Not a report after all.
            Some(_) => rest = &after[digits..],
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_mode_2031() {
        let mut s = String::new();
        EnableThemeReports.write_ansi(&mut s).unwrap();
        DisableThemeReports.write_ansi(&mut s).unwrap();
        assert_eq!(s, "\x1b[?2031h\x1b[?2031l");
    }

    #[test]
    fn reports_parse_in_order() {
        assert_eq!(
            parse_reports(b"\x1b[?997;1n"),
            vec![TerminalReport::Theme(Background::Dark)]
        );
        assert_eq!(
            parse_reports(b"j\x1b[?997;2nk\x1b[?997;1n"),
            vec![
                TerminalReport::Theme(Background::Light),
                TerminalReport::Theme(Background::Dark),
            ]
        );
    }

    #[test]
    fn other_bytes_are_not_reports() {
        for bytes in [
            &b""[..],
            b"\x1b[?997;3n",
            b"\x1b[?997;n",
            b"\x1b[?997;1",
            b"\x1b[?997;1u",
            b"\x1b[?996n",
            b"\x1b[997;1n",
            b"\x1b[?2026;2$y",
        ] {
            assert_eq!(parse_reports(bytes), Vec::new(), "{bytes:?}");
        }
        // A broken report does not hide the next one.
        assert_eq!(
            parse_reports(b"\x1b[?997;1x\x1b[?997;2n"),
            vec![TerminalReport::Theme(Background::Light)]
        );
    }
}
