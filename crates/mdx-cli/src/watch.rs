//! Polling watch state for `mdx watch`.
//!
//! `std` only, no watcher crate: once per [`TICK`] the loop asks the
//! filesystem for the input's modification time and feeds the answer to
//! [`Watcher::step`]. A change is not built the moment it is seen — a save is
//! usually several writes — so the loop waits until the modification time has
//! been stable for the debounce window ([`DEFAULT_DEBOUNCE_MS`] unless
//! `--debounce-ms` says otherwise) and then rebuilds once. The first build of a
//! run is the exception: it starts as soon as the input can be read, so a fresh
//! `mdx watch` shows its result instead of idling for a save.
//!
//! The decision itself is the pure function [`decide`], kept apart from the
//! loop so the debounce rule can be tested without a clock or a file, and so
//! `mdx watch` never has to sleep in a test to prove a timing property.
//!
//! Limitations of mtime polling, by design: a second save that lands within the
//! filesystem's timestamp granularity is invisible, and a timestamp-only change
//! (`touch`) rebuilds. An input that cannot be read is never built; the loop
//! reports what went wrong once and keeps polling for it to come back.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the watch loop asks the input for its modification time.
pub(crate) const TICK: Duration = Duration::from_millis(250);

/// Quiet period a change needs before it is built, unless overridden.
pub(crate) const DEFAULT_DEBOUNCE_MS: u64 = 400;

/// What the loop does with the change pending at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Nothing to build: no change is pending.
    Idle,
    /// A change is pending, but it is younger than the debounce window.
    Waiting,
    /// The pending change has been quiet for the whole debounce window.
    Build,
}

/// Decide whether the change observed at `changed_at` is ready to build.
///
/// Pure: it reads no clock and touches no file, so the debounce rule is one
/// function with one table of cases. `changed_at` is `None` when the built
/// output already matches the input; a `debounce` of zero builds on the first
/// observation. A `now` before `changed_at` counts as no quiet time at all.
pub(crate) fn decide(now: Instant, changed_at: Option<Instant>, debounce: Duration) -> Decision {
    match changed_at {
        None => Decision::Idle,
        Some(since) if now.saturating_duration_since(since) >= debounce => Decision::Build,
        Some(_) => Decision::Waiting,
    }
}

/// Modification time of `path`, or the message saying why it has none.
pub(crate) fn mtime(path: &Path) -> Result<SystemTime, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return Err(unreadable(path, &error)),
    };
    match metadata.modified() {
        Ok(time) => Ok(time),
        Err(error) => Err(unreadable(path, &error)),
    }
}

/// The message a poll reports when `path` cannot be read.
fn unreadable(path: &Path, error: &std::io::Error) -> String {
    format!("cannot read {}: {error}", path.display())
}

/// Whether `input` and `output` name the same file.
///
/// `mdx watch` refuses such a pair: a build that rewrites the file being
/// watched changes its modification time and rebuilds forever. Both paths are
/// resolved as far as they exist — the input does, the output usually does not
/// yet, so its directory is resolved instead — and a pair whose directories
/// cannot be resolved is compared literally.
pub(crate) fn same_file(input: &Path, output: &Path) -> bool {
    match (resolve(input), resolve(output)) {
        (Some(left), Some(right)) => left == right,
        _ => input == output,
    }
}

/// Absolute form of `path` with symlinked directories resolved, if possible.
fn resolve(path: &Path) -> Option<PathBuf> {
    // The file itself first — that follows a symlinked file — and its directory
    // otherwise, which is all a not-yet-written output has.
    match fs::canonicalize(path) {
        Ok(absolute) => Some(absolute),
        Err(_) => {
            let name = path.file_name()?;
            let parent = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            let parent = fs::canonicalize(parent).ok()?;
            Some(parent.join(name))
        }
    }
}

/// What one poll of the input produced for the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// The input cannot be read and its message has not been reported yet.
    Unreachable(String),
    /// Rebuild now.
    Build,
    /// Nothing to do this tick.
    Wait,
}

/// Debounce state of one watch loop.
pub(crate) struct Watcher {
    /// Quiet period a change must reach before it is built.
    debounce: Duration,
    /// Modification time last seen, `None` when the input has never been
    /// readable (which is also what an unreadable input resets to).
    observed: Option<SystemTime>,
    /// When the newest not-yet-built change was first seen; `None` when the
    /// built output is up to date.
    changed_at: Option<Instant>,
    /// Failure reported last, so a lasting one is reported once.
    failure: Option<String>,
}

impl Watcher {
    /// A watcher that has not looked at the input yet.
    pub(crate) fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            observed: None,
            changed_at: None,
            failure: None,
        }
    }

    /// Fold one poll into the state and report what the loop should do.
    ///
    /// `poll` is the input's modification time or the message saying why it
    /// could not be read. The first readable observation is built without
    /// waiting, so a fresh `mdx watch` shows its result instead of idling for
    /// the next save; every later change waits out the debounce window. An
    /// unreadable input drops any pending build, because building a file the
    /// loop cannot read would only repeat the failure.
    pub(crate) fn step(&mut self, now: Instant, poll: Result<SystemTime, String>) -> Step {
        let mtime = match poll {
            Ok(mtime) => mtime,
            Err(message) => {
                self.observed = None;
                self.changed_at = None;
                if self.failure.as_deref() == Some(message.as_str()) {
                    return Step::Wait;
                }
                self.failure = Some(message.clone());
                return Step::Unreachable(message);
            }
        };

        self.failure = None;
        let window = match self.observed {
            None => Duration::ZERO,
            Some(_) => self.debounce,
        };
        if self.observed != Some(mtime) {
            self.observed = Some(mtime);
            self.changed_at = Some(now);
        }

        match decide(now, self.changed_at, window) {
            Decision::Build => {
                self.changed_at = None;
                Step::Build
            }
            Decision::Idle | Decision::Waiting => Step::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `base` moved forward by `millis`, the only clock a test needs.
    fn after(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    /// The debounce window most tests use.
    fn window() -> Duration {
        Duration::from_millis(DEFAULT_DEBOUNCE_MS)
    }

    #[test]
    fn decider_is_idle_without_a_pending_change() {
        let base = Instant::now();
        assert_eq!(decide(base, None, window()), Decision::Idle);
        assert_eq!(decide(after(base, 10_000), None, window()), Decision::Idle);
    }

    #[test]
    fn decider_waits_out_the_window_and_then_builds() {
        let base = Instant::now();
        let since = Some(base);

        assert_eq!(decide(base, since, window()), Decision::Waiting);
        assert_eq!(decide(after(base, 399), since, window()), Decision::Waiting);
        // Exactly at the window the change is quiet enough.
        assert_eq!(decide(after(base, 400), since, window()), Decision::Build);
        assert_eq!(
            decide(after(base, 60_000), since, window()),
            Decision::Build
        );
    }

    #[test]
    fn decider_is_never_fooled_by_a_backwards_clock() {
        let base = Instant::now();
        assert_eq!(
            decide(base, Some(after(base, 100)), window()),
            Decision::Waiting
        );
    }

    #[test]
    fn a_zero_window_builds_on_the_first_observation() {
        let base = Instant::now();
        assert_eq!(decide(base, Some(base), Duration::ZERO), Decision::Build);
    }

    #[test]
    fn the_first_observation_is_built_at_once_and_then_settles() {
        let base = Instant::now();
        let mut watcher = Watcher::new(window());
        let file = SystemTime::UNIX_EPOCH;

        // A fresh watch builds the document it was pointed at without waiting
        // for a save.
        assert_eq!(watcher.step(base, Ok(file)), Step::Build);
        // Nothing changed since that build, so the loop settles down.
        assert_eq!(watcher.step(after(base, 250), Ok(file)), Step::Wait);
        assert_eq!(watcher.step(after(base, 400), Ok(file)), Step::Wait);
        assert_eq!(watcher.step(after(base, 650), Ok(file)), Step::Wait);
    }

    #[test]
    fn a_second_write_restarts_the_window() {
        let base = Instant::now();
        let mut watcher = Watcher::new(window());
        let first = SystemTime::UNIX_EPOCH;
        let second = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        assert_eq!(watcher.step(base, Ok(first)), Step::Build);
        // The file is written again while the first save is still settling.
        assert_eq!(watcher.step(after(base, 300), Ok(second)), Step::Wait);
        assert_eq!(watcher.step(after(base, 600), Ok(second)), Step::Wait);
        assert_eq!(watcher.step(after(base, 700), Ok(second)), Step::Build);
    }

    #[test]
    fn a_later_change_is_built_again() {
        let base = Instant::now();
        let mut watcher = Watcher::new(window());
        let first = SystemTime::UNIX_EPOCH;
        let second = SystemTime::UNIX_EPOCH + Duration::from_secs(30);

        assert_eq!(watcher.step(base, Ok(first)), Step::Build);
        assert_eq!(watcher.step(after(base, 400), Ok(first)), Step::Wait);
        assert_eq!(watcher.step(after(base, 600), Ok(second)), Step::Wait);
        assert_eq!(watcher.step(after(base, 1_000), Ok(second)), Step::Build);
    }

    #[test]
    fn an_unreadable_input_is_reported_once_and_recovery_builds() {
        let base = Instant::now();
        let mut watcher = Watcher::new(window());
        let message = "cannot read doc.md: No such file or directory (os error 2)".to_string();

        assert_eq!(
            watcher.step(base, Err(message.clone())),
            Step::Unreachable(message.clone())
        );
        // Still missing: the loop keeps polling without repeating itself.
        assert_eq!(
            watcher.step(after(base, 250), Err(message.clone())),
            Step::Wait
        );
        assert_eq!(
            watcher.step(after(base, 500), Err(message.clone())),
            Step::Wait
        );

        // The file is created: a document that appears is built at once.
        let file = SystemTime::UNIX_EPOCH + Duration::from_secs(7);
        assert_eq!(watcher.step(after(base, 750), Ok(file)), Step::Build);
        assert_eq!(watcher.step(after(base, 1_050), Ok(file)), Step::Wait);
        // And a third failure is news again.
        let reported = watcher.step(after(base, 1_400), Err(message.clone()));
        assert_eq!(reported, Step::Unreachable(message));
    }
}
