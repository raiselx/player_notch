//! Apple Music UI Automation runs on a separate COM worker.
use crate::model::MediaCommand;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{Foundation::HWND, UI::Accessibility::*};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleSource {
    Music,
    ITunes,
}

impl AppleSource {
    pub const MUSIC_APP_ID: &str = "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App";

    pub fn from_id(id: &str) -> Option<Self> {
        let id = id.to_ascii_lowercase();
        if id.contains("applemusic") {
            Some(Self::Music)
        } else if id.contains("itunes") {
            Some(Self::ITunes)
        } else {
            None
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Music => "\\applemusic.exe",
            Self::ITunes => "\\itunes.exe",
        }
    }
}

#[derive(Clone, Default)]
pub struct State {
    pub can_seek: bool,
    pub can_play_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub shuffle: Option<bool>,
    pub repeat: Option<u8>,
    pub can_repeat: bool,
    pub track: Option<Track>,
}

#[derive(Clone)]
pub struct Track {
    pub title: String,
    pub byline: String,
    pub playing: bool,
    pub timeline: Option<(f64, f64)>,
}

enum Command {
    Poll(AppleSource, bool),
    Seek(AppleSource, f64),
    Shuffle(AppleSource, bool),
    Repeat(AppleSource, u8),
    PlayPause(AppleSource, bool),
    Skip(AppleSource, &'static str),
}

impl Command {
    fn source(&self) -> AppleSource {
        match *self {
            Self::Poll(source, _)
            | Self::Seek(source, _)
            | Self::Shuffle(source, _)
            | Self::Repeat(source, _)
            | Self::PlayPause(source, _)
            | Self::Skip(source, _) => source,
        }
    }
}

type Observation = Option<(AppleSource, Instant, State)>;

pub struct AppleMusicUIA {
    tx: mpsc::SyncSender<Command>,
    state: Arc<Mutex<Observation>>,
    poll_pending: Arc<AtomicBool>,
    last_poll: Option<(AppleSource, bool, Instant)>,
}

impl AppleMusicUIA {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        let state = Arc::new(Mutex::new(None));
        let out = state.clone();
        let poll_pending = Arc::new(AtomicBool::new(false));
        let worker_pending = poll_pending.clone();
        let _ = thread::Builder::new()
            .name("apple-music-uia".into())
            .spawn(move || worker_loop(rx, out, worker_pending));
        Self {
            tx,
            state,
            poll_pending,
            last_poll: None,
        }
    }

    pub fn poll(&mut self, source: AppleSource, read_track: bool) -> State {
        if self.last_poll.is_none_or(|(previous, read_previous, at)| {
            previous != source
                || read_previous != read_track
                || at.elapsed() >= Duration::from_millis(if read_track { 150 } else { 350 })
        }) && !self.poll_pending.swap(true, Ordering::Relaxed)
        {
            if self.tx.try_send(Command::Poll(source, read_track)).is_ok() {
                self.last_poll = Some((source, read_track, Instant::now()));
            } else {
                self.poll_pending.store(false, Ordering::Relaxed);
            }
        }
        self.state(source)
    }

    fn state(&self, source: AppleSource) -> State {
        self.state
            .lock()
            .ok()
            .and_then(|state| {
                let (observed_source, at, state) = state.as_ref()?;
                (*observed_source == source && at.elapsed() < Duration::from_secs(2))
                    .then(|| state.clone())
            })
            .unwrap_or_default()
    }

    pub fn command(&self, source: AppleSource, command: MediaCommand, seek: Option<f64>) -> bool {
        let state = self.state(source);
        let command = match command {
            MediaCommand::Seek { .. } if state.can_seek => {
                let Some(seconds) = seek else { return false };
                Command::Seek(source, seconds)
            }
            MediaCommand::Shuffle(want) if state.shuffle.is_some() => {
                Command::Shuffle(source, want)
            }
            MediaCommand::Repeat(want) if state.can_repeat => Command::Repeat(source, want),
            MediaCommand::Play if state.can_play_pause => Command::PlayPause(source, true),
            MediaCommand::Pause if state.can_play_pause => Command::PlayPause(source, false),
            MediaCommand::Previous if state.can_previous => {
                Command::Skip(source, "TransportControl_SkipBack")
            }
            MediaCommand::Next if state.can_next => {
                Command::Skip(source, "TransportControl_SkipForward")
            }
            _ => return false,
        };
        self.tx.try_send(command).is_ok()
    }
}

fn worker_loop(
    rx: mpsc::Receiver<Command>,
    shared: Arc<Mutex<Observation>>,
    poll_pending: Arc<AtomicBool>,
) {
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        return;
    }
    {
        let mut uia = UiAutomation::new();
        let mut read_track = false;
        while let Ok(cmd) = rx.recv() {
            let source = cmd.source();
            let polling = matches!(cmd, Command::Poll(..));
            uia.select(source);
            let ok = match cmd {
                Command::Poll(_, requested) => {
                    read_track = requested;
                    true
                }
                Command::Seek(_, seconds) => uia.seek(seconds),
                Command::Shuffle(_, want) => uia.shuffle(want),
                Command::Repeat(_, want) => uia.repeat(want),
                Command::PlayPause(_, want) => uia.play_pause(want),
                Command::Skip(_, id) => uia.enabled_element(id).as_ref().is_some_and(invoke),
            };
            if !ok {
                crate::append_log("Apple Music UI Automation rejected a media command");
            }
            let state = uia.read_state(read_track && source == AppleSource::Music);
            if let Ok(mut shared) = shared.lock() {
                *shared = Some((source, Instant::now(), state));
            }
            if polling {
                poll_pending.store(false, Ordering::Relaxed);
            }
        }
    }
    unsafe { CoUninitialize() };
}

struct UiAutomation {
    automation: Option<IUIAutomation2>,
    source: Option<AppleSource>,
    windows: Vec<HWND>,
    scanned_at: Option<Instant>,
    elements: std::collections::HashMap<&'static str, IUIAutomationElement>,
}

impl UiAutomation {
    fn new() -> Self {
        Self {
            automation: None,
            source: None,
            windows: Vec::new(),
            scanned_at: None,
            elements: Default::default(),
        }
    }

    fn select(&mut self, source: AppleSource) {
        if self.source != Some(source)
            || self
                .scanned_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            self.source = Some(source);
            self.windows = apple_windows(source);
            self.scanned_at = Some(Instant::now());
            self.elements.clear();
        }
        self.windows.retain(|hwnd| unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(*hwnd).as_bool()
                && windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(*hwnd).as_bool()
        });
        if self.windows.is_empty() {
            self.elements.clear();
        }
    }

    fn automation(&mut self) -> Option<IUIAutomation2> {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        if self.automation.is_none() {
            self.automation = unsafe {
                let automation: IUIAutomation2 =
                    CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
                automation.SetAutoSetFocus(false).ok()?;
                Some(automation)
            };
        }
        self.automation.clone()
    }

    fn element(&mut self, id: &'static str) -> Option<IUIAutomationElement> {
        if let Some(element) = self.elements.get(id) {
            if unsafe { element.CurrentProcessId() }.is_ok() {
                return Some(element.clone());
            }
            self.elements.remove(id);
        }
        if self.windows.is_empty() {
            return None;
        }
        let auto = self.automation()?;
        let condition = automation_id(&auto, id)?;
        for hwnd in &self.windows {
            if let Ok(element) = unsafe {
                auto.ElementFromHandle(*hwnd)
                    .and_then(|root| root.FindFirst(TreeScope_Descendants, &condition))
            } {
                self.elements.insert(id, element.clone());
                return Some(element);
            }
        }
        None
    }

    fn enabled_element(&mut self, id: &'static str) -> Option<IUIAutomationElement> {
        self.element(id)
            .filter(|el| unsafe { el.CurrentIsEnabled().is_ok_and(|enabled| enabled.as_bool()) })
    }

    fn scrubbers(&mut self) -> impl Iterator<Item = IUIAutomationRangeValuePattern> + '_ {
        ["LCDScrubber", "Scrubber"].into_iter().filter_map(|id| {
            let element = self.enabled_element(id)?;
            let range = unsafe {
                element
                    .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
            }
            .ok()?;
            unsafe {
                range
                    .CurrentIsReadOnly()
                    .is_ok_and(|read_only| !read_only.as_bool())
            }
            .then_some(range)
        })
    }

    fn seek(&mut self, seconds: f64) -> bool {
        self.scrubbers().any(|range| unsafe {
            let (Ok(min), Ok(max)) = (range.CurrentMinimum(), range.CurrentMaximum()) else {
                return false;
            };
            min.is_finite()
                && max.is_finite()
                && max > min
                && range.SetValue(seconds.clamp(min, max)).is_ok()
        })
    }

    fn shuffle_pattern(&mut self) -> Option<IUIAutomationTogglePattern> {
        let el = self.enabled_element("ShuffleButton")?;
        unsafe { el.GetCurrentPatternAs(UIA_TogglePatternId) }.ok()
    }

    fn shuffle(&mut self, want: bool) -> bool {
        self.shuffle_pattern().is_some_and(|pattern| {
            shuffle_state(&pattern)
                .is_some_and(|state| state == want || unsafe { pattern.Toggle().is_ok() })
        })
    }

    fn repeat(&mut self, want: u8) -> bool {
        self.enabled_element("RepeatButton").is_some_and(|el| {
            let mode = repeat_mode(&element_name(&el));
            if mode == Some(want) {
                return true;
            }
            mode.is_some_and(|mode| (mode + 1) % 3 == want) && invoke(&el)
        })
    }

    fn play_pause(&mut self, want: bool) -> bool {
        self.enabled_element("TransportControl_PlayPauseStop")
            .is_some_and(|el| {
                playing_from_action(&element_name(&el))
                    .is_some_and(|playing| playing == want || invoke(&el))
            })
    }

    fn read_state(&mut self, read_track: bool) -> State {
        let scrubber = self.scrubbers().next();
        let play_pause = self.enabled_element("TransportControl_PlayPauseStop");
        let playing = play_pause
            .as_ref()
            .and_then(|el| playing_from_action(&element_name(el)));
        let can_play_pause = playing.is_some() && play_pause.as_ref().is_some_and(can_invoke);
        let shuffle = self.shuffle_pattern().as_ref().and_then(shuffle_state);
        let repeat_button = self.enabled_element("RepeatButton");
        let repeat = repeat_button
            .as_ref()
            .and_then(|el| repeat_mode(&element_name(el)));
        let can_repeat = repeat.is_some() && repeat_button.as_ref().is_some_and(can_invoke);
        let track = if read_track {
            playing.and_then(|playing| self.read_track(playing, scrubber.as_ref()))
        } else {
            None
        };
        State {
            can_seek: scrubber.is_some(),
            can_play_pause,
            can_previous: track.is_some()
                && self
                    .enabled_element("TransportControl_SkipBack")
                    .as_ref()
                    .is_some_and(can_invoke),
            can_next: track.is_some()
                && self
                    .enabled_element("TransportControl_SkipForward")
                    .as_ref()
                    .is_some_and(can_invoke),
            shuffle,
            repeat,
            can_repeat,
            track,
        }
    }

    fn read_track(
        &mut self,
        playing: bool,
        scrubber: Option<&IUIAutomationRangeValuePattern>,
    ) -> Option<Track> {
        let auto = self.automation()?;
        let labels = automation_id(&auto, "ScrollingText")?;
        let lcd = self.element("LCD");
        let (scope, mini) = match lcd {
            Some(lcd) => (lcd, false),
            None => {
                let mini_marker = automation_id(&auto, "ThumbnailHoverButton")?;
                let mini = self.windows.iter().find_map(|hwnd| unsafe {
                    let root = auto.ElementFromHandle(*hwnd).ok()?;
                    root.FindFirst(TreeScope_Descendants, &mini_marker).ok()?;
                    Some(root)
                })?;
                (mini, true)
            }
        };
        let elements = unsafe { scope.FindAll(TreeScope_Descendants, &labels) }.ok()?;
        let mut text = (0..unsafe { elements.Length() }.ok()?).filter_map(|index| {
            let name = unsafe { elements.GetElement(index).ok()?.CurrentName() }.ok()?;
            let name = name.to_string();
            let name = name.trim();
            if mini {
                // The mini player duplicates each scrolling label for its marquee.
                let middle = name.len() / 2;
                if let (Some(first), Some(second)) = (name.get(..middle), name.get(middle..)) {
                    if first == second.trim_start() {
                        return Some(first.to_owned());
                    }
                }
            }
            Some(name.to_owned())
        });
        let title = text.next()?;
        let byline = text.next().unwrap_or_default();
        if title.is_empty()
            || (byline.is_empty()
                && matches!(
                    title.to_lowercase().as_str(),
                    "not playing" | "не исполняется"
                ))
        {
            return None;
        }
        let timeline = scrubber.and_then(|range| unsafe {
            let duration = range.CurrentMaximum().ok()?;
            let position = range.CurrentValue().ok()?;
            (range.CurrentMinimum().ok()? == 0.0
                && duration.is_finite()
                && duration > 0.0
                && position.is_finite())
            .then(|| (position.clamp(0.0, duration), duration))
        });
        Some(Track {
            title,
            byline,
            playing,
            timeline,
        })
    }
}

fn automation_id(auto: &IUIAutomation, id: &str) -> Option<IUIAutomationCondition> {
    unsafe {
        auto.CreatePropertyCondition(
            UIA_AutomationIdPropertyId,
            &windows::core::VARIANT::from(id),
        )
    }
    .ok()
}
fn shuffle_state(pattern: &IUIAutomationTogglePattern) -> Option<bool> {
    let state = unsafe { pattern.CurrentToggleState() }.ok()?;
    if state == ToggleState_On {
        Some(true)
    } else if state == ToggleState_Off {
        Some(false)
    } else {
        None
    }
}

fn element_name(element: &IUIAutomationElement) -> String {
    unsafe { element.CurrentName() }
        .map(|name| name.to_string().to_lowercase())
        .unwrap_or_default()
}

fn can_invoke(element: &IUIAutomationElement) -> bool {
    unsafe {
        element
            .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            .is_ok()
    }
}

fn invoke(element: &IUIAutomationElement) -> bool {
    unsafe {
        element
            .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            .and_then(|pattern| pattern.Invoke())
            .is_ok()
    }
}

fn playing_from_action(name: &str) -> Option<bool> {
    let pause = ["pause", "пауз", "приостан", "stop", "остан"]
        .iter()
        .any(|text| name.contains(text));
    let play = ["play", "воспроиз"]
        .iter()
        .any(|text| name.starts_with(text));
    match (pause, play) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn repeat_mode(name: &str) -> Option<u8> {
    if ["not", "off", "выкл", "без повтор", "не повтор"]
        .iter()
        .any(|text| name.contains(text))
    {
        Some(0)
    } else if ["one", "track", "трек", "один", "одну"]
        .iter()
        .any(|text| name.contains(text))
    {
        Some(2)
    } else if ["all", "list", "все", "список"]
        .iter()
        .any(|text| name.contains(text))
    {
        Some(1)
    } else {
        None
    }
}

fn apple_windows(source: AppleSource) -> Vec<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    struct Found(AppleSource, Vec<HWND>);
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindowVisible};
        if unsafe { !IsWindowVisible(hwnd).as_bool() } {
            return BOOL(1);
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return BOOL(1);
        };
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return BOOL(1);
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle,
                windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .is_ok();
        unsafe {
            let _ = CloseHandle(handle);
        }
        if ok {
            let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
            let found = unsafe { &mut *(lparam.0 as *mut Found) };
            if path.ends_with(found.0.executable()) {
                let found = unsafe { &mut *(lparam.0 as *mut Found) };
                found.1.push(hwnd)
            }
        }
        BOOL(1)
    }
    let mut found = Found(source, Vec::new());
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut _ as isize));
    }
    found.1
}
