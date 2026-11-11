//! The shell's media-session service is also available through COM on recent Windows.
//! Use it for Apple Music when the public WinRT session manager is unavailable.
use crate::{
    apple_music::AppleSource,
    model::{MediaCommand, Playback},
};
use std::{ffi::c_void, ptr};
use windows::{
    Storage::Streams::{
        IRandomAccessStream, IRandomAccessStreamReference, RandomAccessStreamReference,
    },
    Win32::{
        System::{
            Com::{
                CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree, IStream,
                StructuredStorage::PropVariantToBSTR,
            },
            Variant::VT_STREAM,
            WinRT::{BSOS_DEFAULT, CreateRandomAccessStreamOverStream},
        },
        UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY},
    },
    core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PWSTR, Result},
};

const MANAGER: GUID = GUID::from_u128(0xbcbb9860_c012_4ad7_a938_6e337ae6aba5);
const SUMMARY: GUID = GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9);
const MUSIC: GUID = GUID::from_u128(0x56a3372e_ce9c_11d2_9f0e_006097c686f6);

#[derive(Clone)]
#[repr(transparent)]
pub struct NowPlaying(IUnknown);

#[repr(C)]
pub struct ManagerVtbl {
    base: IUnknown_Vtbl,
    count: usize,
    current: unsafe extern "system" fn(*mut c_void, *mut Option<IUnknown>) -> HRESULT,
    add_remove: [usize; 2],
    sessions:
        unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut Option<IUnknown>) -> HRESULT,
}

unsafe impl Interface for NowPlaying {
    type Vtable = ManagerVtbl;
    const IID: GUID = GUID::from_u128(0x3b6a7908_ce07_4ba9_878c_6e4a15db5e5b);
}

#[derive(Clone, PartialEq, Eq)]
#[repr(transparent)]
struct Session(IUnknown);

#[repr(C)]
struct SessionVtbl {
    base: IUnknown_Vtbl,
    session_type: usize,
    app_id: unsafe extern "system" fn(*mut c_void, *mut PWSTR) -> HRESULT,
    identifiers: [usize; 6],
    activate: unsafe extern "system" fn(*mut c_void, *mut Option<IUnknown>) -> HRESULT,
}

unsafe impl Interface for Session {
    type Vtable = SessionVtbl;
    const IID: GUID = GUID::from_u128(0x431268cf_7477_4285_950b_6f892a944712);
}

#[derive(Clone)]
#[repr(transparent)]
struct Source(IUnknown);

#[repr(C)]
struct SourceVtbl {
    base: IUnknown_Vtbl,
    playback: unsafe extern "system" fn(*mut c_void, *mut PlaybackInfo) -> HRESULT,
    command: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    properties: unsafe extern "system" fn(*mut c_void, *mut Option<IPropertyStore>) -> HRESULT,
    timeline: unsafe extern "system" fn(*mut c_void, *mut Timeline) -> HRESULT,
    events: [usize; 2],
    repeat: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    rate: usize,
    shuffle: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    seek: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
}

unsafe impl Interface for Source {
    type Vtable = SourceVtbl;
    const IID: GUID = GUID::from_u128(0xc4f66b80_df04_4f79_afc2_bee3fc7c46e3);
}

impl NowPlaying {
    pub fn connect() -> Result<Self> {
        unsafe { CoCreateInstance(&MANAGER, None, CLSCTX_LOCAL_SERVER) }
    }

    pub fn select_apple(&self, selected: &mut Option<AppleSession>) -> Result<()> {
        let mut current = None;
        unsafe { (self.vtable().current)(self.as_raw(), &mut current) }.ok()?;
        let session = if let Some(current) = current
            .and_then(|current| current.cast::<Session>().ok())
            .filter(Session::is_apple)
        {
            Some(current)
        } else {
            let mut count = 0;
            let mut list = ptr::null_mut();
            unsafe { (self.vtable().sessions)(self.as_raw(), &mut count, &mut list) }.ok()?;
            let mut apple = None;
            if !list.is_null() {
                // The COM array owns one reference to each returned session.
                for index in 0..count as usize {
                    let session = unsafe { list.add(index).read() };
                    if apple.is_none() {
                        apple = session
                            .and_then(|session| session.cast::<Session>().ok())
                            .filter(Session::is_apple);
                    }
                }
                unsafe { CoTaskMemFree(Some(list.cast())) };
            }
            apple
        };
        if selected.as_ref().map(|old| &old.session) != session.as_ref() {
            *selected = session.and_then(AppleSession::activate);
        }
        Ok(())
    }
}

impl Session {
    fn is_apple(&self) -> bool {
        let mut id = PWSTR::null();
        let result = unsafe { (self.vtable().app_id)(self.as_raw(), &mut id) };
        if result.is_err() || id.is_null() {
            return false;
        }
        let source = unsafe { id.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(id.0.cast())) };
        source.as_deref().and_then(AppleSource::from_id) == Some(AppleSource::Music)
    }
}

pub struct AppleSession {
    session: Session,
    source: Source,
}

impl AppleSession {
    fn activate(session: Session) -> Option<Self> {
        let mut source = None;
        unsafe { (session.vtable().activate)(session.as_raw(), &mut source) }
            .ok()
            .ok()?;
        let source = source?.cast().ok()?;
        Some(Self { session, source })
    }

    pub fn read(&self) -> Option<Track> {
        let source = &self.source;
        let vtable = source.vtable();
        let raw = source.as_raw();
        let mut info = PlaybackInfo::default();
        let mut timeline = Timeline::default();
        let mut properties = None;
        unsafe { (vtable.playback)(raw, &mut info) }.ok().ok()?;
        unsafe { (vtable.timeline)(raw, &mut timeline) }.ok().ok()?;
        unsafe { (vtable.properties)(raw, &mut properties) }
            .ok()
            .ok()?;
        let properties = properties?;
        let title = string(&properties, SUMMARY, 2);
        if title.trim().is_empty() {
            return None;
        }
        Some(Track {
            title,
            artist: string(&properties, MUSIC, 2),
            album: string(&properties, MUSIC, 4),
            info,
            timeline,
            properties,
        })
    }

    pub fn command(&self, command: MediaCommand, track: &Track, seek: Option<f64>) -> Option<bool> {
        let source = &self.source;
        let vtable = source.vtable();
        let raw = source.as_raw();
        let info = &track.info;
        let result = unsafe {
            match command {
                MediaCommand::Play if info.supports(PLAY) => (vtable.command)(raw, 0),
                MediaCommand::Pause if info.supports(PAUSE) => (vtable.command)(raw, 1),
                MediaCommand::Play | MediaCommand::Pause if info.supports(TOGGLE) => {
                    if (info.playback() == Playback::Playing)
                        == matches!(command, MediaCommand::Play)
                    {
                        return Some(true);
                    }
                    (vtable.command)(raw, 10)
                }
                MediaCommand::Next if info.supports(NEXT) => (vtable.command)(raw, 6),
                MediaCommand::Previous if info.supports(PREVIOUS) => (vtable.command)(raw, 7),
                MediaCommand::Repeat(mode) if info.supports(REPEAT) => (vtable.repeat)(
                    raw,
                    match mode {
                        1 => 3,
                        2 => 2,
                        _ => 1,
                    },
                ),
                MediaCommand::Shuffle(enabled) if info.supports(SHUFFLE) => {
                    (vtable.shuffle)(raw, i32::from(enabled))
                }
                MediaCommand::Seek { .. } if info.supports(SEEK) => (vtable.seek)(
                    raw,
                    track
                        .timeline
                        .start
                        .saturating_add((seek? * 10_000_000.0).round() as i64),
                ),
                _ => return None,
            }
        };
        Some(result.is_ok())
    }
}

pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub info: PlaybackInfo,
    pub timeline: Timeline,
    properties: IPropertyStore,
}

impl Track {
    pub fn thumbnail(&self) -> Option<IRandomAccessStreamReference> {
        let value = unsafe {
            self.properties.GetValue(&PROPERTYKEY {
                fmtid: SUMMARY,
                pid: 27,
            })
        }
        .ok()?;
        let raw = unsafe { &value.as_raw().Anonymous.Anonymous };
        if raw.vt != VT_STREAM.0 {
            return None;
        }
        let stream = unsafe { IStream::from_raw_borrowed(&raw.Anonymous.pStream) }?;
        let stream: IRandomAccessStream =
            unsafe { CreateRandomAccessStreamOverStream(stream, BSOS_DEFAULT) }.ok()?;
        RandomAccessStreamReference::CreateFromStream(&stream)
            .ok()?
            .cast()
            .ok()
    }
}

fn string(properties: &IPropertyStore, fmtid: GUID, pid: u32) -> String {
    unsafe {
        properties
            .GetValue(&PROPERTYKEY { fmtid, pid })
            .and_then(|value| PropVariantToBSTR(&value))
            .map(|value| value.to_string())
            .unwrap_or_default()
    }
}

pub const PLAY: u32 = 0x1;
pub const PAUSE: u32 = 0x2;
pub const NEXT: u32 = 0x40;
pub const PREVIOUS: u32 = 0x80;
pub const TOGGLE: u32 = 0x400;
pub const SHUFFLE: u32 = 0x1000;
pub const REPEAT: u32 = 0x2000;
pub const SEEK: u32 = 0x8000;

#[repr(C)]
#[derive(Default)]
pub struct PlaybackInfo {
    valid: u32,
    caps: u32,
    state: u32,
    mode: u32,
    repeat: u32,
    rate: f64,
    shuffle: i32,
    last_playing: i64,
    reserved: [i64; 4],
}

impl PlaybackInfo {
    pub fn supports(&self, capability: u32) -> bool {
        self.valid & 0x1 != 0 && self.caps & capability != 0
    }

    pub fn playback(&self) -> Playback {
        match self.state {
            5 => Playback::Playing,
            6 => Playback::Paused,
            _ => Playback::Stopped,
        }
    }

    pub fn repeat(&self) -> Option<u8> {
        (self.valid & 0x8 != 0).then_some(match self.repeat {
            3 => 1,
            2 => 2,
            _ => 0,
        })
    }

    pub fn shuffle(&self) -> Option<bool> {
        (self.valid & 0x20 != 0).then_some(self.shuffle != 0)
    }
}

#[repr(C)]
#[derive(Default)]
pub struct Timeline {
    pub start: i64,
    pub end: i64,
    pub min_seek: i64,
    pub max_seek: i64,
    pub position: i64,
    pub updated_at: i64,
    reserved: [i64; 4],
}
