//! A small Sieve parser and checker for mejl's own block.
//!
//! This is not a full Sieve implementation. It tokenizes and parses the
//! generic RFC 5228 grammar, then checks each command, test and tag against
//! the extensions mejl may rely on. It exists because no permissive Sieve
//! crate is proven, and sieve-rs is AGPL and accepts unknown commands.
//!
//! What it checks:
//!
//! - The text parses: strings, comments, brackets and blocks are closed.
//! - Every command, test and tag is one mejl knows, and the extension it
//!   needs is in [`ALLOWED_EXTENSIONS`].
//! - No `require`, `keep`, `discard`, `reject`, `redirect` or `vacation`.

use std::collections::BTreeSet;
use std::fmt;

/// The require line Fastmail's help page shows at the top of the generated
/// script, as of 2026-10-23 ("Using Sieve scripts in Fastmail").
pub const FASTMAIL_REQUIRE_LINE: &[&str] = &[
    "fileinto",
    "reject",
    "vacation",
    "envelope",
    "body",
    "relational",
    "regex",
    "subaddress",
    "copy",
    "mailbox",
    "mboxmetadata",
    "servermetadata",
    "date",
    "index",
    "comparator-i;ascii-numeric",
    "variables",
    "imap4flags",
    "editheader",
    "duplicate",
    "vacation-seconds",
    "fcc",
    "vnd.cyrus.jmapquery",
    "vnd.cyrus.log",
    "mailboxid",
    "special-use",
    "vnd.cyrus.snooze",
    "vnd.cyrus.imip",
    "vnd.cyrus.implicit_keep_target",
];

/// Extensions mejl may rely on: those in both the help page require line and
/// a 2025 sample of a real script. The sample has `processcalendar` where the
/// help page has `vnd.cyrus.imip`, so neither of those is allowed. Nothing
/// dates either line, so mejl only uses what both agree on.
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    "fileinto",
    "reject",
    "vacation",
    "envelope",
    "body",
    "relational",
    "regex",
    "subaddress",
    "copy",
    "mailbox",
    "mboxmetadata",
    "servermetadata",
    "date",
    "index",
    "comparator-i;ascii-numeric",
    "variables",
    "imap4flags",
    "editheader",
    "duplicate",
    "vacation-seconds",
    "fcc",
    "vnd.cyrus.jmapquery",
    "vnd.cyrus.log",
    "mailboxid",
    "special-use",
    "vnd.cyrus.snooze",
    "vnd.cyrus.implicit_keep_target",
];

/// Commands mejl's block must never contain, even where Fastmail allows
/// them. See the [`model`](crate::model) docs for the reasons.
pub const FORBIDDEN_COMMANDS: &[&str] = &[
    "require", "keep", "discard", "reject", "ereject", "redirect", "vacation",
];

/// Result of checking a script.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptCheck {
    /// Extensions the script needs, sorted.
    pub extensions: BTreeSet<&'static str>,
    /// Command names in the order they appear, nested ones included.
    pub commands: Vec<String>,
    /// Problems found, in text order. Empty when the script is fine.
    pub issues: Vec<ScriptIssue>,
}

impl ScriptCheck {
    /// True when no issue was found.
    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// One problem in a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptIssue {
    /// One-based line number.
    pub line: usize,
    /// What kind of problem.
    pub kind: ScriptIssueKind,
}

impl fmt::Display for ScriptIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}

/// Kinds of script problems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptIssueKind {
    /// The text does not parse.
    Syntax(String),
    /// A command mejl never emits, such as `keep`.
    ForbiddenCommand(String),
    /// A command, test or tag the checker does not know.
    Unknown(String),
    /// A known item whose extension is not in [`ALLOWED_EXTENSIONS`].
    ExtensionNotAllowed {
        /// The command, test or tag.
        item: String,
        /// The extension it needs.
        extension: &'static str,
    },
}

impl fmt::Display for ScriptIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptIssueKind::Syntax(msg) => write!(f, "syntax error: {msg}"),
            ScriptIssueKind::ForbiddenCommand(name) => {
                write!(f, "mejl never emits the {name:?} command")
            }
            ScriptIssueKind::Unknown(name) => write!(f, "unknown command, test or tag {name:?}"),
            ScriptIssueKind::ExtensionNotAllowed { item, extension } => {
                write!(
                    f,
                    "{item:?} needs the {extension:?} extension, which mejl may not use"
                )
            }
        }
    }
}

/// Parses and checks Sieve text against mejl's rules for its own block.
///
/// Meant for mejl's rendered block. A full Fastmail script fails on purpose:
/// it starts with `require` and its generated code uses commands mejl does
/// not emit, such as `set` and `string` from the variables pipeline.
pub fn check_script(text: &str) -> ScriptCheck {
    let mut check = ScriptCheck::default();
    let tokens = match tokenize(text) {
        Ok(tokens) => tokens,
        Err((line, msg)) => {
            check.issues.push(ScriptIssue {
                line,
                kind: ScriptIssueKind::Syntax(msg),
            });
            return check;
        }
    };
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        depth: 0,
    };
    let commands = match parser.commands(true) {
        Ok(commands) => commands,
        Err((line, msg)) => {
            check.issues.push(ScriptIssue {
                line,
                kind: ScriptIssueKind::Syntax(msg),
            });
            return check;
        }
    };
    check_commands(&commands, &mut check);
    check.issues.sort_by_key(|issue| issue.line);
    check
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Tag(String),
    Number,
    Str(String),
    Punct(char),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: usize,
}

type ParseError = (usize, String);

fn tokenize(text: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while let Some(&c) = chars.get(i) {
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            ' ' | '\t' | '\r' => i += 1,
            '#' => {
                while chars.get(i).is_some_and(|&c| c != '\n') {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let start = line;
                i += 2;
                loop {
                    match chars.get(i) {
                        None => return Err((start, "unterminated comment".into())),
                        Some('*') if chars.get(i + 1) == Some(&'/') => {
                            i += 2;
                            break;
                        }
                        Some('\n') => {
                            line += 1;
                            i += 1;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '"' => {
                let start = line;
                let mut value = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err((start, "unterminated string".into())),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            // RFC 5228: a backslash keeps the next character.
                            match chars.get(i + 1) {
                                None => return Err((start, "unterminated string".into())),
                                Some(&next) => {
                                    if next == '\n' {
                                        line += 1;
                                    }
                                    value.push(next);
                                    i += 2;
                                }
                            }
                        }
                        Some(&other) => {
                            if other == '\n' {
                                line += 1;
                            }
                            value.push(other);
                            i += 1;
                        }
                    }
                }
                tokens.push(Token {
                    tok: Tok::Str(value),
                    line: start,
                });
            }
            ';' | ',' | '(' | ')' | '[' | ']' | '{' | '}' => {
                tokens.push(Token {
                    tok: Tok::Punct(c),
                    line,
                });
                i += 1;
            }
            ':' => {
                let start = i + 1;
                let mut end = start;
                while chars.get(end).is_some_and(|&c| is_ident_char(c)) {
                    end += 1;
                }
                if end == start || !(chars[start].is_ascii_alphabetic() || chars[start] == '_') {
                    return Err((line, "tag without a name".into()));
                }
                tokens.push(Token {
                    tok: Tok::Tag(
                        chars[start..end]
                            .iter()
                            .collect::<String>()
                            .to_ascii_lowercase(),
                    ),
                    line,
                });
                i = end;
            }
            '0'..='9' => {
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
                if chars
                    .get(i)
                    .is_some_and(|c| matches!(c, 'K' | 'M' | 'G' | 'k' | 'm' | 'g'))
                {
                    i += 1;
                }
                tokens.push(Token {
                    tok: Tok::Number,
                    line,
                });
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while chars.get(i).is_some_and(|&c| is_ident_char(c)) {
                    i += 1;
                }
                let word: String = chars[start..i]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase();
                if word == "text" && chars.get(i) == Some(&':') {
                    // A multi-line string. mejl never writes one, but a
                    // pasted section may hold one, so read it to its end.
                    let start_line = line;
                    i += 1;
                    let mut value = String::new();
                    while chars.get(i).is_some_and(|&c| c != '\n') {
                        i += 1;
                    }
                    loop {
                        if chars.get(i).is_none() {
                            return Err((start_line, "unterminated multi-line string".into()));
                        }
                        // Skip the newline that ends the previous line.
                        i += 1;
                        line += 1;
                        let line_start = i;
                        while chars.get(i).is_some_and(|&c| c != '\n') {
                            i += 1;
                        }
                        let content: String = chars[line_start..i].iter().collect();
                        let content = content.strip_suffix('\r').unwrap_or(&content);
                        if content == "." {
                            break;
                        }
                        let content = content.strip_prefix('.').unwrap_or(content);
                        value.push_str(content);
                        value.push('\n');
                    }
                    tokens.push(Token {
                        tok: Tok::Str(value),
                        line: start_line,
                    });
                } else {
                    tokens.push(Token {
                        tok: Tok::Ident(word),
                        line,
                    });
                }
            }
            other => return Err((line, format!("unexpected character {other:?}"))),
        }
    }
    Ok(tokens)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[derive(Debug)]
enum Arg {
    Tag(String, usize),
    Strings(Vec<String>),
    Number,
}

#[derive(Debug)]
struct TestNode {
    name: String,
    line: usize,
    args: Vec<Arg>,
    tests: Vec<TestNode>,
}

#[derive(Debug)]
struct Command {
    name: String,
    line: usize,
    args: Vec<Arg>,
    tests: Vec<TestNode>,
    block: Option<Vec<Command>>,
}

/// Nesting limit, so hostile input cannot overflow the stack.
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|t| &t.tok)
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map_or(1, |t| t.line)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), ParseError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err((self.line(), format!("expected {c:?}")))
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err((self.line(), "nested too deeply".into()))
        } else {
            Ok(())
        }
    }

    fn commands(&mut self, top: bool) -> Result<Vec<Command>, ParseError> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None if top => return Ok(out),
                None => return Err((self.line(), "expected '}'".into())),
                Some(Tok::Punct('}')) if !top => return Ok(out),
                _ => out.push(self.command()?),
            }
        }
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        let line = self.line();
        let Some(Tok::Ident(name)) = self.peek().cloned() else {
            return Err((line, "expected a command".into()));
        };
        self.pos += 1;
        let (args, tests) = self.arguments()?;
        let block = if self.eat('{') {
            self.enter()?;
            let inner = self.commands(false)?;
            self.expect('}')?;
            self.depth -= 1;
            Some(inner)
        } else {
            self.expect(';')?;
            None
        };
        Ok(Command {
            name,
            line,
            args,
            tests,
            block,
        })
    }

    fn arguments(&mut self) -> Result<(Vec<Arg>, Vec<TestNode>), ParseError> {
        let mut args = Vec::new();
        loop {
            let line = self.line();
            match self.peek().cloned() {
                Some(Tok::Tag(tag)) => {
                    self.pos += 1;
                    args.push(Arg::Tag(tag, line));
                }
                Some(Tok::Number) => {
                    self.pos += 1;
                    args.push(Arg::Number);
                }
                Some(Tok::Str(s)) => {
                    self.pos += 1;
                    args.push(Arg::Strings(vec![s]));
                }
                Some(Tok::Punct('[')) => {
                    self.pos += 1;
                    args.push(Arg::Strings(self.string_list()?));
                }
                _ => break,
            }
        }
        let tests = match self.peek() {
            Some(Tok::Ident(_)) => vec![self.test()?],
            Some(Tok::Punct('(')) => self.test_list()?,
            _ => Vec::new(),
        };
        Ok((args, tests))
    }

    fn string_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut out = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Tok::Str(s)) => {
                    self.pos += 1;
                    out.push(s);
                }
                _ => return Err((self.line(), "expected a string".into())),
            }
            if self.eat(']') {
                return Ok(out);
            }
            self.expect(',')?;
        }
    }

    fn test(&mut self) -> Result<TestNode, ParseError> {
        let line = self.line();
        let Some(Tok::Ident(name)) = self.peek().cloned() else {
            return Err((line, "expected a test".into()));
        };
        self.pos += 1;
        self.enter()?;
        let (args, tests) = self.arguments()?;
        self.depth -= 1;
        Ok(TestNode {
            name,
            line,
            args,
            tests,
        })
    }

    fn test_list(&mut self) -> Result<Vec<TestNode>, ParseError> {
        self.expect('(')?;
        let mut out = vec![self.test()?];
        while self.eat(',') {
            out.push(self.test()?);
        }
        self.expect(')')?;
        Ok(out)
    }
}

/// The extension a command needs, `Some(None)` for core commands, `None`
/// when unknown.
fn command_extension(name: &str) -> Option<Option<&'static str>> {
    Some(match name {
        "if" | "elsif" | "else" | "stop" => None,
        "fileinto" => Some("fileinto"),
        "addflag" | "setflag" | "removeflag" => Some("imap4flags"),
        "set" => Some("variables"),
        "snooze" => Some("vnd.cyrus.snooze"),
        "implicit_keep_target" => Some("vnd.cyrus.implicit_keep_target"),
        "processcalendar" => Some("processcalendar"),
        "addheader" | "deleteheader" => Some("editheader"),
        "log" => Some("vnd.cyrus.log"),
        _ => return None,
    })
}

fn test_extension(name: &str) -> Option<Option<&'static str>> {
    Some(match name {
        "address" | "allof" | "anyof" | "exists" | "false" | "header" | "not" | "size" | "true" => {
            None
        }
        "envelope" => Some("envelope"),
        "body" => Some("body"),
        "hasflag" => Some("imap4flags"),
        "string" => Some("variables"),
        "date" | "currentdate" => Some("date"),
        "duplicate" => Some("duplicate"),
        "mailboxexists" => Some("mailbox"),
        "specialuse_exists" => Some("special-use"),
        "mailboxidexists" => Some("mailboxid"),
        "jmapquery" => Some("vnd.cyrus.jmapquery"),
        _ => return None,
    })
}

fn tag_extension(tag: &str) -> Option<Option<&'static str>> {
    Some(match tag {
        "is" | "contains" | "matches" | "all" | "localpart" | "domain" | "over" | "under"
        | "comparator" => None,
        "copy" => Some("copy"),
        "mailboxid" => Some("mailboxid"),
        "specialuse" => Some("special-use"),
        "create" => Some("mailbox"),
        "flags" => Some("imap4flags"),
        "regex" => Some("regex"),
        "value" | "count" => Some("relational"),
        "user" | "detail" => Some("subaddress"),
        "index" | "last" => Some("index"),
        _ => return None,
    })
}

fn comparator_extension(name: &str) -> Option<Option<&'static str>> {
    match name.to_ascii_lowercase().as_str() {
        "i;ascii-casemap" | "i;octet" => Some(None),
        "i;ascii-numeric" => Some(Some("comparator-i;ascii-numeric")),
        _ => None,
    }
}

fn need(check: &mut ScriptCheck, line: usize, item: &str, extension: Option<&'static str>) {
    let Some(extension) = extension else {
        return;
    };
    if ALLOWED_EXTENSIONS.contains(&extension) {
        check.extensions.insert(extension);
    } else {
        check.issues.push(ScriptIssue {
            line,
            kind: ScriptIssueKind::ExtensionNotAllowed {
                item: item.to_owned(),
                extension,
            },
        });
    }
}

fn check_args(args: &[Arg], check: &mut ScriptCheck) {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let Arg::Tag(tag, line) = arg else {
            continue;
        };
        match tag_extension(tag) {
            Some(ext) => need(check, *line, &format!(":{tag}"), ext),
            None => check.issues.push(ScriptIssue {
                line: *line,
                kind: ScriptIssueKind::Unknown(format!(":{tag}")),
            }),
        }
        if tag == "comparator" {
            match iter.peek() {
                Some(Arg::Strings(names)) if names.len() == 1 => {
                    match comparator_extension(&names[0]) {
                        Some(ext) => need(check, *line, &names[0], ext),
                        None => check.issues.push(ScriptIssue {
                            line: *line,
                            kind: ScriptIssueKind::Unknown(names[0].clone()),
                        }),
                    }
                }
                _ => check.issues.push(ScriptIssue {
                    line: *line,
                    kind: ScriptIssueKind::Syntax(":comparator needs one string".into()),
                }),
            }
        }
    }
}

fn check_tests(tests: &[TestNode], check: &mut ScriptCheck) {
    for test in tests {
        match test_extension(&test.name) {
            Some(ext) => need(check, test.line, &test.name, ext),
            None => check.issues.push(ScriptIssue {
                line: test.line,
                kind: ScriptIssueKind::Unknown(test.name.clone()),
            }),
        }
        check_args(&test.args, check);
        check_tests(&test.tests, check);
    }
}

fn check_commands(commands: &[Command], check: &mut ScriptCheck) {
    let mut previous_was_if = false;
    for command in commands {
        check.commands.push(command.name.clone());
        let name = command.name.as_str();
        let syntax = |msg: &str| ScriptIssue {
            line: command.line,
            kind: ScriptIssueKind::Syntax(msg.to_owned()),
        };
        if FORBIDDEN_COMMANDS.contains(&name) {
            check.issues.push(ScriptIssue {
                line: command.line,
                kind: ScriptIssueKind::ForbiddenCommand(command.name.clone()),
            });
        } else {
            match command_extension(name) {
                Some(ext) => need(check, command.line, name, ext),
                None => check.issues.push(ScriptIssue {
                    line: command.line,
                    kind: ScriptIssueKind::Unknown(command.name.clone()),
                }),
            }
        }
        match name {
            "if" | "elsif" => {
                if command.tests.len() != 1 {
                    check
                        .issues
                        .push(syntax(&format!("{name} needs exactly one test")));
                }
                if command.block.is_none() {
                    check.issues.push(syntax(&format!("{name} needs a block")));
                }
                if name == "elsif" && !previous_was_if {
                    check.issues.push(syntax("elsif without if"));
                }
            }
            "else" => {
                if !command.tests.is_empty() || !command.args.is_empty() {
                    check.issues.push(syntax("else takes no arguments"));
                }
                if command.block.is_none() {
                    check.issues.push(syntax("else needs a block"));
                }
                if !previous_was_if {
                    check.issues.push(syntax("else without if"));
                }
            }
            _ => {
                if command.block.is_some() {
                    check
                        .issues
                        .push(syntax(&format!("{name} does not take a block")));
                }
                if !command.tests.is_empty() {
                    check
                        .issues
                        .push(syntax(&format!("{name} does not take a test")));
                }
            }
        }
        previous_was_if = matches!(name, "if" | "elsif");
        check_args(&command.args, check);
        check_tests(&command.tests, check);
        if let Some(block) = &command.block {
            check_commands(block, check);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<ScriptIssueKind> {
        check_script(text)
            .issues
            .into_iter()
            .map(|i| i.kind)
            .collect()
    }

    #[test]
    fn allowlist_is_the_help_line_minus_imip() {
        let expected: Vec<&str> = FASTMAIL_REQUIRE_LINE
            .iter()
            .copied()
            .filter(|e| *e != "vnd.cyrus.imip")
            .collect();
        assert_eq!(ALLOWED_EXTENSIONS, expected.as_slice());
        assert!(!ALLOWED_EXTENSIONS.contains(&"processcalendar"));
        assert!(!ALLOWED_EXTENSIONS.contains(&"notify"));
    }

    #[test]
    fn accepts_the_mockup_shapes() {
        let text = r#"
# comment
if anyof (address :domain :is "from" "shop.example.com",
          header :contains "subject" "receipt") {
  addflag "\\Seen";
  fileinto :mailboxid "M7d1e" "INBOX.Receipts";
}
if false # address :domain :is "from" "promo.example.com"
{
  fileinto :specialuse "\\Trash" "INBOX.Trash";
}
if not size :over 10M { fileinto :copy :mailboxid "M1" "INBOX.x"; stop; }
"#;
        let check = check_script(text);
        assert!(check.is_ok(), "{:?}", check.issues);
        let exts: Vec<&str> = check.extensions.iter().copied().collect();
        assert_eq!(
            exts,
            ["copy", "fileinto", "imap4flags", "mailboxid", "special-use"]
        );
        assert_eq!(
            check.commands,
            [
                "if", "addflag", "fileinto", "if", "fileinto", "if", "fileinto", "stop"
            ]
        );
    }

    #[test]
    fn forbidden_commands_are_reported() {
        for cmd in [
            "keep;",
            "discard;",
            "require \"fileinto\";",
            "reject \"no\";",
        ] {
            let found = kinds(cmd);
            assert!(
                matches!(found.as_slice(), [ScriptIssueKind::ForbiddenCommand(_)]),
                "{cmd}: {found:?}"
            );
        }
        let found = kinds("if true { KEEP; }");
        assert_eq!(found, [ScriptIssueKind::ForbiddenCommand("keep".into())]);
        let found = kinds("redirect :copy \"a@example.com\";");
        assert!(found.contains(&ScriptIssueKind::ForbiddenCommand("redirect".into())));
    }

    #[test]
    fn words_inside_strings_and_comments_are_not_commands() {
        let check = check_script(
            "# keep; discard; require\nif header :is \"subject\" \"keep; discard;\" { stop; }\n/* keep; */",
        );
        assert!(check.is_ok(), "{:?}", check.issues);
        assert_eq!(check.commands, ["if", "stop"]);
    }

    #[test]
    fn extensions_outside_the_allowlist_are_reported() {
        let found = kinds("processcalendar;");
        assert_eq!(
            found,
            [ScriptIssueKind::ExtensionNotAllowed {
                item: "processcalendar".into(),
                extension: "processcalendar"
            }]
        );
        let found = kinds("if frobnicate \"x\" { stop; }");
        assert_eq!(found, [ScriptIssueKind::Unknown("frobnicate".into())]);
        let found = kinds("fileinto :bogus \"x\";");
        assert_eq!(found, [ScriptIssueKind::Unknown(":bogus".into())]);
        let found = kinds("if header :comparator \"i;unicode-casemap\" :is \"a\" \"b\" { stop; }");
        assert_eq!(
            found,
            [ScriptIssueKind::Unknown("i;unicode-casemap".into())]
        );
        let check = check_script(
            "if header :comparator \"i;ascii-numeric\" :value \"gt\" \"x-spam-score\" \"5\" { stop; }",
        );
        assert!(check.is_ok(), "{:?}", check.issues);
        assert!(check.extensions.contains("comparator-i;ascii-numeric"));
        assert!(check.extensions.contains("relational"));
    }

    #[test]
    fn syntax_errors_carry_line_numbers() {
        let cases = [
            ("if true {\n stop;\n", 2, "expected '}'"),
            ("stop", 1, "expected ';'"),
            ("\n\"open", 2, "unterminated string"),
            ("/* open", 1, "unterminated comment"),
            ("if true stop;", 1, "if needs a block"),
            ("if { stop; }", 1, "if needs exactly one test"),
            ("else { stop; }", 1, "else without if"),
            ("stop { }", 1, "stop does not take a block"),
            ("fileinto [\"a\",];", 1, "expected a string"),
            ("@", 1, "unexpected character '@'"),
            ("if anyof () { }", 1, "expected a test"),
            ("x: ;", 1, "tag without a name"),
            ("fileinto :9 \"x\";", 1, "tag without a name"),
        ];
        for (text, line, msg) in cases {
            let issues = check_script(text).issues;
            let found = issues
                .iter()
                .find(|i| matches!(&i.kind, ScriptIssueKind::Syntax(m) if m.contains(msg)));
            let Some(issue) = found else {
                panic!("{text:?}: expected {msg:?}, got {issues:?}");
            };
            assert_eq!(issue.line, line, "{text:?}");
        }
    }

    #[test]
    fn multi_line_strings_are_read() {
        let text = "if true {\n  addflag text:\n\\Seen\n..dot\n.\n;\n}\n";
        let check = check_script(text);
        assert!(check.is_ok(), "{:?}", check.issues);
        assert!(check_script("addflag text:\nno end").issues[0].line == 1);
    }

    #[test]
    fn deep_nesting_is_refused_without_overflow() {
        let text = format!("if {}true {{ }}", "not ".repeat(10_000));
        let issues = check_script(&text).issues;
        assert!(matches!(&issues[0].kind, ScriptIssueKind::Syntax(m) if m.contains("deeply")));
        let text = format!("{}{}", "if true {".repeat(10_000), "}".repeat(10_000));
        let issues = check_script(&text).issues;
        assert!(matches!(&issues[0].kind, ScriptIssueKind::Syntax(m) if m.contains("deeply")));
    }

    #[test]
    fn empty_text_is_fine() {
        assert!(check_script("").is_ok());
        assert!(check_script("# only a comment\n").is_ok());
    }
}
