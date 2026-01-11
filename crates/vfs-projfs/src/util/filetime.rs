//! FILETIME conversion utilities.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::FILETIME;

/// Convert SystemTime to FILETIME.
///
/// FILETIME represents the number of 100-nanosecond intervals since
/// January 1, 1601 UTC.
///
/// # Arguments
/// * `time` - System time to convert
///
/// # Returns
/// FILETIME structure.
pub fn systemtime_to_filetime(time: SystemTime) -> FILETIME {
    // FILETIME epoch: January 1, 1601
    // Unix epoch: January 1, 1970
    // Difference: 11644473600 seconds
    const FILETIME_UNIX_DIFF_SECS: u64 = 11644473600;
    const INTERVALS_PER_SEC: u64 = 10_000_000;

    let duration: Duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    let intervals: u64 = duration.as_secs() * INTERVALS_PER_SEC
        + duration.subsec_nanos() as u64 / 100
        + FILETIME_UNIX_DIFF_SECS * INTERVALS_PER_SEC;

    FILETIME {
        dwLowDateTime: (intervals & 0xFFFFFFFF) as u32,
        dwHighDateTime: (intervals >> 32) as u32,
    }
}

/// Convert FILETIME to SystemTime.
///
/// # Arguments
/// * `filetime` - FILETIME structure
///
/// # Returns
/// System time.
pub fn filetime_to_systemtime(filetime: FILETIME) -> SystemTime {
    const FILETIME_UNIX_DIFF_SECS: u64 = 11644473600;
    const INTERVALS_PER_SEC: u64 = 10_000_000;

    let intervals: u64 = (filetime.dwHighDateTime as u64) << 32 | filetime.dwLowDateTime as u64;

    if intervals < FILETIME_UNIX_DIFF_SECS * INTERVALS_PER_SEC {
        // Before Unix epoch
        return UNIX_EPOCH;
    }

    let unix_intervals: u64 = intervals - FILETIME_UNIX_DIFF_SECS * INTERVALS_PER_SEC;
    let secs: u64 = unix_intervals / INTERVALS_PER_SEC;
    let nanos: u32 = ((unix_intervals % INTERVALS_PER_SEC) * 100) as u32;

    UNIX_EPOCH + Duration::new(secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_systemtime_to_filetime_epoch() {
        let filetime: FILETIME = systemtime_to_filetime(UNIX_EPOCH);
        // Unix epoch in FILETIME: 116444736000000000
        let intervals: u64 = (filetime.dwHighDateTime as u64) << 32 | filetime.dwLowDateTime as u64;
        assert_eq!(intervals, 116444736000000000);
    }

    #[test]
    fn test_filetime_to_systemtime_epoch() {
        let filetime = FILETIME {
            dwLowDateTime: 0xD53E8000,
            dwHighDateTime: 0x019DB1DE,
        };
        let time: SystemTime = filetime_to_systemtime(filetime);
        assert_eq!(time, UNIX_EPOCH);
    }

    #[test]
    fn test_roundtrip() {
        let now: SystemTime = SystemTime::now();
        let filetime: FILETIME = systemtime_to_filetime(now);
        let back: SystemTime = filetime_to_systemtime(filetime);

        // Should be within 100ns (1 FILETIME interval)
        let diff: Duration = if back > now {
            back.duration_since(now).unwrap()
        } else {
            now.duration_since(back).unwrap()
        };
        assert!(diff < Duration::from_nanos(100));
    }
}
