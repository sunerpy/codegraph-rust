pub mod connection;
mod file_identity;
pub mod index_lease;
pub mod index_state;
pub mod index_state_publisher;
pub mod migrations;
pub mod queries;
pub mod rebuild;
pub mod schema;

pub use connection::{
    ExtractionStampIssue, Store, StoreError, StoreStatusOpen, StoreWriteAuthorization,
    StoreWriteOpen, StoreWritePurpose,
};
pub use index_lease::{IndexLease, IndexLeaseError, IndexLeaseValidationError};
pub use index_state::{
    AuthoritativeSlot, CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, CorruptReason,
    EXTRACTION_VERSION_KEY, ExtractionStatus, IndexStateClassification, SlotOutcome, StatePhase,
    StateSlotRecord, canonical_checksum_payload, checksum_hex, classify, classify_slots,
};
pub use index_state_publisher::{
    ParentSyncStatus, PublishedState, StatePublishError, publish_index_state,
};
pub use queries::{
    CODEGRAPH_NO_WAL_DEFER, CODEGRAPH_WAL_VALVE_MB, DEFAULT_WAL_VALVE_MB, wal_valve_threshold_bytes,
};
pub use rebuild::{
    ActiveFullRebuild, FullRebuild, RebuildError, RebuildKind, begin_full_rebuild,
    resume_full_rebuild,
};
