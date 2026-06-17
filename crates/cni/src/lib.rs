pub mod config;
pub mod delegate;
pub mod env;
pub mod error;
pub mod iptables;
pub mod result;
pub mod version;

pub use error::CniError;

/// Serializes tests that write an executable and then exec it. Running such tests
/// in parallel races on `ETXTBSY`: when one test `fork()`s (to spawn its script)
/// while another is mid-`execve` on its own freshly-written script, the forked
/// child transiently holds a write fd to that file (CLOEXEC only clears at the
/// child's exec), so the exec fails with "Text file busy". Every exec-spawning
/// test in this crate takes this lock.
#[cfg(test)]
pub(crate) static EXEC_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn exec_test_guard() -> std::sync::MutexGuard<'static, ()> {
    EXEC_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
