//! Immutable public intent for local, durable message-device operations.

use bincode::{Decode, Encode};
use rostra_core::event::VerifiedEvent;

/// Caller-generated random identifier, persisted with the exact request before
/// use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct DmDeviceRequestId(pub [u8; 16]);

/// Exact local installation state that permits explicit enrollment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum DmEnrollmentExpectation {
    /// No installation has been created in this database.
    Absent,
    /// This exact installation has permanently retired.
    Retired {
        /// Installation incarnation, not a label or epoch public key.
        device_id: [u8; 16],
    },
}

/// An immutable lifecycle intention; never retarget a pending request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum DmDeviceRequest {
    /// Retire this known, nonretired own device, regardless of epoch rotation.
    Retire {
        /// Exact account-scoped device identifier.
        device_id: [u8; 16],
    },
    /// Create a fresh local device and epoch only from the expected
    /// incarnation.
    Reenroll {
        /// Exact state observed before requesting enrollment.
        expected: DmEnrollmentExpectation,
    },
}

/// Original locally accepted outcome, not a statement of current device state.
pub struct DmDeviceOutcome {
    /// Retired device or newly enrolled installation.
    pub device_id: [u8; 16],
    /// Original signed announcement; acceptance does not imply remote delivery.
    pub event: VerifiedEvent,
}
