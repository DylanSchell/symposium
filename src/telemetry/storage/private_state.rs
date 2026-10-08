//! Bounded, privacy-preserving I/O for telemetry-state.toml.

use std::{
    error::Error,
    fmt,
    fs::File,
    io::{self, Read as _},
    path::Path,
};

use super::{
    LockedStorage,
    atomic::{self, AtomicReplaceError},
};
use crate::telemetry::state::{
    StateContentError, StateDecodeError, TelemetryStateV1, decode, encode,
};

/// Safety ceiling for private state read into one recorder process.
///
/// The aggregate-backed state shape will get a generated worst-case benchmark
/// before recording is activated. This limit gives the current estimate more
/// than twice its expected headroom while bounding corrupt input now.
const MAX_PRIVATE_STATE_BYTES: usize = 16 * 1024 * 1024;

/// Failure to load private telemetry state while holding its lock.
#[derive(Debug)]
pub(in crate::telemetry) enum LoadStateError {
    Io(io::Error),
    TooLarge { maximum: usize },
    Content(StateContentError),
    UnsupportedVersion(u64),
}

impl fmt::Display for LoadStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("failed to read telemetry private state"),
            Self::TooLarge { maximum } => write!(
                formatter,
                "telemetry private state exceeds the {maximum}-byte safety limit"
            ),
            Self::Content(error) => error.fmt(formatter),
            Self::UnsupportedVersion(version) => {
                StateDecodeError::UnsupportedVersion(*version).fmt(formatter)
            }
        }
    }
}

impl Error for LoadStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TooLarge { .. } | Self::Content(_) | Self::UnsupportedVersion(_) => None,
        }
    }
}

impl From<io::Error> for LoadStateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StateDecodeError> for LoadStateError {
    fn from(error: StateDecodeError) -> Self {
        match error {
            StateDecodeError::Malformed(error) => Self::Content(error),
            StateDecodeError::UnsupportedVersion(version) => Self::UnsupportedVersion(version),
        }
    }
}

/// Failure to serialize or atomically replace private telemetry state.
#[derive(Debug)]
pub(in crate::telemetry) enum ReplaceStateError {
    Serialize(toml::ser::Error),
    Replace(AtomicReplaceError),
}

impl fmt::Display for ReplaceStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(_) => {
                formatter.write_str("failed to serialize telemetry private state")
            }
            Self::Replace(_) => formatter.write_str("failed to replace telemetry private state"),
        }
    }
}

impl Error for ReplaceStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize(error) => Some(error),
            Self::Replace(error) => Some(error),
        }
    }
}

impl LockedStorage {
    /// Load and validate private state without retaining sensitive diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an I/O error, a sanitized malformed-state reason, or the
    /// unsupported version found in an otherwise syntactically valid document.
    pub(in crate::telemetry) fn load_state(
        &self,
    ) -> Result<Option<TelemetryStateV1>, LoadStateError> {
        let Some(bytes) = read_bounded(self.paths.state_file())? else {
            return Ok(None);
        };

        decode(&bytes).map(Some).map_err(Into::into)
    }

    /// Serialize completely, then atomically replace private state.
    ///
    /// Mutable access keeps two replacements from overlapping under one held
    /// lock. The replacement is atomic but intentionally not crash-durable.
    ///
    /// # Errors
    ///
    /// Returns the serialization or atomic replacement stage that failed.
    pub(in crate::telemetry) fn replace_state(
        &mut self,
        state: &TelemetryStateV1,
    ) -> Result<(), ReplaceStateError> {
        let serialized = encode(state).map_err(ReplaceStateError::Serialize)?;
        atomic::replace(self.paths.state_file(), serialized.as_bytes())
            .map_err(ReplaceStateError::Replace)
    }
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, LoadStateError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let limit = u64::try_from(MAX_PRIVATE_STATE_BYTES + 1)
        .expect("BUG: private-state read limit must fit in u64");
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PRIVATE_STATE_BYTES {
        return Err(LoadStateError::TooLarge {
            maximum: MAX_PRIVATE_STATE_BYTES,
        });
    }

    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::{error::Error as _, fs};

    use super::{LoadStateError, MAX_PRIVATE_STATE_BYTES};
    use crate::telemetry::{
        state::{IDENTIFIER_WINDOW_TEST_STATE, StateContentError, TelemetryStateV1},
        storage::LockedStorage,
    };

    fn storage(temporary: &tempfile::TempDir) -> LockedStorage {
        LockedStorage::try_acquire(temporary.path()).unwrap()
    }

    fn assert_diagnostic_redacts(error: &LoadStateError, sensitive: &str) {
        assert!(!error.to_string().contains(sensitive));
        assert!(!format!("{error:?}").contains(sensitive));
        assert!(error.source().is_none());
    }

    #[test]
    fn missing_private_state_is_not_created_or_reported_as_an_error() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);

        let state = storage.load_state().unwrap();

        assert!(state.is_none());
        assert!(!storage.paths.state_file().exists());
    }

    #[test]
    fn invalid_utf8_is_malformed_instead_of_an_io_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);
        fs::write(storage.paths.state_file(), b"version = 1\n\xff").unwrap();

        let Err(error) = storage.load_state() else {
            panic!("accepted private state containing invalid UTF-8");
        };

        assert!(matches!(
            error,
            LoadStateError::Content(StateContentError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn malformed_state_diagnostics_never_retain_the_identity_key() {
        const RECOGNIZABLE_SECRET: &str = "recognizable-private-identity-key";

        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);
        let source = format!("version = 1\n\n[identity]\nkey = \"{RECOGNIZABLE_SECRET}\n");
        fs::write(storage.paths.state_file(), source).unwrap();

        let Err(error) = storage.load_state() else {
            panic!("accepted malformed private state");
        };
        assert!(matches!(
            &error,
            LoadStateError::Content(StateContentError::InvalidToml {
                line: Some(_),
                column: Some(_)
            })
        ));
        assert_diagnostic_redacts(&error, RECOGNIZABLE_SECRET);
    }

    #[test]
    fn invalid_state_diagnostics_never_retain_the_identity_key() {
        const RECOGNIZABLE_INVALID_KEY: &str = "recognizable-invalid-private-key";

        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);
        let source = format!(
            "version = 1\n\n[identity]\nkey = \"{RECOGNIZABLE_INVALID_KEY}\"\n\
             identifier-window-anchor = \"2026-08-03\"\n"
        );
        fs::write(storage.paths.state_file(), source).unwrap();

        let Err(error) = storage.load_state() else {
            panic!("accepted private state containing an invalid identity key");
        };

        assert!(matches!(
            &error,
            LoadStateError::Content(StateContentError::InvalidState {
                line: Some(_),
                column: Some(_)
            })
        ));
        assert_diagnostic_redacts(&error, RECOGNIZABLE_INVALID_KEY);
    }

    #[test]
    fn unsupported_version_is_distinct_from_malformed_state() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);
        fs::write(
            storage.paths.state_file(),
            b"version = 2\nfuture-field = true\n",
        )
        .unwrap();

        let Err(error) = storage.load_state() else {
            panic!("accepted an unsupported private-state version");
        };

        assert!(matches!(error, LoadStateError::UnsupportedVersion(2)));
    }

    #[test]
    fn private_state_read_failure_is_reported_as_io() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);
        fs::create_dir(storage.paths.state_file()).unwrap();

        let Err(error) = storage.load_state() else {
            panic!("loaded a directory as private state");
        };

        assert!(matches!(error, LoadStateError::Io(_)));
    }

    #[test]
    fn oversized_private_state_is_rejected_at_the_read_boundary() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = storage(&temporary);
        fs::write(
            storage.paths.state_file(),
            vec![b' '; MAX_PRIVATE_STATE_BYTES + 1],
        )
        .unwrap();

        let Err(error) = storage.load_state() else {
            panic!("accepted oversized private state");
        };

        assert!(matches!(
            error,
            LoadStateError::TooLarge {
                maximum: MAX_PRIVATE_STATE_BYTES
            }
        ));
    }

    #[test]
    fn replacement_load_and_reserialization_preserve_canonical_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let mut storage = storage(&temporary);
        let state: TelemetryStateV1 = toml::from_str(IDENTIFIER_WINDOW_TEST_STATE).unwrap();

        storage.replace_state(&state).unwrap();
        let on_disk = fs::read(storage.paths.state_file()).unwrap();
        let loaded = storage
            .load_state()
            .unwrap()
            .expect("the replaced state file must exist");
        let reserialized = toml::to_string_pretty(&loaded).unwrap().into_bytes();

        assert_eq!(on_disk, IDENTIFIER_WINDOW_TEST_STATE.as_bytes());
        assert_eq!(reserialized, on_disk);
    }
}
