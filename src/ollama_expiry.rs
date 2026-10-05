//! Absolute observed-deadline policy and validated persisted deadline evidence.
//! The experimental adapter stores the policy/version; production routing is pending.
use anyhow::{Result, bail};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    System::Time::SystemTimeToFileTime,
};

pub const POLICY: &str = "absolute-observed-deadline-v1";
/// Narrow experimental capture limit. Longer or indefinite residency is refused,
/// never silently shortened or converted to an indefinite request.
pub const MAX_CAPTURED_RESIDENCY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deadline {
    captured_at: Duration,
    expires_at: Duration,
    captured_remaining: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyPlan {
    Expired,
    KeepFor(Duration),
}

impl Deadline {
    pub fn validate(&self, raw: Option<&str>) -> Result<()> {
        let raw = raw.ok_or_else(|| anyhow::anyhow!("Ollama expiry evidence is missing"))?;
        if UNIX_EPOCH.checked_add(self.captured_at).is_none()
            || parse_timestamp(raw)? != self.expires_at
            || self.expires_at.saturating_sub(self.captured_at) != self.captured_remaining
            || self.captured_remaining > MAX_CAPTURED_RESIDENCY
        {
            bail!("Ollama persisted expiry evidence is inconsistent");
        }
        Ok(())
    }
    pub fn captured_at(&self) -> Duration {
        self.captured_at
    }
    /// Missing, zero, indefinite or malformed evidence cannot become a replay
    /// obligation. An already-expired valid timestamp is a distinct outcome.
    pub fn capture(raw: Option<&str>, now: SystemTime) -> Result<Self> {
        let Some(raw) = raw else {
            bail!("Ollama expiry evidence is missing")
        };
        let expires_at = parse_timestamp(raw)?;
        let now = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow::anyhow!("Clock precedes Unix epoch; expiry capture is refused"))?;
        let remaining = expires_at.checked_sub(now).unwrap_or(Duration::ZERO);
        if remaining > MAX_CAPTURED_RESIDENCY {
            bail!("Ollama expiry exceeds the experimental residency limit");
        }
        Ok(Self {
            captured_at: now,
            expires_at,
            captured_remaining: remaining,
        })
    }

    /// Within a process, elapsed monotonic time caps the original budget even
    /// after a wall-clock rollback. After restart only the persisted absolute
    /// evidence is available; a clock earlier than capture refuses replay.
    pub fn plan(&self, now: SystemTime, elapsed: Option<Duration>) -> Result<ResidencyPlan> {
        let now = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow::anyhow!("Clock precedes Unix epoch; replay is refused"))?;
        if now < self.captured_at {
            bail!("Clock precedes Ollama expiry capture; replay is refused");
        }
        let wall_remaining = self.expires_at.checked_sub(now).unwrap_or(Duration::ZERO);
        let budget = elapsed.map_or(self.captured_remaining, |elapsed| {
            self.captured_remaining.saturating_sub(elapsed)
        });
        let remaining = wall_remaining.min(budget);
        if remaining.is_zero() {
            Ok(ResidencyPlan::Expired)
        } else {
            Ok(ResidencyPlan::KeepFor(remaining))
        }
    }
}

/// Strict RFC3339 subset used by Go's time JSON: numeric offset or Z, optional
/// 1..9 fractional digits. Calendar validation uses the existing native stack.
/// Unknown offsets, leap seconds and pre-epoch evidence are deliberately refused.
fn parse_timestamp(raw: &str) -> Result<Duration> {
    fn invalid() -> anyhow::Error {
        anyhow::anyhow!("Unsupported Ollama expiry timestamp")
    }
    let bytes = raw.as_bytes();
    if !(20..=35).contains(&bytes.len())
        || !raw.is_ascii()
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(invalid());
    }
    let number = |start: usize, end: usize| -> Result<u16> {
        if !bytes[start..end].iter().all(u8::is_ascii_digit) {
            return Err(invalid());
        }
        raw[start..end].parse().map_err(|_| invalid())
    };
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if year < 1970 || hour > 23 || minute > 59 || second > 59 {
        return Err(invalid());
    }
    let mut index = 19;
    let mut nanos = 0u32;
    if bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            nanos = nanos
                .checked_mul(10)
                .and_then(|n| n.checked_add((bytes[index] - b'0') as u32))
                .ok_or_else(invalid)?;
            index += 1;
        }
        let digits = index - start;
        if !(1..=9).contains(&digits) {
            return Err(invalid());
        }
        nanos *= 10u32.pow((9 - digits) as u32);
    }
    let offset_seconds = match bytes.get(index) {
        Some(b'Z') if index + 1 == bytes.len() => 0i64,
        Some(b'+' | b'-') if index + 6 == bytes.len() && bytes[index + 3] == b':' => {
            let hours = number(index + 1, index + 3)?;
            let minutes = number(index + 4, index + 6)?;
            if hours > 23 || minutes > 59 || &raw[index..] == "-00:00" {
                return Err(invalid());
            }
            (hours as i64 * 3600 + minutes as i64 * 60) * if bytes[index] == b'-' { -1 } else { 1 }
        }
        _ => return Err(invalid()),
    };
    let native = SYSTEMTIME {
        wYear: year,
        wMonth: month,
        wDay: day,
        wHour: hour,
        wMinute: minute,
        wSecond: second,
        ..Default::default()
    };
    let mut filetime = FILETIME::default();
    if unsafe { SystemTimeToFileTime(&native, &mut filetime) } == 0 {
        return Err(invalid());
    }
    let ticks = ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64;
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let seconds = ticks.checked_sub(UNIX_EPOCH_TICKS).ok_or_else(invalid)? / 10_000_000;
    let utc_seconds = (seconds as i64)
        .checked_sub(offset_seconds)
        .ok_or_else(invalid)?;
    if utc_seconds < 0 {
        return Err(invalid());
    }
    // Retain nanoseconds as Duration: Windows SystemTime stores only 100 ns
    // precision and would otherwise lose exact capture-limit boundary evidence.
    Ok(Duration::new(utc_seconds as u64, nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> SystemTime {
        UNIX_EPOCH + parse_timestamp("2026-10-05T12:00:00Z").unwrap()
    }

    #[test]
    fn offsets_fractions_and_calendar_dates_have_exact_meaning() {
        let unix_now = now().duration_since(UNIX_EPOCH).unwrap();
        for raw in [
            "2026-10-05T14:00:00+02:00",
            "2026-10-05T05:00:00-07:00",
            "2026-10-05T12:00:00+00:00",
        ] {
            assert_eq!(parse_timestamp(raw).unwrap(), unix_now);
        }
        assert_eq!(
            parse_timestamp("2026-10-05T12:00:00.123456789Z").unwrap(),
            unix_now + Duration::from_nanos(123_456_789)
        );
        assert_eq!(
            parse_timestamp("2026-10-05T12:00:00.1Z").unwrap(),
            unix_now + Duration::from_millis(100)
        );
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:00Z").unwrap(),
            Duration::ZERO
        );
        assert_eq!(
            parse_timestamp("2024-03-01T00:00:00Z")
                .unwrap()
                .checked_sub(parse_timestamp("2024-02-29T00:00:00Z").unwrap())
                .unwrap(),
            Duration::from_secs(86_400)
        );
    }

    #[test]
    fn malformed_missing_and_indefinite_evidence_never_gets_a_default() {
        assert!(Deadline::capture(None, now()).is_err());
        for raw in [
            "",
            "unknown",
            "0001-01-01T00:00:00Z",
            "9999-12-31T23:59:59Z",
            "2026-02-29T12:00:00Z",
            "2026-04-31T12:00:00Z",
            "2026-00-01T12:00:00Z",
            "2026-10-05T24:00:00Z",
            "2026-10-05T12:00:60Z",
            "2026-10-05T12:00:00-00:00",
            "2026-10-05T12:00:00+24:00",
            "2026-10-05T12:00:00.1234567890Z",
            "2026-10-05T12:00:00.Z",
            "2026-10-05T12:00:00Z trailing",
            "2026-10-05t12:00:00z",
            "2026-10-05T12:00:00",
            "2026-10-05T12:00:00é",
        ] {
            assert!(Deadline::capture(Some(raw), now()).is_err(), "{raw}");
        }
    }

    #[test]
    fn absolute_deadline_counts_pause_time_and_expires_separately() {
        let deadline = Deadline::capture(Some("2026-10-05T12:05:00Z"), now()).unwrap();
        assert_eq!(
            deadline
                .plan(
                    now() + Duration::from_secs(120),
                    Some(Duration::from_secs(120))
                )
                .unwrap(),
            ResidencyPlan::KeepFor(Duration::from_secs(180))
        );
        assert_eq!(
            deadline
                .plan(
                    now() + Duration::from_secs(300),
                    Some(Duration::from_secs(300))
                )
                .unwrap(),
            ResidencyPlan::Expired
        );
        assert_eq!(
            deadline
                .plan(now() + Duration::from_secs(600), None)
                .unwrap(),
            ResidencyPlan::Expired
        );
        let already_expired = Deadline::capture(Some("2026-10-05T11:59:59Z"), now()).unwrap();
        assert_eq!(
            already_expired.plan(now(), None).unwrap(),
            ResidencyPlan::Expired
        );
    }

    #[test]
    fn clock_changes_cannot_exceed_original_monotonic_budget() {
        let deadline = Deadline::capture(Some("2026-10-05T12:05:00Z"), now()).unwrap();
        assert!(deadline.plan(now() - Duration::from_secs(1), None).is_err());
        assert_eq!(
            deadline
                .plan(
                    now() + Duration::from_secs(10),
                    Some(Duration::from_secs(200))
                )
                .unwrap(),
            ResidencyPlan::KeepFor(Duration::from_secs(100))
        );
        assert_eq!(
            deadline
                .plan(
                    now() + Duration::from_secs(250),
                    Some(Duration::from_secs(200))
                )
                .unwrap(),
            ResidencyPlan::KeepFor(Duration::from_secs(50))
        );
        assert_eq!(
            deadline.plan(now(), Some(Duration::MAX)).unwrap(),
            ResidencyPlan::Expired
        );
        assert_eq!(
            deadline
                .plan(now() + Duration::from_secs(120), None)
                .unwrap(),
            ResidencyPlan::KeepFor(Duration::from_secs(180))
        );
    }

    #[test]
    fn captured_limit_and_subsecond_expiry_are_not_rounded_up() {
        let deadline = Deadline::capture(Some("2026-10-06T12:00:00Z"), now()).unwrap();
        assert_eq!(
            deadline.plan(now(), None).unwrap(),
            ResidencyPlan::KeepFor(MAX_CAPTURED_RESIDENCY)
        );
        assert!(Deadline::capture(Some("2026-10-06T12:00:00.000000001Z"), now()).is_err());
        let deadline = Deadline::capture(Some("2026-10-05T12:00:00.000000001Z"), now()).unwrap();
        assert_eq!(
            deadline.plan(now(), None).unwrap(),
            ResidencyPlan::KeepFor(Duration::from_nanos(1))
        );
        assert_eq!(
            deadline
                .plan(
                    now() + Duration::from_nanos(1),
                    Some(Duration::from_nanos(1))
                )
                .unwrap(),
            ResidencyPlan::Expired
        );
    }
}
