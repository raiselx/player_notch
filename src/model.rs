#[cfg(windows)]
use crate::artwork::Artwork;
#[cfg(windows)]
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Playback {
    Playing,
    Paused,
    #[default]
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackPosition {
    seconds: f64,
    at: Instant,
    rate: f64,
    correction: f64,
}

impl PlaybackPosition {
    pub fn new(seconds: f64, at: Instant, rate: f64) -> Self {
        Self {
            seconds,
            at,
            rate,
            correction: 0.0,
        }
    }

    pub fn reconcile(self, seconds: f64, at: Instant, rate: f64, duration: f64) -> Self {
        let previous = self.value(at, duration);
        if rate > 0.0
            && ((previous - seconds).abs() <= 1.25
                || (self.uncorrected_value(at, duration) - seconds).abs() <= 1.25)
        {
            Self::smooth_to(seconds, at, rate, previous)
        } else {
            Self::new(seconds, at, rate)
        }
    }

    pub fn smooth_to(seconds: f64, at: Instant, rate: f64, previous: f64) -> Self {
        Self {
            correction: previous - seconds,
            ..Self::new(seconds, at, rate)
        }
    }

    pub fn rate(self) -> f64 {
        self.rate
    }

    pub fn with_rate(self, rate: f64, now: Instant, duration: f64) -> Self {
        if self.rate == rate {
            self
        } else {
            Self::new(self.value(now, duration), now, rate)
        }
    }

    pub fn value(self, now: Instant, duration: f64) -> f64 {
        let elapsed = self.elapsed(now);
        // Even a late seek confirmation should never move the cursor backward.
        let correction_span = (self.correction.abs() * 2.0).max(2.0);
        (self.seconds + elapsed + self.correction * (1.0 - elapsed / correction_span).max(0.0))
            .clamp(0.0, duration)
    }

    pub fn uncorrected_value(self, now: Instant, duration: f64) -> f64 {
        (self.seconds + self.elapsed(now)).clamp(0.0, duration)
    }

    fn elapsed(self, now: Instant) -> f64 {
        now.saturating_duration_since(self.at).as_secs_f64() * self.rate
    }
}

#[derive(Clone, Debug)]
pub struct MediaSnapshot {
    pub active: bool,
    pub session: u64,
    pub track: u64,
    pub playback: Playback,
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub position: PlaybackPosition,
    pub duration: f64,
    #[cfg(windows)]
    pub artwork: Option<Arc<Artwork>>,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub seek_range: Option<(f64, f64)>,
    pub can_shuffle: bool,
    pub can_repeat: bool,
    pub shuffle: bool,
    /// 0 = off, 1 = repeat all, 2 = repeat one.
    pub repeat: u8,
    pub handled_seek: u64,
    pub observed_at: Instant,
    pub error: Option<String>,
}

impl Default for MediaSnapshot {
    fn default() -> Self {
        Self {
            active: false,
            session: 0,
            track: 0,
            playback: Playback::Stopped,
            app_id: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            position: PlaybackPosition::new(0.0, Instant::now(), 0.0),
            duration: 0.0,
            #[cfg(windows)]
            artwork: None,
            can_previous: false,
            can_next: false,
            can_play: false,
            can_pause: false,
            seek_range: None,
            can_shuffle: false,
            can_repeat: false,
            shuffle: false,
            repeat: 0,
            handled_seek: 0,
            observed_at: Instant::now(),
            error: None,
        }
    }
}

impl MediaSnapshot {
    pub fn track_key(&self) -> (&str, &str, &str, &str) {
        (&self.app_id, &self.title, &self.artist, &self.album)
    }

    pub fn position_at(&self, now: Instant) -> f64 {
        self.position.value(now, self.duration)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum MediaCommand {
    Play,
    Pause,
    Next,
    Previous,
    Seek { seconds: f64, id: u64 },
    Shuffle(bool),
    Repeat(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_position_advances_and_clamps_to_duration() {
        let sample = MediaSnapshot {
            position: PlaybackPosition::new(
                9.0,
                Instant::now() - std::time::Duration::from_secs(2),
                1.0,
            ),
            duration: 10.0,
            playback: Playback::Playing,
            ..Default::default()
        };
        assert_eq!(sample.position_at(Instant::now()), 10.0);
    }

    #[test]
    fn paused_position_does_not_advance() {
        let sample = MediaSnapshot {
            position: PlaybackPosition::new(
                9.0,
                Instant::now() - std::time::Duration::from_secs(2),
                0.0,
            ),
            duration: 10.0,
            playback: Playback::Paused,
            ..Default::default()
        };
        assert_eq!(sample.position_at(Instant::now()), 9.0);
    }
}
