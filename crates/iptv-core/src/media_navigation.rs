//! Pure validation for on-demand seek controls; never changes
//! playback rate (port of Python `media_navigation.py`).

use std::time::{Duration, Instant};

/// Parse a Go to Time entry: seconds, MM:SS or HH:MM:SS, as
/// milliseconds.
///
/// # Errors
/// Python's messages: the shape error for anything that is not
/// 1-6 digits optionally followed by up to two `:N`/`NN` groups,
/// the below-60 error for a minutes/seconds group of 60 or more,
/// and the too-large error past seven days.
pub fn parse_position(value: &str) -> Result<i64, String> {
    let value = value.trim();
    let parts: Vec<&str> = value.split(':').collect();
    let shape_ok = !parts.is_empty()
        && parts.len() <= 3
        && !parts[0].is_empty()
        && parts[0].len() <= 6
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && parts[1..].iter().all(|part| {
            !part.is_empty() && part.len() <= 2 && part.bytes().all(|b| b.is_ascii_digit())
        });
    if !shape_ok {
        return Err("Enter seconds, MM:SS or HH:MM:SS.".to_string());
    }
    let numbers: Vec<i64> = parts.iter().map(|part| part.parse().unwrap_or(0)).collect();
    if numbers[1..].iter().any(|part| *part >= 60) {
        return Err("Minutes and seconds after a colon must be below 60.".to_string());
    }
    let mut seconds: i64 = 0;
    for part in &numbers {
        seconds = seconds * 60 + part;
    }
    if seconds > 7 * 86_400 {
        return Err("The requested position is too large.".to_string());
    }
    Ok(seconds * 1000)
}

/// Python `clock_text`: whole seconds as HH:MM:SS.
#[must_use]
pub fn clock_text(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    let (hours, rest) = (seconds / 3600, seconds % 3600);
    let (minutes, seconds) = (rest / 60, rest % 60);
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Python `target_position`: the clamped destination for a
/// relative or absolute seek, never closer than the final
/// second to the end.
///
/// # Errors
/// Python's messages when the position or duration is not
/// available yet, and its untranslated guard when both or
/// neither of delta/target are given.
pub fn target_position(
    current: i64,
    length: i64,
    delta: Option<i64>,
    target: Option<i64>,
) -> Result<i64, String> {
    if current < 0 {
        return Err("The playback position is not available yet.".to_string());
    }
    if delta.is_none() == target.is_none() {
        return Err("Specify one seek target".to_string());
    }
    let value = match delta {
        Some(delta) => current + delta,
        None => target.unwrap_or(0),
    };
    if length <= 0 {
        return Err("The media duration is not available yet.".to_string());
    }
    Ok(value.clamp(0, (length - 1000).max(0)))
}

/// The verdict of one pending-seek poll (Python
/// `PendingSeek.outcome`'s strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekOutcome {
    Stale,
    Done,
    Failed,
    Pending,
}

/// A seek in flight (port of Python `PendingSeek`): the outcome
/// is judged against the session generation and the live clock.
#[derive(Debug, Clone)]
pub struct PendingSeek {
    pub generation: u64,
    pub target: i64,
    pub started: Instant,
    pub origin: i64,
}

impl PendingSeek {
    #[must_use]
    pub fn new(generation: u64, target: i64, origin: i64) -> Self {
        Self {
            generation,
            target,
            started: Instant::now(),
            origin,
        }
    }

    /// Python `outcome`: a different generation is stale; a
    /// position within one second of the target is done (the
    /// tolerance accommodates keyframe timing without silently
    /// accepting an ignored jump); eight seconds without
    /// landing is failed; anything else is still pending.
    #[must_use]
    pub fn outcome(&self, generation: u64, position: i64, now: Instant) -> SeekOutcome {
        if generation != self.generation {
            return SeekOutcome::Stale;
        }
        if (position - self.target).abs() <= 1000 {
            return SeekOutcome::Done;
        }
        if now.duration_since(self.started) >= Duration::from_secs(8) {
            return SeekOutcome::Failed;
        }
        SeekOutcome::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_position_accepts_python_shapes() {
        assert_eq!(parse_position("90"), Ok(90_000));
        assert_eq!(parse_position(" 2:30 "), Ok(150_000));
        assert_eq!(parse_position("1:02:03"), Ok(3_723_000));
        assert_eq!(parse_position("90:30"), Ok(5_430_000));
        assert_eq!(parse_position("0"), Ok(0));
    }

    #[test]
    fn parse_position_rejects_with_python_messages() {
        assert_eq!(
            parse_position(""),
            Err("Enter seconds, MM:SS or HH:MM:SS.".to_string())
        );
        assert_eq!(
            parse_position("1:2:3:4"),
            Err("Enter seconds, MM:SS or HH:MM:SS.".to_string())
        );
        assert_eq!(
            parse_position("-5"),
            Err("Enter seconds, MM:SS or HH:MM:SS.".to_string())
        );
        assert_eq!(
            parse_position("1234567"),
            Err("Enter seconds, MM:SS or HH:MM:SS.".to_string())
        );
        assert_eq!(
            parse_position("1:60"),
            Err("Minutes and seconds after a colon must be below 60.".to_string())
        );
        assert_eq!(
            parse_position("200:00:00"),
            Err("The requested position is too large.".to_string())
        );
    }

    #[test]
    fn clock_text_formats_whole_seconds() {
        assert_eq!(clock_text(0), "00:00:00");
        assert_eq!(clock_text(3_723_999), "01:02:03");
        assert_eq!(clock_text(-5), "00:00:00");
    }

    #[test]
    fn target_position_clamps_and_validates() {
        assert_eq!(target_position(5_000, 60_000, Some(-10_000), None), Ok(0));
        assert_eq!(
            target_position(5_000, 60_000, Some(120_000), None),
            Ok(59_000)
        );
        assert_eq!(
            target_position(5_000, 60_000, None, Some(30_000)),
            Ok(30_000)
        );
        assert_eq!(
            target_position(-1, 60_000, Some(1), None),
            Err("The playback position is not available yet.".to_string())
        );
        assert_eq!(
            target_position(0, 0, Some(1), None),
            Err("The media duration is not available yet.".to_string())
        );
        assert_eq!(
            target_position(0, 60_000, None, None),
            Err("Specify one seek target".to_string())
        );
    }

    #[test]
    fn pending_seek_outcomes_match_python() {
        let seek = PendingSeek::new(3, 50_000, 10_000);
        assert_eq!(seek.outcome(4, 50_000, Instant::now()), SeekOutcome::Stale);
        assert_eq!(seek.outcome(3, 50_900, Instant::now()), SeekOutcome::Done);
        assert_eq!(
            seek.outcome(3, 10_000, Instant::now()),
            SeekOutcome::Pending
        );
        let later = seek.started + Duration::from_secs(9);
        assert_eq!(seek.outcome(3, 10_000, later), SeekOutcome::Failed);
    }
}
