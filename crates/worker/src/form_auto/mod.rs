//! Server-side automatic form templates (서류 작성 도우미): the worker job's part.
//!
//! The pipeline runs unattended on the production host, one sandboxed
//! container per step (`scripts/forms/server/form-auto.sh`, started by a
//! systemd timer): no container holds the Docker socket, so the steps that
//! parse agency files are launched by the host and this crate does what needs
//! the database or must be checked in Rust:
//!   - `config`     settings from the environment; every flag off by default;
//!   - `disk`       the free-space gate (skip below 22 GB; the 20 GB floor is fixed);
//!   - `candidates` the read-only candidate query (scripts/forms/auto/candidates.sql);
//!   - `outputs`    step outputs read back only as regular, size-capped, valid files;
//!   - `record`     the run summary event and the `job:form_auto` incident.
//!
//! Publication itself is `form-auto-import` in the API image (it reuses the
//! admin services). No user data is involved anywhere: blank official forms only.
pub mod candidates;
pub mod config;
pub mod disk;
pub mod outputs;
pub mod record;

pub use config::{Config, ReviewMode};

/// Incident fingerprint of the job (same path as the scheduled jobs).
pub const INCIDENT: &str = "job:form_auto";
