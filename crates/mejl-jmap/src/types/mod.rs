//! JMAP data types.
//!
//! Every object keeps properties it does not model in a flattened `extra`
//! map, so new server fields never break parsing and survive a round trip.

pub mod contact;
pub(crate) mod core;
pub mod email;
pub mod mailbox;
pub mod masked_email;
pub mod submission;

pub use self::contact::{ContactCard, ContactEmail, ContactName, NameComponent};
pub use self::core::{Extra, Id, State, UtcDate};
pub use self::email::{
    Email, EmailAddress, EmailBodyPart, EmailBodyValue, EmailHeader, HeaderForm, SearchSnippet,
    Thread, header_property, keyword,
};
pub use self::mailbox::{Mailbox, MailboxRights, Role};
pub use self::masked_email::{MaskedEmail, MaskedEmailState};
pub use self::submission::{
    DeliveryStatus, EmailSubmission, Envelope, EnvelopeAddress, Identity, UndoStatus,
};
