//! Reading `SKULD_*` variables. One policy: a value that is not valid UTF-8 is
//! an error, never "unset".

/// The value of `name`, or `None` if unset.
///
/// # Panics
///
/// If the value is set but not valid UTF-8.
pub(crate) fn read(name: &str) -> Option<String> {
    #[cfg(test)]
    READS.with(|r| r.borrow_mut().push(name.to_owned()));
    match std::env::var(name) {
        Ok(val) => Some(val),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(raw)) => panic!("skuld: {name} is not valid UTF-8 ({raw:?})"),
    }
}

// Read counting (unit tests only) =====

#[cfg(test)]
thread_local! {
    static READS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Forget the reads recorded on this thread.
#[cfg(test)]
pub(crate) fn reset_reads() {
    READS.with(|r| r.borrow_mut().clear());
}

/// How many times this thread has [`read`] `name` since the last [`reset_reads`].
#[cfg(test)]
pub(crate) fn reads_of(name: &str) -> usize {
    READS.with(|r| r.borrow().iter().filter(|n| *n == name).count())
}
