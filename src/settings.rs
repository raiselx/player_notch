use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Russian,
    English,
}

impl Default for Language {
    fn default() -> Self {
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetUserDefaultUILanguage() -> u16;
            }
            // The primary language identifier occupies the low ten bits.
            if unsafe { GetUserDefaultUILanguage() } & 0x03ff == 0x19 {
                return Self::Russian;
            }
        }
        Self::English
    }
}

impl Language {
    pub fn text<'a>(self, russian: &'a str, english: &'a str) -> &'a str {
        match self {
            Self::Russian => russian,
            Self::English => english,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleView {
    #[default]
    Mini,
    Clock,
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotchPosition {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: Language,
    pub mini_notch: bool,
    pub static_bg: bool,
    pub idle_view: IdleView,
    pub hide_fullscreen: bool,
    pub position: NotchPosition,
    pub open_on_hover: bool,
    pub hover_delay_ms: u64,
    pub collapse_delay_ms: u64,
    pub pause_linger_secs: u64,
    pub smooth_resize: bool,
    pub show_clock: bool,
    pub show_date: bool,
    pub artwork_bg: bool,
    pub mini_progress: bool,
    pub scroll_title: bool,
    pub open_source: bool,
    pub priority_app: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::default(),
            mini_notch: true,
            static_bg: false,
            idle_view: IdleView::Mini,
            hide_fullscreen: false,
            position: NotchPosition::Center,
            open_on_hover: true,
            hover_delay_ms: 0,
            collapse_delay_ms: 180,
            pause_linger_secs: 10,
            smooth_resize: true,
            show_clock: true,
            show_date: true,
            artwork_bg: true,
            mini_progress: true,
            scroll_title: true,
            open_source: true,
            priority_app: None,
        }
    }
}

impl Settings {
    fn path() -> Result<PathBuf, String> {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData/Roaming"))
            })
            .ok_or_else(|| "APPDATA is not available".to_owned())?;
        Ok(appdata.join("PlayerNotch").join("settings.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        self.save_to(&path)
    }

    fn load_from(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_to(&self, path: &Path) -> Result<(), String> {
        let dir = path
            .parent()
            .ok_or_else(|| "Invalid settings path".to_owned())?;
        fs::create_dir_all(dir).map_err(|e| format!("Create settings directory: {e}"))?;
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|e| format!("Serialize settings: {e}"))?;
        let tmp = dir.join(format!("settings.json.{}.tmp", std::process::id()));
        fs::write(&tmp, bytes).map_err(|e| format!("Write settings: {e}"))?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows::{
                Win32::Storage::FileSystem::{
                    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
                },
                core::PCWSTR,
            };
            let src: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let dst: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                MoveFileExW(
                    PCWSTR(src.as_ptr()),
                    PCWSTR(dst.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
                .map_err(|e| format!("Commit settings: {e}"))?;
            }
        }
        #[cfg(not(windows))]
        {
            fs::rename(&tmp, &path).map_err(|e| format!("Commit settings: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{IdleView, Language, NotchPosition, Settings};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn isolated_path() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "player-notch-settings-{}-{stamp}/settings.json",
            std::process::id()
        ))
    }

    #[test]
    fn old_settings_file_keeps_existing_value_and_defaults_new_fields() {
        let path = isolated_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"mini_notch":false}"#).unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(
            loaded,
            Settings {
                mini_notch: false,
                ..Settings::default()
            }
        );
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn mini_mode_defaults_to_on() {
        assert!(serde_json::from_str::<Settings>("{}").unwrap().mini_notch);
    }

    #[test]
    fn save_to_atomically_round_trips_settings() {
        let path = isolated_path();
        let expected = Settings {
            language: Language::English,
            mini_notch: false,
            static_bg: true,
            idle_view: IdleView::Clock,
            position: NotchPosition::Right,
            hover_delay_ms: 250,
            show_clock: false,
            priority_app: Some("Example.Player".into()),
            ..Settings::default()
        };
        Settings::default().save_to(&path).unwrap();
        expected.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), expected);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
    }
}
