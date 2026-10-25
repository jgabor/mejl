//! mejl's rules: model, Sieve renderer, validation, paste-back check and a
//! local dry-run matcher.
//!
//! Fastmail has no public API for rules or Sieve, and no ManageSieve. So mejl
//! owns its rules and renders them into one marked Sieve block. The user
//! pastes that block into one custom section of Fastmail's script, by default
//! the one directly below spam protection. To check what is on the server,
//! the user pastes the section back and mejl compares markers and hashes.
//!
//! The pieces:
//!
//! - [`model`]: [`Rule`] and its conditions and actions, with stable JSON.
//! - [`render`]: [`render_block`] turns rules into the marked block.
//! - [`script`]: [`check_script`] parses Sieve text and checks it against
//!   the extensions mejl may use and the commands it must never emit.
//! - [`validate`](mod@validate): [`validate`](fn@validate) returns errors and lints per
//!   rule.
//! - [`paste`]: [`check_paste`] finds the block in pasted text and reports
//!   drift.
//! - [`matcher`]: [`evaluate`] and [`evaluate_all`] run rules against
//!   [`MessageFacts`] from cached mail, with Sieve semantics.
//! - [`suggest`]: [`rule_from_message`] for "new rule from this message".
//! - [`summary`]: [`summarize`] for the rules list.
//!
//! The crate does no I/O and never touches the network.

#![warn(missing_docs)]

mod lower;
pub mod matcher;
pub mod model;
pub mod paste;
pub mod render;
pub mod script;
pub mod suggest;
pub mod summary;
pub mod validate;

pub use matcher::{
    Delivery, DeliveryTarget, EmailAddress, Evaluation, MessageFacts, PlannedAction, RuleMatch,
    evaluate, evaluate_all,
};
pub use model::{
    Action, AddressHeader, AddressPart, Condition, Flag, MailboxRef, MatchMode, Rule, RuleId, Test,
    TextOp,
};
pub use paste::{FoundBlock, FoundRule, PasteReport, PasteStatus, check_paste};
pub use render::{RenderError, RenderErrorKind, RenderedBlock, RenderedRule, render_block};
pub use script::{ALLOWED_EXTENSIONS, ScriptCheck, ScriptIssue, ScriptIssueKind, check_script};
pub use suggest::{SuggestionKind, rule_from_message};
pub use summary::summarize;
pub use validate::{Diagnostic, DiagnosticCode, Severity, ValidationContext, has_errors, validate};
