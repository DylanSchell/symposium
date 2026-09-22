//! Local telemetry paths and exclusive filesystem access.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the storage foundation is built before the recorder uses it."
    )
)]

mod lock;
mod paths;
