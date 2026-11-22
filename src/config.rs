//! Tiny key=value settings file (config.ini) in the folder the user approved at setup.

use std::path::PathBuf;

pub struct Config {
    pub flags: String,
    pub sites: String,
    pub autotune: bool,
    pub on_at_logon: bool,
    /// Seconds between background block checks while ON.
    pub check_secs: u64,
    pub ttl_min: u8,
    pub ttl_max: u8,
    /// Wrap long lines in the Log tab.
    pub log_wrap: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            flags: sidestep_core::flags::DEFAULT_FLAGS.into(),
            sites: "epicgames.com, store.steampowered.com, gog.com".into(),
            autotune: true,
            on_at_logon: false,
            check_secs: 90,
            ttl_min: 1,
            ttl_max: 12,
            log_wrap: false,
        }
    }
}

static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Set once at start-up to the folder the user approved (install or portable).
pub fn set_data_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

pub fn data_dir() -> PathBuf {
    DIR.get().cloned().expect("data dir not chosen yet")
}

fn path() -> PathBuf {
    data_dir().join("config.ini")
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config::default();
        let Ok(text) = std::fs::read_to_string(path()) else { return c };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "flags" => c.flags = v.to_string(),
                "sites" => c.sites = v.to_string(),
                "autotune" => c.autotune = v == "1",
                "on_at_logon" => c.on_at_logon = v == "1",
                "check_secs" => c.check_secs = v.parse().unwrap_or(c.check_secs).max(15),
                "ttl_min" => c.ttl_min = v.parse().unwrap_or(c.ttl_min).max(1),
                "log_wrap" => c.log_wrap = v == "1",
                "ttl_max" => c.ttl_max = v.parse().unwrap_or(c.ttl_max).min(64),
                _ => {}
            }
        }
        if c.ttl_max < c.ttl_min {
            c.ttl_max = c.ttl_min;
        }
        c
    }

    pub fn save(&self) -> Result<(), String> {
        let b = |x: bool| if x { "1" } else { "0" };
        let text = format!(
            "flags={}\nsites={}\nautotune={}\non_at_logon={}\ncheck_secs={}\nttl_min={}\nttl_max={}\nlog_wrap={}\n",
            self.flags,
            self.sites,
            b(self.autotune),
            b(self.on_at_logon),
            self.check_secs,
            self.ttl_min,
            self.ttl_max,
            b(self.log_wrap)
        );
        std::fs::write(path(), text).map_err(|e| format!("saving config: {e}"))
    }
}
