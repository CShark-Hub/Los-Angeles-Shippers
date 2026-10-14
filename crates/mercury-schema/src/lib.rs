//! Every type that crosses a boundary between Mercury's parts.
//!
//! The promise of this crate: a probe reply has no field that can carry free text. Titles and
//! names travel as [`SafeText`] and [`DisplayName`], which only the [`redact`] module can build
//! and which are checked again whenever they are deserialized. Every other value is an enum, a
//! number, a date or an identifier with a fixed shape. One text goes the other way, into a tool
//! and never out of one: the asker's question for the code reader, as a [`RedactedQuestion`] that
//! only [`redact`] builds too.
//!
//! Beyond the probe's replies: the core's API bodies ([`api`], and [`admin`] for the admin panel's
//! reads), the events of a question's stream ([`events`]), the probe's questions to the core's
//! judge ([`probe_judge`]), the configuration files ([`config`]) and the OpenAPI document that
//! describes the API ([`openapi`]).

pub mod admin;
pub mod api;
pub mod config;
pub mod enums;
pub mod events;
pub mod facts;
pub mod ids;
pub mod manifest;
pub mod openapi;
pub mod probe_judge;
pub mod redact;
pub mod tools;

pub use enums::{AccessLimit, AccessMode, Channel, JudgeMode, QuestionClass, Role, ToolName, Verdict};
pub use events::Event;
pub use facts::Fact;
pub use ids::{
    ApprovalId, CheckId, CommitShort, Email, EventType, FactId, FieldPointer, IssueNumber, JudgeItemId,
    ModelSlug, ModuleName, ProjectId, QuestionId, Sha256Hex, TokenPublicId, UserId,
};
pub use manifest::ProbeManifest;
pub use redact::{DisplayName, RedactedQuestion, Redactor, SafeText};

/// The schema id every probe reply carries.
pub const SCHEMA: &str = "mercury.facts/0.1";
