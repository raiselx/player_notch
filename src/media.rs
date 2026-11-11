//! Windows SMTC media backend. WinRT objects stay on the media worker.
use crate::apple_music::{self, AppleMusicUIA, AppleSource};
use crate::artwork::ArtworkLoader;
use crate::model::{MediaCommand, MediaSnapshot, Playback, PlaybackPosition};
use crate::now_playing::{self, AppleSession, NowPlaying};
use eframe::egui;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use windows::{
    Foundation::{
        AsyncOperationCompletedHandler, AsyncStatus, EventRegistrationToken, IAsyncOperation,
        TypedEventHandler,
    },
    Media::{
        Control::{
            GlobalSystemMediaTransportControlsSession as Session,
            GlobalSystemMediaTransportControlsSessionManager as Manager,
            GlobalSystemMediaTransportControlsSessionMediaProperties as Properties,
            GlobalSystemMediaTransportControlsSessionPlaybackInfo as PlaybackInfo,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
        },
        MediaPlaybackAutoRepeatMode as Repeat,
    },
    Storage::Streams::IRandomAccessStreamReference,
    core::{Error, HRESULT, Result, RuntimeType},
};

const PLAYING_POLL: Duration = Duration::from_millis(200);
const PAUSED_POLL: Duration = Duration::from_millis(400);
const IDLE_POLL: Duration = Duration::from_millis(750);
const LOADING_POLL: Duration = Duration::from_millis(70);
const APPLE_POLL: Duration = Duration::from_millis(100);
const ASYNC_TIMEOUT: Duration = Duration::from_secs(2);
// A queued command can wait for one asynchronous operation before starting its own.
pub const SEEK_ACK_TIMEOUT: Duration = Duration::from_millis(4250);
const MANAGER_TIMEOUT: Duration = Duration::from_secs(12);
const MANAGER_RETRY: Duration = Duration::from_secs(2);
const SEEK_TIMEOUT: Duration = Duration::from_millis(2400);
const SEEK_SETTLE: Duration = Duration::from_millis(400);
const SEEK_TOLERANCE: f64 = 1.25;
const PAUSE_SETTLE: Duration = Duration::from_millis(500);
const TICKS_PER_SECOND: f64 = 10_000_000.0;
const TIMEOUT: HRESULT = HRESULT(0x800705b4u32 as i32);

pub struct MediaBackend {
    shared: Arc<Mutex<MediaSnapshot>>,
    tx: mpsc::SyncSender<Request>,
    preferences: Arc<Mutex<MediaPreferences>>,
}

#[derive(Default)]
struct MediaPreferences {
    priority_app: Option<String>,
    apps: Vec<String>,
}

struct Request {
    session: u64,
    track: u64,
    command: MediaCommand,
    queued_at: Instant,
    playback_rate: f64,
}

impl MediaBackend {
    pub fn start(ctx: egui::Context, priority_app: Option<String>) -> Self {
        let shared = Arc::new(Mutex::new(MediaSnapshot::default()));
        let (tx, rx) = mpsc::sync_channel(32);
        let preferences = Arc::new(Mutex::new(MediaPreferences {
            priority_app,
            ..Default::default()
        }));
        let worker_preferences = preferences.clone();
        let out = shared.clone();
        let wake = ctx.clone();
        if let Err(error) = thread::Builder::new()
            .name("media-smtc".into())
            .spawn(move || worker_loop(out, rx, wake, worker_preferences))
        {
            publish_error(
                &shared,
                &ctx,
                format!("Не удалось запустить управление медиа: {error}"),
            );
        }
        Self {
            shared,
            tx,
            preferences,
        }
    }

    pub fn apps(&self) -> Vec<String> {
        self.preferences
            .lock()
            .map(|p| p.apps.clone())
            .unwrap_or_default()
    }

    pub fn set_priority_app(&self, priority_app: Option<String>) {
        if let Ok(mut preferences) = self.preferences.lock() {
            preferences.priority_app = priority_app;
        }
    }

    pub fn snapshot_since(&self, observed_at: Instant) -> Option<MediaSnapshot> {
        let snapshot = self.shared.lock().ok()?;
        (snapshot.observed_at != observed_at).then(|| snapshot.clone())
    }

    pub fn command(&self, snapshot: &MediaSnapshot, command: MediaCommand, now: Instant) -> bool {
        self.tx
            .try_send(Request {
                session: snapshot.session,
                track: snapshot.track,
                command,
                queued_at: now,
                playback_rate: snapshot.position.rate(),
            })
            .is_ok()
    }
}

fn worker_loop(
    shared: Arc<Mutex<MediaSnapshot>>,
    rx: mpsc::Receiver<Request>,
    ctx: egui::Context,
    preferences: Arc<Mutex<MediaPreferences>>,
) {
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
    if unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_err() {
        publish_error(
            &shared,
            &ctx,
            "Не удалось инициализировать управление медиа Windows".into(),
        );
        return;
    }
    run_worker(&shared, &rx, &ctx, &preferences);
    unsafe { RoUninitialize() };
}

fn run_worker(
    shared: &Mutex<MediaSnapshot>,
    rx: &mpsc::Receiver<Request>,
    ctx: &egui::Context,
    preferences: &Mutex<MediaPreferences>,
) {
    let mut manager = None;
    let mut manager_request = None;
    let mut manager_retry_at = Instant::now();
    let mut manager_error = None;
    let mut shell_manager = None;
    let mut shell_retry_at = Instant::now();
    let mut shell_session = None;
    let mut selected: Option<Selected> = None;
    let mut fallback = None;
    let mut next_session = 0;
    let mut artwork = ArtworkLoader::new();
    let mut apple = None;
    let mut request = None;
    loop {
        if manager.is_none() {
            let now = Instant::now();
            if let Some(result) = poll_manager(&mut manager_request, now, manager_retry_at) {
                match result {
                    Ok(new_manager) => {
                        manager = Some(new_manager);
                        manager_error = None;
                    }
                    Err(error) => {
                        manager_error = Some(format!("Медиасессии Windows недоступны: {error}"));
                        manager_retry_at = now + MANAGER_RETRY;
                    }
                }
            }
        }
        let sessions = manager.as_ref().map_or(Ok(None), |mgr| {
            pick_session(mgr, selected.as_ref(), preferences, ctx)
        });
        let mut wait = IDLE_POLL;
        match sessions {
            Ok(Some((session, info))) => {
                fallback = None;
                shell_session = None;
                if selected.as_ref().is_none_or(|old| old.session != session) {
                    next_session += 1;
                    artwork.clear();
                    selected = Selected::new(session, next_session).ok();
                }
                if let Some(state) = &mut selected {
                    state.update(&info, &mut artwork, &mut apple);
                    if let Some(req) = request.take() {
                        state.dispatch(req, &info, apple.as_ref());
                    }
                    wait = if state.metadata.is_some() || artwork.is_loading() {
                        LOADING_POLL
                    } else if state.snapshot.playback == Playback::Playing || state.seek.is_some() {
                        PLAYING_POLL
                    } else if state.snapshot.playback == Playback::Paused
                        && !state.snapshot.title.is_empty()
                    {
                        PAUSED_POLL
                    } else {
                        IDLE_POLL
                    };
                    publish(shared, ctx, state.snapshot.clone());
                }
            }
            unavailable => {
                if let Err(error) = unavailable {
                    manager = None;
                    manager_error = Some(format!("Список медиасессий Windows недоступен: {error}"));
                    manager_retry_at = Instant::now() + MANAGER_RETRY;
                }
                if selected.take().is_some() {
                    artwork.reset();
                }
                let now = Instant::now();
                if shell_manager.is_none() && now >= shell_retry_at {
                    shell_manager = NowPlaying::connect().ok();
                    shell_retry_at = now + MANAGER_RETRY;
                }
                if let Some(shell) = &shell_manager {
                    if shell.select_apple(&mut shell_session).is_err() {
                        shell_session = None;
                        shell_manager = None;
                        shell_retry_at = now + MANAGER_RETRY;
                    }
                }
                let shell_track = shell_session.as_ref().and_then(AppleSession::read);
                let apple = apple.get_or_insert_with(AppleMusicUIA::new);
                let state = apple.poll(AppleSource::Music, shell_track.is_none());
                if shell_track.is_some() || state.track.is_some() {
                    let playback = fallback.get_or_insert_with(|| {
                        next_session += 1;
                        ApplePlayback::new(next_session)
                    });
                    playback.update(&state, shell_track.as_ref(), &mut artwork);
                    if let Some(req) = request.take() {
                        playback.dispatch(
                            req,
                            apple,
                            shell_session.as_ref().zip(shell_track.as_ref()),
                        );
                        wait = LOADING_POLL;
                    } else {
                        wait = APPLE_POLL;
                    }
                    publish(shared, ctx, playback.snapshot.clone());
                } else {
                    if fallback.take().is_some() {
                        artwork.reset();
                    }
                    publish(
                        shared,
                        ctx,
                        MediaSnapshot {
                            error: manager_error.clone(),
                            ..Default::default()
                        },
                    );
                    if manager_request.is_some() {
                        wait = PLAYING_POLL;
                    }
                }
            }
        }
        match rx.recv_timeout(wait) {
            Ok(req) => request = Some(req),
            Err(mpsc::RecvTimeoutError::Timeout) => request = None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    artwork.clear();
}

fn poll_manager(
    request: &mut Option<(IAsyncOperation<Manager>, Instant)>,
    now: Instant,
    retry_at: Instant,
) -> Option<Result<Manager>> {
    if request.is_none() && now >= retry_at {
        match Manager::RequestAsync() {
            Ok(operation) => *request = Some((operation, now + MANAGER_TIMEOUT)),
            Err(error) => return Some(Err(error)),
        }
    }
    let (operation, deadline) = request.as_ref()?;
    let result = match operation.Status() {
        Ok(AsyncStatus::Started) if now < *deadline => return None,
        Ok(AsyncStatus::Started) => {
            let _ = operation.Cancel();
            Err(Error::from_hresult(TIMEOUT))
        }
        Ok(_) => operation.GetResults(),
        Err(error) => Err(error),
    };
    *request = None;
    Some(result)
}

struct ApplePlayback {
    snapshot: MediaSnapshot,
    timeline: TimelineClock,
    seek: Option<PendingSeek>,
    thumbnail: Option<IRandomAccessStreamReference>,
    next_thumbnail: Instant,
}

impl ApplePlayback {
    fn new(session: u64) -> Self {
        Self {
            snapshot: MediaSnapshot {
                active: true,
                session,
                app_id: AppleSource::MUSIC_APP_ID.into(),
                ..Default::default()
            },
            timeline: TimelineClock::default(),
            seek: None,
            thumbnail: None,
            next_thumbnail: Instant::now(),
        }
    }

    fn update(
        &mut self,
        state: &apple_music::State,
        shell_track: Option<&now_playing::Track>,
        artwork: &mut ArtworkLoader,
    ) {
        let now = Instant::now();
        let snapshot = &mut self.snapshot;
        let (title, artist, album, playback) = if let Some(track) = shell_track {
            (
                track.title.as_str(),
                track.artist.as_str(),
                track.album.as_str(),
                track.info.playback(),
            )
        } else if let Some(track) = &state.track {
            (
                track.title.as_str(),
                track.byline.as_str(),
                "",
                if track.playing {
                    Playback::Playing
                } else {
                    Playback::Paused
                },
            )
        } else {
            return;
        };
        if (
            snapshot.title.as_str(),
            snapshot.artist.as_str(),
            snapshot.album.as_str(),
        ) != (title, artist, album)
        {
            snapshot.title = title.to_owned();
            snapshot.artist = artist.to_owned();
            snapshot.album = album.to_owned();
            snapshot.track += 1;
            self.timeline = TimelineClock::default();
            self.seek = None;
            self.thumbnail = None;
            self.next_thumbnail = now;
        }
        snapshot.playback = playback;
        let info = shell_track.map(|track| &track.info);
        let supports = |flag| info.is_some_and(|info| info.supports(flag));
        snapshot.can_play =
            supports(now_playing::PLAY | now_playing::TOGGLE) || state.can_play_pause;
        snapshot.can_pause =
            supports(now_playing::PAUSE | now_playing::TOGGLE) || state.can_play_pause;
        snapshot.can_previous = supports(now_playing::PREVIOUS) || state.can_previous;
        snapshot.can_next = supports(now_playing::NEXT) || state.can_next;
        snapshot.can_shuffle = supports(now_playing::SHUFFLE) || state.shuffle.is_some();
        snapshot.shuffle = info
            .and_then(now_playing::PlaybackInfo::shuffle)
            .or(state.shuffle)
            .unwrap_or(false);
        snapshot.can_repeat = supports(now_playing::REPEAT) || state.can_repeat;
        snapshot.repeat = info
            .and_then(now_playing::PlaybackInfo::repeat)
            .or(state.repeat)
            .unwrap_or(0);
        let rate = f64::from(playback == Playback::Playing);
        if let Some(track) = shell_track {
            let raw = &track.timeline;
            let timeline = Timeline {
                start: raw.start,
                end: raw.end,
                position: raw.position,
                min_seek: raw.min_seek,
                max_seek: raw.max_seek,
                updated_at: raw.updated_at,
            };
            snapshot.duration = timeline.duration();
            snapshot.seek_range = supports(now_playing::SEEK)
                .then(|| timeline.seek_range())
                .flatten()
                .or_else(|| {
                    (state.can_seek && snapshot.duration > 0.0).then_some((0.0, snapshot.duration))
                });
            self.timeline.update(
                raw.position,
                timeline.position(),
                timeline.age(SystemTime::now()),
                rate,
                now,
                snapshot.duration,
            );
            if now >= self.next_thumbnail {
                self.thumbnail = track.thumbnail();
                self.next_thumbnail = now
                    + if self.thumbnail.is_some() {
                        Duration::from_secs(1)
                    } else {
                        APPLE_POLL
                    };
            }
        } else if let Some((position, duration)) =
            state.track.as_ref().and_then(|track| track.timeline)
        {
            snapshot.duration = duration;
            snapshot.seek_range = state.can_seek.then_some((0.0, duration));
            self.timeline.update(
                (position * TICKS_PER_SECOND) as i64,
                position,
                0.0,
                rate,
                now,
                duration,
            );
        } else {
            snapshot.duration = 0.0;
            snapshot.seek_range = None;
            self.timeline = TimelineClock::default();
            self.seek = None;
        }
        snapshot.artwork = artwork.update(
            snapshot.track_key().into(),
            shell_track.and(self.thumbnail.clone()),
            now,
        );
        update_seek(snapshot, &mut self.timeline, &mut self.seek, now);
    }

    fn dispatch(
        &mut self,
        request: Request,
        apple: &AppleMusicUIA,
        shell: Option<(&AppleSession, &now_playing::Track)>,
    ) {
        let snapshot = &mut self.snapshot;
        if request.session != snapshot.session {
            return;
        }
        let command = request.command;
        let target = if let MediaCommand::Seek { seconds, id } = command {
            if request.track != snapshot.track {
                return;
            }
            snapshot.handled_seek = id;
            let Some((min, max)) = snapshot.seek_range else {
                return;
            };
            Some(seconds.clamp(min, max))
        } else {
            None
        };
        if request.queued_at.elapsed() >= ASYNC_TIMEOUT {
            return;
        }
        let accepted = shell
            .and_then(|(session, track)| session.command(command, track, target))
            .unwrap_or_else(|| apple.command(AppleSource::Music, command, target));
        if accepted {
            if let Some(target) = target {
                self.seek = Some(PendingSeek::begin(
                    target,
                    &request,
                    &mut self.timeline,
                    snapshot,
                    Instant::now(),
                ));
            } else if matches!(command, MediaCommand::Next | MediaCommand::Previous) {
                snapshot.track += 1;
                self.seek = None;
                snapshot.position = self.timeline.position;
                self.next_thumbnail = Instant::now();
            }
        }
    }
}

fn publish_error(shared: &Mutex<MediaSnapshot>, ctx: &egui::Context, error: String) {
    publish(
        shared,
        ctx,
        MediaSnapshot {
            error: Some(error),
            ..Default::default()
        },
    );
}

fn publish(shared: &Mutex<MediaSnapshot>, ctx: &egui::Context, mut snapshot: MediaSnapshot) {
    let Ok(mut previous) = shared.lock() else {
        return;
    };
    let same_art = match (&previous.artwork, &snapshot.artwork) {
        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
        (None, None) => true,
        _ => false,
    };
    let same_state = previous.session == snapshot.session
        && previous.track == snapshot.track
        && previous.playback == snapshot.playback
        && previous.duration == snapshot.duration
        && previous.can_play == snapshot.can_play
        && previous.can_pause == snapshot.can_pause
        && previous.can_previous == snapshot.can_previous
        && previous.can_next == snapshot.can_next
        && previous.can_shuffle == snapshot.can_shuffle
        && previous.can_repeat == snapshot.can_repeat
        && previous.seek_range == snapshot.seek_range
        && previous.shuffle == snapshot.shuffle
        && previous.repeat == snapshot.repeat
        && previous.handled_seek == snapshot.handled_seek
        && previous.error == snapshot.error
        && same_art;
    let same_position = !snapshot.active || previous.position == snapshot.position;
    if same_state && same_position {
        return;
    }
    let now = Instant::now();
    let repaint = !same_state
        || snapshot.playback != Playback::Playing
        || (snapshot.position_at(now) - previous.position_at(now)).abs() > 0.35;
    snapshot.observed_at = now;
    *previous = snapshot;
    drop(previous);
    if repaint {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

fn pick_session(
    mgr: &Manager,
    selected: Option<&Selected>,
    preferences: &Mutex<MediaPreferences>,
    ctx: &egui::Context,
) -> Result<Option<(Session, PlaybackInfo)>> {
    let sessions = mgr.GetSessions()?;
    let current = mgr.GetCurrentSession().ok();
    let priority_app = preferences.lock().ok().and_then(|p| p.priority_app.clone());
    let mut apps = Vec::new();
    let mut best = None;
    let mut best_rank = None;
    for i in 0..sessions.Size()? {
        let Ok(session) = sessions.GetAt(i) else {
            continue;
        };
        let Ok(info) = session.GetPlaybackInfo() else {
            continue;
        };
        let app_id = session
            .SourceAppUserModelId()
            .ok()
            .map(|id| id.to_string())
            .unwrap_or_default();
        if !app_id.is_empty() {
            apps.push(app_id.clone());
        }
        let previous = selected.filter(|old| old.session == session);
        let rank = match info.PlaybackStatus() {
            Ok(Status::Playing) => 2,
            Ok(Status::Changing) => {
                match previous.map_or(Playback::Paused, |old| old.snapshot.playback) {
                    Playback::Playing => 2,
                    Playback::Paused => 1,
                    Playback::Stopped => 0,
                }
            }
            Ok(Status::Paused) => 1,
            Ok(Status::Opened | Status::Stopped) => 0,
            _ => continue,
        };
        let rank = (
            is_priority(&app_id, priority_app.as_deref()),
            rank,
            previous.is_some(),
            current.as_ref() == Some(&session),
        );
        if best_rank.is_none_or(|old| rank > old) {
            best_rank = Some(rank);
            best = Some((session, info));
        }
    }
    apps.sort();
    apps.dedup();
    if let Ok(mut preferences) = preferences.lock() {
        if preferences.apps != apps {
            preferences.apps = apps;
            ctx.request_repaint_of(egui::ViewportId::from_hash_of("settings"));
        }
    }
    Ok(best)
}

fn is_priority(app_id: &str, priority: Option<&str>) -> bool {
    priority.is_some_and(|priority| !priority.is_empty() && app_id.eq_ignore_ascii_case(priority))
}

struct Selected {
    session: Session,
    snapshot: MediaSnapshot,
    metadata: Option<(IAsyncOperation<Properties>, Instant)>,
    next_metadata: Instant,
    metadata_changed: Arc<AtomicBool>,
    metadata_token: Option<EventRegistrationToken>,
    refresh_artwork: bool,
    thumbnail: Option<IRandomAccessStreamReference>,
    timeline: TimelineClock,
    seek: Option<PendingSeek>,
}

impl Selected {
    fn new(session: Session, id: u64) -> Result<Self> {
        let app_id = session.SourceAppUserModelId()?.to_string();
        let metadata_changed = Arc::new(AtomicBool::new(false));
        let dirty = metadata_changed.clone();
        let metadata_token = session
            .MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                dirty.store(true, Ordering::Relaxed);
                Ok(())
            }))
            .ok();
        Ok(Self {
            session,
            snapshot: MediaSnapshot {
                active: true,
                session: id,
                app_id,
                ..Default::default()
            },
            metadata: None,
            next_metadata: Instant::now(),
            metadata_changed,
            metadata_token,
            refresh_artwork: false,
            thumbnail: None,
            timeline: TimelineClock::default(),
            seek: None,
        })
    }

    fn update(
        &mut self,
        info: &PlaybackInfo,
        artwork: &mut ArtworkLoader,
        apple: &mut Option<AppleMusicUIA>,
    ) {
        let now = Instant::now();
        let previous_playback = self.snapshot.playback;
        self.snapshot.playback = match info.PlaybackStatus() {
            Ok(Status::Playing) => Playback::Playing,
            Ok(Status::Paused) => Playback::Paused,
            Ok(Status::Changing) | Err(_) => previous_playback,
            _ => Playback::Stopped,
        };
        let rate = if self.snapshot.playback == Playback::Playing {
            info.PlaybackRate()
                .ok()
                .and_then(|value| value.Value().ok())
                .filter(|rate| rate.is_finite() && *rate > 0.0)
                .unwrap_or(1.0)
        } else {
            0.0
        };
        if self.snapshot.playback != previous_playback {
            self.next_metadata = now;
        }
        self.update_properties(now, artwork);
        let source = AppleSource::from_id(&self.snapshot.app_id);
        let fallback = source
            .map(|source| {
                apple
                    .get_or_insert_with(AppleMusicUIA::new)
                    .poll(source, false)
            })
            .unwrap_or_default();
        let controls = info.Controls().ok();
        let toggle = controls
            .as_ref()
            .is_some_and(|c| c.IsPlayPauseToggleEnabled().unwrap_or(false));
        self.snapshot.can_play = toggle
            || fallback.can_play_pause
            || controls
                .as_ref()
                .is_some_and(|c| c.IsPlayEnabled().unwrap_or(false));
        self.snapshot.can_pause = toggle
            || fallback.can_play_pause
            || controls
                .as_ref()
                .is_some_and(|c| c.IsPauseEnabled().unwrap_or(false));
        self.snapshot.can_previous = controls
            .as_ref()
            .is_some_and(|c| c.IsPreviousEnabled().unwrap_or(false));
        self.snapshot.can_next = controls
            .as_ref()
            .is_some_and(|c| c.IsNextEnabled().unwrap_or(false));
        let native_shuffle = controls
            .as_ref()
            .is_some_and(|c| c.IsShuffleEnabled().unwrap_or(false));
        let native_repeat = controls
            .as_ref()
            .is_some_and(|c| c.IsRepeatEnabled().unwrap_or(false));
        self.snapshot.can_shuffle = native_shuffle || fallback.shuffle.is_some();
        self.snapshot.can_repeat = native_repeat || fallback.can_repeat;
        let reported_shuffle = info.IsShuffleActive().ok().and_then(|v| v.Value().ok());
        let reported_repeat = info
            .AutoRepeatMode()
            .ok()
            .and_then(|v| v.Value().ok())
            .map(|mode| match mode {
                Repeat::List => 1,
                Repeat::Track => 2,
                _ => 0,
            });
        self.snapshot.shuffle = if native_shuffle {
            reported_shuffle.or(fallback.shuffle)
        } else {
            fallback.shuffle.or(reported_shuffle)
        }
        .unwrap_or(false);
        self.snapshot.repeat = if native_repeat {
            reported_repeat.or(fallback.repeat)
        } else {
            fallback.repeat.or(reported_repeat)
        }
        .unwrap_or(0);
        self.snapshot.seek_range = None;
        if let Ok(timeline) = Timeline::read(&self.session) {
            let duration = timeline.duration();
            self.snapshot.duration = duration;
            let native_seek = controls
                .as_ref()
                .is_some_and(|c| c.IsPlaybackPositionEnabled().unwrap_or(false));
            self.snapshot.seek_range = native_seek
                .then(|| timeline.seek_range())
                .flatten()
                .or_else(|| (fallback.can_seek && duration > 0.0).then_some((0.0, duration)));
            let raw = timeline.position();
            let age = timeline.age(SystemTime::now());
            self.timeline
                .update(timeline.position, raw, age, rate, now, duration);
        } else if rate != self.timeline.rate {
            self.timeline.change_rate(rate, now, self.snapshot.duration);
        }
        update_seek(&mut self.snapshot, &mut self.timeline, &mut self.seek, now);
        self.snapshot.artwork = artwork.update(
            self.snapshot.track_key().into(),
            self.thumbnail.clone(),
            now,
        );
    }

    fn update_properties(&mut self, now: Instant, artwork: &mut ArtworkLoader) {
        if self.metadata.is_none() && self.metadata_changed.swap(false, Ordering::Relaxed) {
            self.next_metadata = now;
            self.refresh_artwork = true;
        }
        if self.metadata.is_none() && now >= self.next_metadata {
            self.metadata = self
                .session
                .TryGetMediaPropertiesAsync()
                .ok()
                .map(|op| (op, now));
            self.next_metadata = now + Duration::from_millis(600);
        }
        let Some((op, started)) = &self.metadata else {
            return;
        };
        let properties = match op.Status() {
            Ok(AsyncStatus::Completed) => op.GetResults().ok(),
            Ok(AsyncStatus::Started) if now.duration_since(*started) < ASYNC_TIMEOUT => return,
            Ok(AsyncStatus::Started) => {
                let _ = op.Cancel();
                None
            }
            _ => None,
        };
        self.metadata = None;
        if let Some(props) = properties {
            let title = props.Title().map(|v| v.to_string()).unwrap_or_default();
            let artist = props.Artist().map(|v| v.to_string()).unwrap_or_default();
            let album = props
                .AlbumTitle()
                .map(|v| v.to_string())
                .unwrap_or_default();
            if (title.as_str(), artist.as_str(), album.as_str())
                != (
                    &self.snapshot.title[..],
                    &self.snapshot.artist[..],
                    &self.snapshot.album[..],
                )
            {
                self.snapshot.title = title;
                self.snapshot.artist = artist;
                self.snapshot.album = album;
                self.snapshot.track += 1;
                self.timeline = TimelineClock::default();
                self.snapshot.duration = 0.0;
                self.seek = None;
            }
            self.thumbnail = props.Thumbnail().ok();
            if self.refresh_artwork {
                artwork.refresh(now);
                self.refresh_artwork = false;
            }
            if self.metadata_token.is_some() && self.thumbnail.is_some() {
                self.next_metadata = now + Duration::from_secs(2);
            }
        }
    }

    fn dispatch(&mut self, req: Request, info: &PlaybackInfo, apple: Option<&AppleMusicUIA>) {
        if req.session != self.snapshot.session {
            return;
        }
        let command = req.command;
        let seek = if let MediaCommand::Seek { seconds, id } = command {
            if req.track != self.snapshot.track {
                return;
            }
            self.snapshot.handled_seek = id;
            Some(seconds)
        } else {
            None
        };
        if req.queued_at.elapsed() >= ASYNC_TIMEOUT {
            return;
        }
        let seek_target = if let Some(seconds) = seek {
            let Some((min, max)) = self.snapshot.seek_range else {
                return;
            };
            Some(seconds.clamp(min, max))
        } else {
            None
        };
        let accepted = run_command(
            &self.session,
            &self.snapshot,
            info,
            command,
            seek_target,
            apple,
        );
        if accepted {
            if let Some(target) = seek_target {
                self.seek = Some(PendingSeek::begin(
                    target,
                    &req,
                    &mut self.timeline,
                    &mut self.snapshot,
                    Instant::now(),
                ));
            }
            if matches!(command, MediaCommand::Next | MediaCommand::Previous) {
                // Any queued seek from the previous track has lost its target.
                self.snapshot.track += 1;
                self.seek = None;
                self.snapshot.position = self.timeline.position;
                self.next_metadata = Instant::now();
                if let Some((op, _)) = self.metadata.take() {
                    let _ = op.Cancel();
                }
            }
        } else {
            crate::append_log(&format!(
                "SMTC {} rejected {command:?}",
                self.snapshot.app_id
            ));
        }
    }
}

impl Drop for Selected {
    fn drop(&mut self) {
        if let Some(token) = self.metadata_token {
            let _ = self.session.RemoveMediaPropertiesChanged(token);
        }
        if let Some((op, _)) = &self.metadata {
            let _ = op.Cancel();
        }
    }
}

#[derive(Clone, Copy)]
struct Timeline {
    start: i64,
    end: i64,
    position: i64,
    min_seek: i64,
    max_seek: i64,
    updated_at: i64,
}

impl Timeline {
    fn read(session: &Session) -> Result<Self> {
        let tl = session.GetTimelineProperties()?;
        Ok(Self {
            start: tl.StartTime()?.Duration,
            end: tl.EndTime()?.Duration,
            position: tl.Position()?.Duration,
            min_seek: tl.MinSeekTime().map_or(0, |t| t.Duration),
            max_seek: tl.MaxSeekTime().map_or(0, |t| t.Duration),
            updated_at: tl.LastUpdatedTime().map_or(0, |t| t.UniversalTime),
        })
    }

    fn relative(self, ticks: i64) -> f64 {
        ticks.saturating_sub(self.start).max(0) as f64 / TICKS_PER_SECOND
    }

    fn duration(self) -> f64 {
        self.relative(self.end)
    }

    fn position(self) -> f64 {
        self.relative(self.position).min(self.duration())
    }

    fn seek_range(self) -> Option<(f64, f64)> {
        if self.min_seek == 0 && self.max_seek == 0 {
            // Some providers advertise seeking but omit its bounds.
            return (self.duration() > 0.0).then(|| (0.0, self.duration()));
        }
        let min = self.relative(self.min_seek);
        let max = self.relative(self.max_seek).min(self.duration());
        (max > min).then_some((min, max))
    }

    fn seek_ticks(self, seconds: f64) -> Option<i64> {
        let (min, max) = self.seek_range()?;
        Some(
            self.start
                .saturating_add((seconds.clamp(min, max) * TICKS_PER_SECOND).round() as i64),
        )
    }

    fn age(self, wall: SystemTime) -> f64 {
        const UNIX_EPOCH_FILETIME: i64 = 116_444_736_000_000_000;
        if self.updated_at < UNIX_EPOCH_FILETIME {
            return 0.0;
        }
        let now = UNIX_EPOCH_FILETIME as i128
            + (wall
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                / 100) as i128;
        (now - i128::from(self.updated_at)).max(0) as f64 / TICKS_PER_SECOND
    }
}

struct TimelineClock {
    raw: Option<i64>,
    relative: f64,
    sample_position: f64,
    rate: f64,
    position: PlaybackPosition,
    pause_settle_until: Option<Instant>,
}

impl Default for TimelineClock {
    fn default() -> Self {
        Self {
            raw: None,
            relative: 0.0,
            sample_position: 0.0,
            rate: 0.0,
            position: PlaybackPosition::new(0.0, Instant::now(), 0.0),
            pause_settle_until: None,
        }
    }
}

impl TimelineClock {
    fn change_rate(&mut self, rate: f64, now: Instant, duration: f64) {
        self.pause_settle_until = (self.rate > 0.0 && rate == 0.0).then(|| now + PAUSE_SETTLE);
        self.position = PlaybackPosition::new(self.position.value(now, duration), now, rate);
        self.rate = rate;
    }

    fn update(&mut self, ticks: i64, raw: f64, age: f64, rate: f64, now: Instant, duration: f64) {
        let previous_rate = self.rate;
        if previous_rate != rate {
            // The playback status can change before the provider updates its timeline.
            self.change_rate(rate, now, duration);
        }
        if self.raw == Some(ticks) && self.relative == raw {
            return;
        }
        let previous = self.position.value(now, duration);
        let resumed = self.raw.is_some() && previous_rate == 0.0 && rate > 0.0;
        let projected = if resumed { raw } else { raw + age * rate };
        let target = if projected > duration + 1.25 {
            raw
        } else {
            projected.min(duration)
        };
        self.sample_position = target;
        let settling_pause = self.pause_settle_until.is_some_and(|until| now < until)
            && (target - previous).abs() <= 1.25;
        if !settling_pause {
            self.position = if self.raw.is_some() && rate > 0.0 {
                self.position.reconcile(target, now, rate, duration)
            } else {
                PlaybackPosition::new(target, now, rate)
            };
            self.pause_settle_until = None;
        }
        self.raw = Some(ticks);
        self.relative = raw;
    }
}

struct PendingSeek {
    target: f64,
    position: PlaybackPosition,
    last_raw: Option<i64>,
    confirmed: bool,
    until: Instant,
}

impl PendingSeek {
    fn begin(
        target: f64,
        request: &Request,
        clock: &mut TimelineClock,
        snapshot: &mut MediaSnapshot,
        now: Instant,
    ) -> Self {
        clock.pause_settle_until = None;
        let position = PlaybackPosition::new(target, request.queued_at, request.playback_rate)
            .with_rate(clock.rate, now, snapshot.duration);
        snapshot.position = position;
        Self {
            target,
            position,
            last_raw: clock.raw,
            confirmed: false,
            until: now + SEEK_TIMEOUT,
        }
    }

    fn preview(
        &mut self,
        clock: &mut TimelineClock,
        now: Instant,
        duration: f64,
    ) -> Option<PlaybackPosition> {
        self.position = self.position.with_rate(clock.rate, now, duration);
        let expected = self.position.value(now, duration);
        if clock.raw.is_some() && clock.raw != self.last_raw {
            self.last_raw = clock.raw;
            let sample = clock.sample_position;
            let reference = if self.confirmed {
                self.position.uncorrected_value(now, duration)
            } else {
                self.target
            };
            let lower = reference.min(expected);
            let upper = reference.max(expected);
            if (lower - SEEK_TOLERANCE..=upper + SEEK_TOLERANCE).contains(&sample) {
                if clock.rate > 0.0 {
                    self.position = PlaybackPosition::smooth_to(sample, now, clock.rate, expected);
                }
                if !self.confirmed {
                    self.confirmed = true;
                    self.until = now + SEEK_SETTLE;
                }
            }
        }
        if self.confirmed {
            // The provider can still publish a sample from before the seek after its first
            // confirmation. Keep the clock on the confirmed trajectory through the handoff.
            clock.position = self.position;
        }
        (now < self.until).then_some(self.position)
    }
}

fn update_seek(
    snapshot: &mut MediaSnapshot,
    clock: &mut TimelineClock,
    seek: &mut Option<PendingSeek>,
    now: Instant,
) {
    let preview = seek
        .as_mut()
        .and_then(|seek| seek.preview(clock, now, snapshot.duration));
    snapshot.position = preview.unwrap_or(clock.position);
    if preview.is_none() {
        *seek = None;
    }
}

fn run_command(
    session: &Session,
    snapshot: &MediaSnapshot,
    info: &PlaybackInfo,
    command: MediaCommand,
    seek: Option<f64>,
    apple: Option<&AppleMusicUIA>,
) -> bool {
    let controls = info.Controls().ok();
    let source = AppleSource::from_id(&snapshot.app_id);
    let result = match command {
        MediaCommand::Play | MediaCommand::Pause => {
            let play = matches!(command, MediaCommand::Play);
            if (play && snapshot.playback == Playback::Playing)
                || (!play && snapshot.playback == Playback::Paused)
            {
                return true;
            }
            let mut result = Ok(false);
            if controls.as_ref().is_some_and(|c| {
                (if play {
                    c.IsPlayEnabled()
                } else {
                    c.IsPauseEnabled()
                })
                .unwrap_or(false)
            }) {
                result = if play {
                    session.TryPlayAsync()
                } else {
                    session.TryPauseAsync()
                }
                .and_then(get_async);
            }
            if matches!(result, Ok(false))
                && info
                    .PlaybackStatus()
                    .is_ok_and(|status| status != Status::Changing)
                && controls
                    .as_ref()
                    .is_some_and(|c| c.IsPlayPauseToggleEnabled().unwrap_or(false))
            {
                result = session.TryTogglePlayPauseAsync().and_then(get_async);
            }
            result
        }
        MediaCommand::Previous if snapshot.can_previous => {
            session.TrySkipPreviousAsync().and_then(get_async)
        }
        MediaCommand::Next if snapshot.can_next => session.TrySkipNextAsync().and_then(get_async),
        MediaCommand::Seek { .. } => {
            if controls
                .as_ref()
                .is_some_and(|c| c.IsPlaybackPositionEnabled().unwrap_or(false))
            {
                match seek.and_then(|seconds| Timeline::read(session).ok()?.seek_ticks(seconds)) {
                    Some(ticks) => session
                        .TryChangePlaybackPositionAsync(ticks)
                        .and_then(get_async),
                    None => Ok(false),
                }
            } else {
                Ok(false)
            }
        }
        MediaCommand::Shuffle(shuffle)
            if controls
                .as_ref()
                .is_some_and(|c| c.IsShuffleEnabled().unwrap_or(false)) =>
        {
            session
                .TryChangeShuffleActiveAsync(shuffle)
                .and_then(get_async)
        }
        MediaCommand::Repeat(repeat)
            if controls
                .as_ref()
                .is_some_and(|c| c.IsRepeatEnabled().unwrap_or(false)) =>
        {
            session
                .TryChangeAutoRepeatModeAsync(match repeat {
                    1 => Repeat::List,
                    2 => Repeat::Track,
                    _ => Repeat::None,
                })
                .and_then(get_async)
        }
        _ => Ok(false),
    };
    match result {
        Ok(true) => true,
        // A timed out operation may still be in flight. A second toggle could undo it.
        Err(error) if error.code() == TIMEOUT => false,
        Ok(false) | Err(_) => source
            .zip(apple)
            .is_some_and(|(source, apple)| apple.command(source, command, seek)),
    }
}

fn get_async<T: RuntimeType + 'static>(op: IAsyncOperation<T>) -> Result<T> {
    wait_async(op, ASYNC_TIMEOUT)
}

fn wait_async<T: RuntimeType + 'static>(op: IAsyncOperation<T>, timeout: Duration) -> Result<T> {
    if op.Status()? == AsyncStatus::Started {
        let deadline = Instant::now() + timeout;
        let (tx, rx) = mpsc::sync_channel(1);
        let completed = AsyncOperationCompletedHandler::new(move |_, _| {
            let _ = tx.try_send(());
            Ok(())
        });
        let _ = op.SetCompleted(&completed);
        while op.Status()? == AsyncStatus::Started {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = op.Cancel();
                return Err(Error::from_hresult(TIMEOUT));
            }
            if rx
                .recv_timeout(remaining.min(Duration::from_millis(50)))
                .is_ok()
            {
                break;
            }
        }
    }
    op.GetResults()
}

#[cfg(test)]
mod tests {
    #[test]
    fn priority_matches_only_the_selected_app() {
        assert!(super::is_priority("Example.Player", Some("example.player")));
        assert!(!super::is_priority("Other.Player", Some("Example.Player")));
        assert!(!super::is_priority("Example.Player", None));
        assert!(!super::is_priority("", Some("")));
    }

    use super::*;

    #[test]
    fn seek_uses_absolute_ticks_within_the_source_range() {
        let ticks = |seconds: i64| seconds * 10_000_000;
        let timeline = Timeline {
            start: ticks(10),
            end: ticks(110),
            position: ticks(40),
            min_seek: ticks(15),
            max_seek: ticks(90),
            updated_at: 0,
        };
        assert_eq!(timeline.duration(), 100.0);
        assert_eq!(timeline.position(), 30.0);
        assert_eq!(timeline.seek_range(), Some((5.0, 80.0)));
        assert_eq!(timeline.seek_ticks(0.0), Some(ticks(15)));
        assert_eq!(timeline.seek_ticks(12.25), Some(ticks(22) + 2_500_000));
        assert_eq!(timeline.seek_ticks(100.0), Some(ticks(90)));
        assert_eq!(
            Timeline {
                end: ticks(10),
                ..timeline
            }
            .seek_ticks(0.0),
            None
        );
    }

    #[test]
    fn timeline_does_not_rewind_on_coarse_updates_but_honors_external_seeks() {
        let now = Instant::now();
        let mut clock = TimelineClock::default();
        clock.update(100, 10.0, 0.0, 1.0, now, 100.0);
        let later = now + Duration::from_millis(1700);
        let before = clock.position.value(later, 100.0);
        clock.update(110, 11.0, 0.0, 1.0, later, 100.0);
        assert_eq!(clock.position.value(later, 100.0), before);
        let later = later + Duration::from_millis(500);
        assert!(clock.position.value(later, 100.0) > before);
        clock.update(20, 2.0, 0.0, 1.0, later, 100.0);
        assert_eq!(clock.position.value(later, 100.0), 2.0);
    }

    #[test]
    fn pause_and_resume_keep_position_when_the_source_reuses_its_sample() {
        let now = Instant::now();
        let mut clock = TimelineClock::default();
        clock.update(100, 10.0, 0.0, 1.5, now, 100.0);
        let paused = now + Duration::from_secs(2);
        clock.update(100, 10.0, 2.0, 0.0, paused, 100.0);
        let resumed = paused + Duration::from_secs(30);
        assert_eq!(clock.position.value(resumed, 100.0), 13.0);
        clock.update(100, 10.0, 32.0, 1.5, resumed, 100.0);
        assert_eq!(
            clock
                .position
                .value(resumed + Duration::from_secs(2), 100.0),
            16.0
        );
    }

    #[test]
    fn delayed_pause_samples_do_not_move_the_scrubber() {
        let now = Instant::now();
        let mut clock = TimelineClock::default();
        clock.update(100, 10.0, 0.0, 1.0, now, 100.0);
        let paused = now + Duration::from_millis(1750);
        clock.update(110, 11.0, 0.0, 0.0, paused, 100.0);
        let delayed = paused + Duration::from_millis(100);
        clock.update(120, 12.0, 0.0, 0.0, delayed, 100.0);
        assert_eq!(clock.position.value(delayed, 100.0), 11.75);

        let external_seek = paused + PAUSE_SETTLE;
        clock.update(113, 11.3, 0.0, 0.0, external_seek, 100.0);
        assert_eq!(clock.position.value(external_seek, 100.0), 11.3);
    }

    fn seek_request(seconds: f64, queued_at: Instant, playback_rate: f64) -> Request {
        Request {
            session: 1,
            track: 1,
            command: MediaCommand::Seek { seconds, id: 1 },
            queued_at,
            playback_rate,
        }
    }

    #[test]
    fn unconfirmed_seek_expires() {
        let now = Instant::now();
        let mut clock = TimelineClock::default();
        clock.update(100, 10.0, 0.0, 0.0, now, 100.0);
        let mut snapshot = MediaSnapshot {
            duration: 100.0,
            ..Default::default()
        };
        let mut seek = PendingSeek::begin(
            60.0,
            &seek_request(60.0, now, 0.0),
            &mut clock,
            &mut snapshot,
            now,
        );
        assert_eq!(
            seek.preview(&mut clock, now, 100.0)
                .unwrap()
                .value(now, 100.0),
            60.0
        );
        assert!(
            seek.preview(&mut clock, now + SEEK_TIMEOUT, 100.0)
                .is_none()
        );
    }

    #[test]
    fn seek_stays_continuous_through_dispatch_confirmation_and_a_stale_sample() {
        let click = Instant::now();
        let mut clock = TimelineClock::default();
        clock.update(100, 10.0, 0.0, 1.0, click, 100.0);
        let request = seek_request(60.0, click, 1.0);
        let mut snapshot = MediaSnapshot {
            duration: 100.0,
            ..Default::default()
        };

        let dispatched = click + Duration::from_millis(1750);
        let mut seek = Some(PendingSeek::begin(
            60.0,
            &request,
            &mut clock,
            &mut snapshot,
            dispatched,
        ));
        assert_eq!(snapshot.position_at(dispatched), 61.75);

        let confirmed = click + Duration::from_millis(1900);
        let expected = snapshot.position_at(confirmed);
        clock.update(600, 60.0, 0.0, 1.0, confirmed, 100.0);
        update_seek(&mut snapshot, &mut clock, &mut seek, confirmed);
        assert!((snapshot.position_at(confirmed) - expected).abs() < 1e-9);

        let stale = confirmed + Duration::from_millis(100);
        let expected = snapshot.position_at(stale);
        clock.update(120, 12.0, 0.0, 1.0, stale, 100.0);
        update_seek(&mut snapshot, &mut clock, &mut seek, stale);
        assert!((snapshot.position_at(stale) - expected).abs() < 1e-9);

        let settled = confirmed + SEEK_SETTLE;
        let expected = snapshot.position_at(settled);
        update_seek(&mut snapshot, &mut clock, &mut seek, settled);
        assert!(seek.is_none());
        assert!((snapshot.position_at(settled) - expected).abs() < 1e-9);

        let next = settled + Duration::from_millis(100);
        let expected = snapshot.position_at(next);
        clock.update(605, 60.5, 0.0, 1.0, next, 100.0);
        update_seek(&mut snapshot, &mut clock, &mut seek, next);
        assert!((snapshot.position_at(next) - expected).abs() < 1e-9);
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop with SMTC"]
    fn smtc_manager_initializes() {
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.unwrap();
        let started = Instant::now();
        let mut request = None;
        let result = loop {
            if let Some(result) = poll_manager(&mut request, Instant::now(), started) {
                break result.and_then(|manager| manager.GetSessions()?.Size());
            }
            thread::sleep(PLAYING_POLL);
        };
        unsafe { RoUninitialize() };
        assert!(result.is_ok(), "SMTC initialization failed: {result:?}");
    }
}
