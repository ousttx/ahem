//! Modes by application: the user ties a mode to a program, and the "by application" hotkey
//! dictates with that mode while the program is in focus. Only the user's own rules apply; the
//! app never adds or suggests one. The main hotkey, mode hotkeys and the selected-text hotkeys
//! are not affected.

use serde::{Deserialize, Serialize};

use super::modes::Mode;

/// How many programs the list of recent ones keeps.
pub const RECENT_APPS: usize = 12;

/// A program as the OS names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppId {
    /// Stable across launches: the bundle identifier on macOS, the program file on Windows.
    pub id: String,
    /// What the user sees, such as `Telegram`.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRule {
    /// The program's `AppId::id`, or a name the user typed: it matches the id or the name.
    pub app_id: String,
    /// What the interface shows.
    pub app_name: String,
    pub mode_id: String,
}

/// The form two program names are compared in: without the case, the surrounding spaces and a
/// trailing `.exe`, so `telegram` matches `Telegram.exe`.
fn key(text: &str) -> String {
    let text = text.trim().to_lowercase();
    match text.strip_suffix(".exe") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => text,
    }
}

impl AppRule {
    pub fn matches(&self, app: &AppId) -> bool {
        let rule = key(&self.app_id);
        !rule.is_empty() && (rule == key(&app.id) || rule == key(&app.name))
    }
}

/// The mode the "by application" hotkey dictates with in `app`, if a rule names one.
pub fn mode_for<'a>(rules: &'a [AppRule], app: &AppId) -> Option<&'a str> {
    rules.iter().find(|rule| rule.matches(app)).map(|rule| rule.mode_id.as_str())
}

/// Drops the rules that can no longer apply: an empty program, a mode that is gone, and a second
/// rule for the same program (one program, one rule; the first stays).
pub fn tidy(rules: &mut Vec<AppRule>, modes: &[Mode]) {
    let mut seen = Vec::new();
    rules.retain_mut(|rule| {
        rule.app_id = rule.app_id.trim().to_string();
        rule.app_name = rule.app_name.trim().to_string();
        if rule.app_name.is_empty() {
            rule.app_name = rule.app_id.clone();
        }
        let program = key(&rule.app_id);
        let keep = !program.is_empty()
            && modes.iter().any(|mode| mode.id == rule.mode_id)
            && !seen.contains(&program);
        seen.push(program);
        keep
    });
}

/// The programs dictated into lately, the latest first. Kept in memory only, never written to
/// disk: it only helps the user pick a program for a rule.
#[derive(Debug, Default)]
pub struct RecentApps(Vec<AppId>);

impl RecentApps {
    pub fn add(&mut self, app: AppId) {
        self.0.retain(|known| known.id != app.id);
        self.0.insert(0, app);
        self.0.truncate(RECENT_APPS);
    }

    pub fn list(&self) -> Vec<AppId> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::modes::{self, ModeAction};

    fn app(id: &str, name: &str) -> AppId {
        AppId { id: id.into(), name: name.into() }
    }

    fn rule(app_id: &str, mode_id: &str) -> AppRule {
        AppRule { app_id: app_id.into(), app_name: String::new(), mode_id: mode_id.into() }
    }

    fn mode(id: &str) -> Mode {
        Mode {
            id: id.into(),
            name: id.into(),
            action: ModeAction::Plain,
            hotkey: None,
            selection_hotkey: None,
        }
    }

    #[test]
    fn a_rule_matches_the_program_id_or_its_name() {
        let telegram = app("ru.keepcoder.Telegram", "Telegram");
        assert!(rule("ru.keepcoder.Telegram", "short").matches(&telegram));
        assert!(rule(" telegram ", "short").matches(&telegram));
        assert!(!rule("Telegram Lite", "short").matches(&telegram));
        assert!(!rule("", "short").matches(&app("", "")));
    }

    #[test]
    fn a_typed_name_matches_the_program_file() {
        let windows = app("Telegram.exe", "Telegram");
        assert!(rule("telegram", "short").matches(&windows));
        assert!(rule("TELEGRAM.EXE", "short").matches(&windows));
        assert!(rule("Telegram", "short").matches(&app("Telegram.exe", "Telegram Desktop")));
        assert!(!rule(".exe", "short").matches(&app("x.exe", "x")));
    }

    #[test]
    fn the_first_matching_rule_names_the_mode() {
        let rules = [rule("Notepad", "email"), rule("Telegram", "short")];
        assert_eq!(mode_for(&rules, &app("Telegram.exe", "Telegram")), Some("short"));
        assert_eq!(mode_for(&rules, &app("com.apple.TextEdit", "TextEdit")), None);
    }

    #[test]
    fn tidy_drops_rules_of_removed_modes_empty_programs_and_repeats() {
        let modes = [mode(modes::PLAIN), mode("short"), mode("email")];
        let mut rules = vec![
            rule(" Telegram ", "short"),
            rule("Mail", "gone"),
            rule("  ", "email"),
            rule("telegram.exe", "email"),
            AppRule {
                app_id: "com.apple.mail".into(),
                app_name: "Mail".into(),
                mode_id: "email".into(),
            },
        ];
        tidy(&mut rules, &modes);
        assert_eq!(
            rules,
            [
                AppRule {
                    app_id: "Telegram".into(),
                    app_name: "Telegram".into(),
                    mode_id: "short".into()
                },
                AppRule {
                    app_id: "com.apple.mail".into(),
                    app_name: "Mail".into(),
                    mode_id: "email".into()
                },
            ]
        );
    }

    #[test]
    fn recent_apps_keep_the_latest_first_without_repeats() {
        let mut recent = RecentApps::default();
        recent.add(app("a", "A"));
        recent.add(app("b", "B"));
        recent.add(app("a", "A"));
        assert_eq!(recent.list(), [app("a", "A"), app("b", "B")]);
        for index in 0..20 {
            recent.add(app(&index.to_string(), "x"));
        }
        assert_eq!(recent.list().len(), RECENT_APPS);
        assert_eq!(recent.list()[0].id, "19");
    }
}
