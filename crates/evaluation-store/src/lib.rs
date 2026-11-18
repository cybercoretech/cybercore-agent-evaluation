//! Local persistence for the CyberCore Agent Evaluation Workbench (ADR 0002).
//!
//! - [`SqliteMetadataStore`]: append-only, contract-validated records with a
//!   reference index and idempotency keys.
//! - [`FsArtifactStore`]: content-addressed evidence objects behind the
//!   [`ArtifactStore`] port.
//! - [`LocalStore`]: both together, with the pending → available/failed
//!   evidence write protocol and crash reconciliation.
//!
//! The evaluation core does not depend on this crate; adapters depend on the
//! core's records, not the other way round. The SQL in `migrations/` is plain
//! SQLite so a Cloudflare D1 adapter can apply the same schema.

mod artifact;
mod canonical;
mod contracts;
mod embedded_schemas;
mod error;
mod local;
mod metadata;

pub use artifact::{ArtifactStore, FsArtifactStore};
pub use canonical::{canonical_json, sha256_hex};
pub use contracts::ContractValidator;
pub use error::{StoreError, StoreResult};
pub use local::{LocalStore, ReconcileReport};
pub use metadata::{PutOutcome, RecordRef, SqliteMetadataStore, UploadState};
