//! Reading `SKULD_*` variables. One policy: a value that is not valid UTF-8 is
//! an error, never "unset".

/// The value of `name`, or `None` if unset.
///
/// # Panics
///
/// If the value is set but not valid UTF-8.
pub(crate) fn read(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(val) => Some(val),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(raw)) => panic!("skuld: {name} is not valid UTF-8 ({raw:?})"),
    }
}
