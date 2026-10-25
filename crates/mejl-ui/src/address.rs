//! Parsing and checking addresses typed or pasted into compose.
//!
//! The rules are deliberately practical rather than full RFC 5322: a
//! recipient is `name <local@domain>` or `local@domain`, the domain has a
//! dot and only letters, digits, hyphens and dots, and nothing contains
//! spaces or control characters. Anything else stays in the field with a
//! message saying what is wrong, so a typo never silently disappears.

use crate::data::Participant;

/// Why an address was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressError {
    /// Nothing was typed.
    Empty,
    /// No `@`.
    MissingAt,
    /// Nothing before the `@`.
    MissingLocal,
    /// The part after the `@` is not a domain name.
    BadDomain,
    /// Spaces, control characters or unbalanced brackets.
    BadCharacters,
}

impl std::fmt::Display for AddressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AddressError::Empty => "the address is empty",
            AddressError::MissingAt => "an address needs an @",
            AddressError::MissingLocal => "nothing before the @",
            AddressError::BadDomain => "the part after the @ is not a domain name",
            AddressError::BadCharacters => "the address has spaces or characters it cannot have",
        })
    }
}

/// True for a plausible `local@domain` address.
pub fn check_email(email: &str) -> Result<(), AddressError> {
    if email.is_empty() {
        return Err(AddressError::Empty);
    }
    if email
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"'))
    {
        return Err(AddressError::BadCharacters);
    }
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err(AddressError::MissingAt);
    };
    if local.is_empty() {
        return Err(AddressError::MissingLocal);
    }
    let labels: Vec<&str> = domain.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_alphanumeric() || c == '-')
    };
    if labels.len() < 2 || !labels.iter().all(label_ok) {
        return Err(AddressError::BadDomain);
    }
    Ok(())
}

/// Parses `Name <local@domain>`, `"Name" <local@domain>`, `<local@domain>`
/// or `local@domain`.
pub fn parse(text: &str) -> Result<Participant, AddressError> {
    let t = text.trim();
    if t.is_empty() {
        return Err(AddressError::Empty);
    }
    if let Some(open) = t.rfind('<') {
        let Some(rest) = t[open + 1..].strip_suffix('>') else {
            return Err(AddressError::BadCharacters);
        };
        let email = rest.trim();
        check_email(email)?;
        let name = t[..open].trim().trim_matches('"').trim();
        if name.chars().any(char::is_control) {
            return Err(AddressError::BadCharacters);
        }
        return Ok(Participant {
            name: (!name.is_empty()).then(|| name.to_owned()),
            email: email.to_owned(),
        });
    }
    if t.contains('>') {
        return Err(AddressError::BadCharacters);
    }
    check_email(t)?;
    Ok(Participant {
        name: None,
        email: t.to_owned(),
    })
}

/// Splits pasted or typed text into address candidates at commas,
/// semicolons and line breaks, ignoring separators inside quotes or angle
/// brackets.
pub fn split(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut angle = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '<' if !quoted => {
                angle = true;
                cur.push(c);
            }
            '>' if !quoted => {
                angle = false;
                cur.push(c);
            }
            ',' | ';' | '\n' | '\r' if !quoted && !angle => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_owned());
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// `Name <email>`, or the bare address when there is no name.
pub fn format(p: &Participant) -> String {
    match &p.name {
        Some(n) if !n.trim().is_empty() => format!("{} <{}>", n.trim(), p.email),
        _ => p.email.clone(),
    }
}

/// The domain of an address, lowercased, if it has one.
pub fn domain(email: &str) -> Option<String> {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.to_lowercase())
        .filter(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_forms() {
        let p = parse("Juniper Tran <juniper@garden.example.com>").unwrap();
        assert_eq!(p.name.as_deref(), Some("Juniper Tran"));
        assert_eq!(p.email, "juniper@garden.example.com");
        let p = parse("\"Tran, Juniper\" <j@example.com>").unwrap();
        assert_eq!(p.name.as_deref(), Some("Tran, Juniper"));
        let p = parse("  bookings@cabins.example.net ").unwrap();
        assert_eq!(p.name, None);
        assert_eq!(parse("<a@example.org>").unwrap().name, None);
        assert!(parse("ünï@exämple.org").is_ok(), "international names");
    }

    #[test]
    fn refuses_with_a_reason() {
        assert_eq!(parse(""), Err(AddressError::Empty));
        assert_eq!(parse("juniper"), Err(AddressError::MissingAt));
        assert_eq!(parse("@example.com"), Err(AddressError::MissingLocal));
        assert_eq!(parse("a@localhost"), Err(AddressError::BadDomain));
        assert_eq!(parse("a@-x.example"), Err(AddressError::BadDomain));
        assert_eq!(parse("a@x..example"), Err(AddressError::BadDomain));
        assert_eq!(parse("a b@example.com"), Err(AddressError::BadCharacters));
        assert_eq!(
            parse("Ada <a@example.com"),
            Err(AddressError::BadCharacters)
        );
        assert_eq!(parse("a@example.com>"), Err(AddressError::BadCharacters));
        assert!(!AddressError::BadDomain.to_string().is_empty());
    }

    #[test]
    fn splits_lists() {
        assert_eq!(
            split("a@example.com, \"Tran, J\" <j@example.com>;\nb@example.org,,"),
            vec![
                "a@example.com",
                "\"Tran, J\" <j@example.com>",
                "b@example.org"
            ]
        );
        assert!(split(" , ;").is_empty());
    }

    #[test]
    fn formats_and_domains() {
        let p = Participant::new("Mira Holt", "mira@example.org");
        assert_eq!(format(&p), "Mira Holt <mira@example.org>");
        assert_eq!(
            format(&Participant {
                name: None,
                email: "x@example.org".into()
            }),
            "x@example.org"
        );
        assert_eq!(
            domain("A@Shop.Example.com").as_deref(),
            Some("shop.example.com")
        );
        assert_eq!(domain("nodomain"), None);
    }
}
