//! Sanitization of peer-supplied text and filenames (PRD §10, §12, §16).

/// Strips control characters, ANSI/escape sequences and bidi controls from
/// peer text. Newlines and tabs are kept (tabs become spaces) so multi-line
/// messages still read naturally.
pub fn text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // ESC: drop the whole escape sequence, not just the ESC byte, so
            // "\x1b[31m" does not leave "[31m" behind.
            '\u{1b}' => skip_escape(&mut chars),
            // C1 CSI is the 8-bit form of "ESC [".
            '\u{9b}' => skip_csi_body(&mut chars),
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\r' => {}
            c if c.is_control() => {}
            c if is_bidi_or_invisible(c) => {}
            c => out.push(c),
        }
    }
    out
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek() {
        Some('[') => {
            chars.next();
            skip_csi_body(chars);
        }
        // OSC / DCS / APC / PM / SOS: run until BEL or ST (ESC \).
        Some(']' | 'P' | '_' | '^' | 'X') => {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == '\u{1b}' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        // Two-character escapes (ESC c, ESC 7, ...).
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn skip_csi_body(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    // Parameter and intermediate bytes are 0x20..=0x3F; the final byte is 0x40..=0x7E.
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) || !('\u{20}'..='\u{3f}').contains(&c) {
            break;
        }
    }
}

/// Bidi overrides/isolates/marks and zero-width characters that can be used to
/// disguise text (e.g. make a filename look like it ends in `.txt`).
fn is_bidi_or_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}'   // ZW space/joiners, LRM, RLM
            | '\u{202a}'..='\u{202e}' // LRE RLE PDF LRO RLO
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI
            | '\u{061c}'              // Arabic letter mark
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

/// Windows reserved device names (case-insensitive, with or without extension).
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

pub const MAX_FILENAME_CHARS: usize = 120;

#[derive(Debug, PartialEq, Eq)]
pub enum FilenameError {
    Empty,
    Traversal,
    Reserved,
}

impl std::fmt::Display for FilenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilenameError::Empty => write!(f, "empty filename"),
            FilenameError::Traversal => write!(f, "filename contains '..'"),
            FilenameError::Reserved => write!(f, "filename is a reserved device name"),
        }
    }
}

impl std::error::Error for FilenameError {}

/// Turns a peer-supplied filename into a safe single path component.
///
/// Path components are stripped (only the last component is kept), `..` is
/// rejected outright, reserved Windows names are rejected, and characters that
/// are invalid on any of the three OSes are replaced by `_`.
pub fn filename(input: &str) -> Result<String, FilenameError> {
    let cleaned = text(input).replace('\n', "");
    if cleaned.split(['/', '\\']).any(|part| part == "..") {
        return Err(FilenameError::Traversal);
    }
    let last = cleaned.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = last
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c => c,
        })
        .collect();
    // Windows silently drops trailing dots/spaces, which can change the extension.
    name = name
        .trim_matches(|c: char| c == ' ' || c == '.')
        .to_string();
    if name.is_empty() {
        return Err(FilenameError::Empty);
    }
    if name.contains("..") && name.chars().all(|c| c == '.') {
        return Err(FilenameError::Traversal);
    }
    let stem = name.split('.').next().unwrap_or("").trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(FilenameError::Reserved);
    }
    if name.chars().count() > MAX_FILENAME_CHARS {
        // Keep the extension so type warnings stay accurate.
        let ext = extension(&name)
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let keep = MAX_FILENAME_CHARS.saturating_sub(ext.chars().count());
        name = name.chars().take(keep).collect::<String>() + &ext;
    }
    Ok(name)
}

fn extension(name: &str) -> Option<String> {
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty() && ext.len() <= 10).then(|| ext.to_ascii_lowercase())
}

const EXECUTABLE_EXTS: &[&str] = &[
    "exe", "msi", "bat", "cmd", "com", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh",
    "scr", "pif", "lnk", "hta", "cpl", "msc", "jar", "reg", "dll", "sh", "bash", "zsh", "command",
    "app", "pkg", "dmg", "run", "bin", "appimage", "deb", "rpm", "py", "pl", "rb", "iso", "docm",
    "xlsm", "pptm",
];

/// True for file types that can run code when double-clicked (PRD §10 warning).
pub fn is_executable(name: &str) -> bool {
    extension(name).is_some_and(|e| EXECUTABLE_EXTS.contains(&e.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_sequences() {
        assert_eq!(text("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(text("a\x1b]0;title\x07b"), "ab");
        assert_eq!(text("a\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\b"), "alinkb");
        assert_eq!(text("x\u{9b}2Jy"), "xy");
        assert_eq!(text("trailing\x1b"), "trailing");
    }

    #[test]
    fn strips_control_and_bidi() {
        assert_eq!(text("a\x00b\x07c\x08d"), "abcd");
        assert_eq!(text("evil\u{202e}txt.exe"), "eviltxt.exe");
        assert_eq!(text("a\u{2066}b\u{2069}c\u{200b}d\u{feff}"), "abcd");
        assert_eq!(text("line1\r\nline2\tx"), "line1\nline2    x");
    }

    #[test]
    fn keeps_normal_unicode() {
        assert_eq!(text("你好，世界 👋"), "你好，世界 👋");
    }

    #[test]
    fn filename_strips_paths() {
        assert_eq!(filename("/etc/passwd").unwrap(), "passwd");
        assert_eq!(filename("C:\\Windows\\evil.dll").unwrap(), "evil.dll");
        assert_eq!(filename("dir/sub/photo.jpg").unwrap(), "photo.jpg");
    }

    #[test]
    fn filename_rejects_traversal() {
        assert_eq!(filename("../../x"), Err(FilenameError::Traversal));
        assert_eq!(filename("..\\x"), Err(FilenameError::Traversal));
        assert_eq!(filename(".."), Err(FilenameError::Traversal));
    }

    #[test]
    fn filename_rejects_reserved_and_empty() {
        for n in ["CON", "nul", "Com1.txt", "lpt9.log", "AUX .txt"] {
            assert_eq!(filename(n), Err(FilenameError::Reserved), "{n}");
        }
        assert_eq!(filename(""), Err(FilenameError::Empty));
        assert_eq!(filename("dir/"), Err(FilenameError::Empty));
        assert_eq!(filename(" . "), Err(FilenameError::Empty));
    }

    #[test]
    fn filename_replaces_invalid_chars_and_bidi() {
        assert_eq!(filename("a:b*c?.txt").unwrap(), "a_b_c_.txt");
        assert_eq!(filename("photo\u{202e}gpj.exe").unwrap(), "photogpj.exe");
        assert_eq!(filename("report.pdf. . ").unwrap(), "report.pdf");
    }

    #[test]
    fn filename_truncates_but_keeps_extension() {
        let long = format!("{}.png", "a".repeat(500));
        let n = filename(&long).unwrap();
        assert!(n.chars().count() <= MAX_FILENAME_CHARS);
        assert!(n.ends_with(".png"));
    }

    #[test]
    fn executable_detection() {
        assert!(is_executable("setup.EXE"));
        assert!(is_executable("run.sh"));
        assert!(is_executable("x.ps1"));
        assert!(!is_executable("photo.jpg"));
        assert!(!is_executable("noext"));
    }
}
