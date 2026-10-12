use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use evdev::KeyCode;
use serde::Deserialize;

use crate::layout::{Action, Groups, Key, Layout};

// The example config is also the built-in default.
const BUILTIN: &str = include_str!("../examples/magicbar.toml");

// These structs are the shape of the TOML file.
#[derive(Deserialize)]
struct KeyFile {
    label: String,
    key: Option<String>,
    command: Option<String>,
}

#[derive(Deserialize)]
struct StripFile {
    collapsed: Vec<KeyFile>,
    expanded: Vec<Vec<KeyFile>>,
}

#[derive(Deserialize)]
struct LayoutFile {
    apps: Vec<String>,
    groups: Vec<Vec<KeyFile>>,
}

#[derive(Deserialize)]
struct ConfigFile {
    esc: Option<KeyFile>,
    strip: StripFile,
    #[serde(default)]
    layouts: HashMap<String, LayoutFile>,
}

// The config once checked and turned into keys.
pub struct Config {
    esc: Key,
    strip: Strip,
    // Window class -> that app's groups.
    apps: HashMap<String, Groups>,
}

struct Strip {
    collapsed: Vec<Key>,
    expanded: Groups,
}

impl Config {
    pub fn dir() -> PathBuf {
        let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
        };
        config_home.join("magicbar")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("magicbar.toml")
    }

    // The user's config if there is one, otherwise the built-in default.
    pub fn load() -> anyhow::Result<Config> {
        let path = Self::path();
        if !path.exists() {
            tracing::info!("no {} yet, using the built-in layout", path.display());
            return Self::parse(BUILTIN);
        }
        let text = std::fs::read_to_string(&path)?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    // The bar to show for a window class. Like Apple's: esc, then the app's
    // keys, then the Control Strip. Expanded, the full strip covers the app's keys.
    pub fn layout_for(&self, class: &str, expanded: bool) -> Layout {
        let mut groups = vec![vec![self.esc.clone()]];
        if expanded {
            groups.push(vec![toggle("󰅙")]);
            groups.extend(self.strip.expanded.clone());
            return Layout { groups, strip: vec![] };
        }
        if let Some(app) = self.apps.get(class) {
            groups.extend(app.clone());
        }
        let mut strip = vec![toggle("")];
        strip.extend(self.strip.collapsed.clone());
        Layout { groups, strip }
    }

    fn parse(text: &str) -> anyhow::Result<Config> {
        let file: ConfigFile = toml::from_str(text)?;

        let esc = match file.esc {
            Some(key) => parse_key(key)?,
            None => Key { label: "esc".to_string(), action: Action::Key(KeyCode::KEY_ESC) },
        };
        let mut collapsed = Vec::new();
        for key in file.strip.collapsed {
            collapsed.push(parse_key(key)?);
        }
        let expanded = parse_groups(file.strip.expanded)?;

        let mut apps = HashMap::new();
        for layout in file.layouts.into_values() {
            let groups = parse_groups(layout.groups)?;
            for app in layout.apps {
                apps.insert(app, groups.clone());
            }
        }
        Ok(Config { esc, strip: Strip { collapsed, expanded }, apps })
    }
}

fn toggle(label: &str) -> Key {
    Key { label: label.to_string(), action: Action::ToggleStrip }
}

fn parse_groups(groups: Vec<Vec<KeyFile>>) -> anyhow::Result<Groups> {
    let mut out = Vec::new();
    for group in groups {
        let mut keys = Vec::new();
        for key in group {
            keys.push(parse_key(key)?);
        }
        out.push(keys);
    }
    Ok(out)
}

fn parse_key(key: KeyFile) -> anyhow::Result<Key> {
    let label = key.label;
    let action = match (key.key, key.command) {
        (Some(name), None) => {
            let code = name
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown key {name} on \"{label}\". Names look like KEY_ESC"))?;
            Action::Key(code)
        }
        (None, Some(command)) => Action::Command(command),
        _ => anyhow::bail!("\"{label}\" needs either key or command, not both or neither"),
    };
    Ok(Key { label, action })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_only(keys: &str) -> String {
        format!("[strip]\ncollapsed = [{keys}]\nexpanded = []\n")
    }

    #[test]
    fn the_example_config_is_valid() {
        Config::parse(BUILTIN).unwrap();
    }

    #[test]
    fn an_unknown_app_gets_esc_and_the_strip() {
        let config = Config::parse(BUILTIN).unwrap();
        let layout = config.layout_for("kitty", false);
        assert_eq!(layout.groups.len(), 1);
        assert_eq!(layout.groups[0][0].label, "esc");
        assert_eq!(layout.strip[0].action, Action::ToggleStrip);
        assert_eq!(layout.strip.len(), 5);
    }

    #[test]
    fn a_known_app_gets_its_keys_in_the_middle() {
        let config = Config::parse(BUILTIN).unwrap();
        let layout = config.layout_for("firefox", false);
        assert_eq!(layout.groups[1][0].action, Action::Key(KeyCode::KEY_BACK));
        assert_eq!(layout.strip.len(), 5);
    }

    #[test]
    fn the_expanded_strip_replaces_the_app_keys() {
        let config = Config::parse(BUILTIN).unwrap();
        let layout = config.layout_for("firefox", true);
        assert_eq!(layout.groups[0][0].label, "esc");
        assert_eq!(layout.groups[1][0].action, Action::ToggleStrip);
        assert!(layout.keys().all(|k| k.action != Action::Key(KeyCode::KEY_BACK)));
        assert!(layout.strip.is_empty());
    }

    #[test]
    fn esc_defaults_to_the_escape_key() {
        let config = Config::parse(&strip_only("")).unwrap();
        assert_eq!(config.esc.action, Action::Key(KeyCode::KEY_ESC));
    }

    #[test]
    fn a_key_name_must_exist() {
        let result = Config::parse(&strip_only(r#"{ label = "x", key = "KEY_NOPE" }"#));
        let message = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("KEY_NOPE"), "got: {message}");
    }

    #[test]
    fn a_key_needs_exactly_one_action() {
        assert!(Config::parse(&strip_only(r#"{ label = "x" }"#)).is_err());
        assert!(Config::parse(&strip_only(r#"{ label = "x", key = "KEY_ESC", command = "ls" }"#)).is_err());
        assert!(Config::parse(&strip_only(r#"{ label = "x", command = "ls" }"#)).is_ok());
    }

    #[test]
    fn the_strip_table_is_required() {
        assert!(Config::parse("").is_err());
    }
}
