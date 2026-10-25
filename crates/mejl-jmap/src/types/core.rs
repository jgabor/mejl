//! Primitive JMAP types shared by every object.

use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// Properties a type does not model. Kept so that new server fields survive a
/// round trip and stay readable.
pub type Extra = serde_json::Map<String, serde_json::Value>;

/// A JMAP UTCDate, always `Z` on the wire.
pub type UtcDate = jiff::Timestamp;

/// A JMAP `Id` (RFC 8620 section 1.2).
///
/// No validation is done on parse: an odd id from the server is still an id.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

/// An opaque state string, such as a `/get` `state` or a `queryState`.
///
/// It may be empty: Fastmail reportedly returns `""` for MaskedEmail.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct State(String);

macro_rules! string_newtype {
    ($name:ident) => {
        impl $name {
            /// Wrap a string.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The string value.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Whether the string is empty.
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            /// Unwrap into the inner string.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&String> for $name {
            fn from(value: &String) -> Self {
                Self(value.clone())
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

string_newtype!(Id);
string_newtype!(State);

/// Defines a string enum with a fallback variant for values this crate does
/// not know, so a new server value never breaks parsing.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $value:literal, )*
            @other $other:ident,
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )*
            /// A value this crate does not know, kept as sent.
            $other(String),
        }

        impl $name {
            /// The wire value.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $value, )*
                    Self::$other(value) => value,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $( $value => Self::$variant, )*
                    other => Self::$other(other.to_owned()),
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = <::std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Ok(Self::from(value.as_ref()))
            }
        }
    };
}

pub(crate) use string_enum;

/// Deserializes `null` as the type's default, for arrays and maps that some
/// servers send as `null` where the RFC says empty.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

/// Deserializes an optional UTCDate, dropping values that do not parse
/// instead of failing the whole object. Used where a server is known to be
/// loose with dates.
pub(crate) fn lenient_date<'de, D>(deserializer: D) -> Result<Option<UtcDate>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(s)) => match s.parse::<UtcDate>() {
            Ok(ts) => Some(ts),
            Err(err) => {
                tracing::debug!(value = %s, %err, "ignoring unparsable date");
                None
            }
        },
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    string_enum! {
        enum Colour {
            Red => "red",
            @other Other,
        }
    }

    #[test]
    fn id_is_transparent() {
        let id: Id = serde_json::from_str("\"Mabc\"").unwrap();
        assert_eq!(id, "Mabc");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"Mabc\"");
        let mut map = std::collections::HashMap::new();
        map.insert(id, 1);
        assert_eq!(map.get("Mabc"), Some(&1));
    }

    #[test]
    fn state_may_be_empty() {
        let state: State = serde_json::from_str("\"\"").unwrap();
        assert!(state.is_empty());
    }

    #[test]
    fn string_enum_falls_back() {
        let red: Colour = serde_json::from_str("\"red\"").unwrap();
        assert_eq!(red, Colour::Red);
        let teal: Colour = serde_json::from_str("\"teal\"").unwrap();
        assert_eq!(teal, Colour::Other("teal".into()));
        assert_eq!(serde_json::to_string(&teal).unwrap(), "\"teal\"");
        assert!(serde_json::from_str::<Colour>("3").is_err());
    }

    #[test]
    fn lenient_date_drops_garbage() {
        #[derive(Deserialize)]
        struct T {
            #[serde(default, deserialize_with = "lenient_date")]
            at: Option<UtcDate>,
        }
        let ok: T = serde_json::from_str(r#"{"at":"2026-10-01T09:12:44Z"}"#).unwrap();
        assert!(ok.at.is_some());
        let bad: T = serde_json::from_str(r#"{"at":"yesterday"}"#).unwrap();
        assert!(bad.at.is_none());
        let null: T = serde_json::from_str(r#"{"at":null}"#).unwrap();
        assert!(null.at.is_none());
        let num: T = serde_json::from_str(r#"{"at":5}"#).unwrap();
        assert!(num.at.is_none());
        let missing: T = serde_json::from_str("{}").unwrap();
        assert!(missing.at.is_none());
    }
}
