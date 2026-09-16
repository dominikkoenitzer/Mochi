//! Matching rules for deciding what to do with a window.
//!
//! The JSON shape is the one of the existing config format, so `ignore_rules`
//! and friends copy across from an existing config file unchanged:
//!
//! ```json
//! { "kind": "Exe", "id": "wallpaper64.exe", "matching_strategy": "Equals" }
//! ```
//!
//! A rule can also be an array of those objects, in which case every element
//! has to match. That is how the community `applications.json` expresses rules
//! like "class equals X **and** title does not contain Y".

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The piece of window metadata a rule looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum ApplicationIdentifier {
    /// The file name of the owning executable, for example `firefox.exe`.
    Exe,
    /// The Win32 window class.
    Class,
    /// The window title.
    Title,
    /// The full path of the owning executable.
    Path,
}

impl std::fmt::Display for ApplicationIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exe => f.write_str("Exe"),
            Self::Class => f.write_str("Class"),
            Self::Title => f.write_str("Title"),
            Self::Path => f.write_str("Path"),
        }
    }
}

/// How the rule's `id` is compared against the window's value.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum MatchingStrategy {
    /// The compatibility fallback used by older rule files.
    ///
    /// The identifier is treated as a regular expression when it contains
    /// regex metacharacters, and as an exact comparison otherwise.
    #[default]
    Legacy,
    /// Exact, case sensitive.
    Equals,
    /// Anything but an exact match.
    DoesNotEqual,
    /// The value begins with the identifier.
    StartsWith,
    /// The value does not begin with the identifier.
    DoesNotStartWith,
    /// The value ends with the identifier.
    EndsWith,
    /// The value does not end with the identifier.
    DoesNotEndWith,
    /// The identifier appears somewhere in the value.
    Contains,
    /// The identifier appears nowhere in the value.
    DoesNotContain,
    /// The identifier is a regular expression that matches somewhere in the value.
    Regex,
}

/// One condition: look at `kind`, compare it with `id` using `matching_strategy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IdWithIdentifier {
    /// Which piece of metadata to look at.
    pub kind: ApplicationIdentifier,
    /// The value to compare against.
    pub id: String,
    /// How to compare. Missing means [`MatchingStrategy::Legacy`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matching_strategy: Option<MatchingStrategy>,
}

impl IdWithIdentifier {
    /// A new condition.
    #[must_use]
    pub fn new(
        kind: ApplicationIdentifier,
        id: impl Into<String>,
        matching_strategy: MatchingStrategy,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            matching_strategy: Some(matching_strategy),
        }
    }

    /// The strategy, defaulting to [`MatchingStrategy::Legacy`].
    #[must_use]
    pub fn strategy(&self) -> MatchingStrategy {
        self.matching_strategy.unwrap_or_default()
    }

    /// Checks the condition against a window.
    #[must_use]
    pub fn matches(&self, window: &WindowInfo<'_>) -> bool {
        let value = window.value_for(self.kind);
        match self.strategy() {
            MatchingStrategy::Equals => value == self.id,
            MatchingStrategy::DoesNotEqual => value != self.id,
            MatchingStrategy::StartsWith => value.starts_with(&self.id),
            MatchingStrategy::DoesNotStartWith => !value.starts_with(&self.id),
            MatchingStrategy::EndsWith => value.ends_with(&self.id),
            MatchingStrategy::DoesNotEndWith => !value.ends_with(&self.id),
            MatchingStrategy::Contains => value.contains(&self.id),
            MatchingStrategy::DoesNotContain => !value.contains(&self.id),
            MatchingStrategy::Regex => regex_matches(&self.id, value),
            MatchingStrategy::Legacy => {
                if looks_like_regex(&self.id) {
                    regex_matches(&self.id, value)
                } else {
                    value == self.id
                }
            }
        }
    }

    /// Reports a broken regular expression, so a config reload can warn about it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRegex`] when the strategy needs a regular
    /// expression and `id` does not compile.
    pub fn validate(&self) -> Result<()> {
        let needs_regex = match self.strategy() {
            MatchingStrategy::Regex => true,
            MatchingStrategy::Legacy => looks_like_regex(&self.id),
            _ => false,
        };
        if needs_regex && compile(&self.id).is_none() {
            return Err(Error::InvalidRegex {
                pattern: self.id.clone(),
                message: regex::Regex::new(&self.id)
                    .err()
                    .map_or_else(|| "unknown".to_string(), |e| e.to_string()),
            });
        }
        Ok(())
    }
}

/// A rule: either one condition or a set of conditions that all have to hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum MatchingRule {
    /// A single condition.
    Simple(IdWithIdentifier),
    /// Several conditions, combined with a logical and.
    Composite(Vec<IdWithIdentifier>),
}

impl MatchingRule {
    /// A single-condition rule.
    #[must_use]
    pub fn simple(
        kind: ApplicationIdentifier,
        id: impl Into<String>,
        matching_strategy: MatchingStrategy,
    ) -> Self {
        Self::Simple(IdWithIdentifier::new(kind, id, matching_strategy))
    }

    /// Checks the rule against a window.
    ///
    /// An empty composite rule never matches.
    #[must_use]
    pub fn matches(&self, window: &WindowInfo<'_>) -> bool {
        match self {
            Self::Simple(id) => id.matches(window),
            Self::Composite(ids) => !ids.is_empty() && ids.iter().all(|id| id.matches(window)),
        }
    }

    /// The conditions this rule is made of.
    #[must_use]
    pub fn conditions(&self) -> &[IdWithIdentifier] {
        match self {
            Self::Simple(id) => std::slice::from_ref(id),
            Self::Composite(ids) => ids,
        }
    }

    /// Reports the first broken regular expression in the rule.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRegex`] when a condition's pattern does not compile.
    pub fn validate(&self) -> Result<()> {
        self.conditions()
            .iter()
            .try_for_each(IdWithIdentifier::validate)
    }
}

/// The metadata of one window, borrowed for the length of a rule check.
///
/// Build it by hand in the daemon or with [`crate::model::Window::info`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowInfo<'a> {
    /// The window title.
    pub title: &'a str,
    /// The Win32 window class.
    pub class: &'a str,
    /// The file name of the owning executable.
    pub exe: &'a str,
    /// The full path of the owning executable.
    pub path: &'a str,
}

impl<'a> WindowInfo<'a> {
    /// All four fields at once.
    #[must_use]
    pub const fn new(title: &'a str, class: &'a str, exe: &'a str, path: &'a str) -> Self {
        Self {
            title,
            class,
            exe,
            path,
        }
    }

    /// The value a rule of that kind compares against.
    #[must_use]
    pub const fn value_for(&self, kind: ApplicationIdentifier) -> &'a str {
        match kind {
            ApplicationIdentifier::Exe => self.exe,
            ApplicationIdentifier::Class => self.class,
            ApplicationIdentifier::Title => self.title,
            ApplicationIdentifier::Path => self.path,
        }
    }
}

/// `true` when any rule in the list matches.
#[must_use]
pub fn matches_any(rules: &[MatchingRule], window: &WindowInfo<'_>) -> bool {
    rules.iter().any(|rule| rule.matches(window))
}

/// Every rule list the window manager consults, in one place.
///
/// The field names are the existing config keys so a config migrates without
/// renaming anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RuleSets {
    /// Windows that are never touched at all.
    pub ignore_rules: Vec<MatchingRule>,
    /// Windows that are managed even though the usual heuristics say no.
    pub manage_rules: Vec<MatchingRule>,
    /// Windows that are managed but never tiled.
    pub floating_applications: Vec<MatchingRule>,
    /// Applications that keep a hidden window alive in the tray, so a
    /// destroyed window does not mean the app is gone.
    pub tray_and_multi_window_applications: Vec<MatchingRule>,
    /// Applications that reuse one window and only change its title, so the
    /// daemon has to listen for name changes to spot a new document.
    pub object_name_change_applications: Vec<MatchingRule>,
    /// Applications whose border sits outside the window rectangle.
    pub border_overflow_applications: Vec<MatchingRule>,
    /// Layered windows that should be managed anyway.
    pub layered_whitelist: Vec<MatchingRule>,
    /// Windows that must never be made transparent.
    pub transparency_ignore_rules: Vec<MatchingRule>,
    /// Applications that need an extra beat before their window is ready.
    pub slow_application_identifiers: Vec<MatchingRule>,
}

/// What the window manager should do with a window, according to the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleDecision {
    /// Leave the window completely alone.
    Ignore,
    /// Manage it, but keep it floating.
    Float,
    /// Manage and tile it.
    Tile,
}

impl RuleSets {
    /// An empty set of rules.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges `other` into `self`, appending every list.
    pub fn extend(&mut self, other: RuleSets) {
        self.ignore_rules.extend(other.ignore_rules);
        self.manage_rules.extend(other.manage_rules);
        self.floating_applications
            .extend(other.floating_applications);
        self.tray_and_multi_window_applications
            .extend(other.tray_and_multi_window_applications);
        self.object_name_change_applications
            .extend(other.object_name_change_applications);
        self.border_overflow_applications
            .extend(other.border_overflow_applications);
        self.layered_whitelist.extend(other.layered_whitelist);
        self.transparency_ignore_rules
            .extend(other.transparency_ignore_rules);
        self.slow_application_identifiers
            .extend(other.slow_application_identifiers);
    }

    /// `true` when no list holds a rule.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ignore_rules.is_empty()
            && self.manage_rules.is_empty()
            && self.floating_applications.is_empty()
            && self.tray_and_multi_window_applications.is_empty()
            && self.object_name_change_applications.is_empty()
            && self.border_overflow_applications.is_empty()
            && self.layered_whitelist.is_empty()
            && self.transparency_ignore_rules.is_empty()
            && self.slow_application_identifiers.is_empty()
    }

    /// The number of rules across every list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ignore_rules.len()
            + self.manage_rules.len()
            + self.floating_applications.len()
            + self.tray_and_multi_window_applications.len()
            + self.object_name_change_applications.len()
            + self.border_overflow_applications.len()
            + self.layered_whitelist.len()
            + self.transparency_ignore_rules.len()
            + self.slow_application_identifiers.len()
    }

    /// `true` when an ignore rule matches.
    #[must_use]
    pub fn should_ignore(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.ignore_rules, window)
    }

    /// `true` when the window should stay opaque instead of being faded.
    #[must_use]
    pub fn should_stay_opaque(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.transparency_ignore_rules, window)
    }

    /// `true` when a manage rule forces the window to be managed.
    #[must_use]
    pub fn should_manage(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.manage_rules, window)
    }

    /// `true` when the window should be managed but left floating.
    #[must_use]
    pub fn should_float(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.floating_applications, window)
    }

    /// `true` when the application keeps windows alive in the tray.
    #[must_use]
    pub fn is_tray_or_multi_window(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.tray_and_multi_window_applications, window)
    }

    /// `true` when the application reuses a window and only changes its title.
    #[must_use]
    pub fn changes_object_name(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.object_name_change_applications, window)
    }

    /// `true` when the window's border overflows its rectangle.
    #[must_use]
    pub fn has_border_overflow(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.border_overflow_applications, window)
    }

    /// `true` when a layered window is whitelisted for management.
    #[must_use]
    pub fn is_layered_whitelisted(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.layered_whitelist, window)
    }

    /// `true` when the window must never be made transparent.
    #[must_use]
    pub fn ignores_transparency(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.transparency_ignore_rules, window)
    }

    /// `true` when the application needs an extra beat before its window is ready.
    #[must_use]
    pub fn is_slow(&self, window: &WindowInfo<'_>) -> bool {
        matches_any(&self.slow_application_identifiers, window)
    }

    /// The verdict for a window. Ignore wins over float, float wins over tile.
    #[must_use]
    pub fn decide(&self, window: &WindowInfo<'_>) -> RuleDecision {
        if self.should_ignore(window) {
            RuleDecision::Ignore
        } else if self.should_float(window) {
            RuleDecision::Float
        } else {
            RuleDecision::Tile
        }
    }

    /// Reports every rule with a regular expression that does not compile.
    #[must_use]
    pub fn validate(&self) -> Vec<Error> {
        self.lists()
            .into_iter()
            .flatten()
            .filter_map(|rule| rule.validate().err())
            .collect()
    }

    fn lists(&self) -> [&Vec<MatchingRule>; 9] {
        [
            &self.ignore_rules,
            &self.manage_rules,
            &self.floating_applications,
            &self.tray_and_multi_window_applications,
            &self.object_name_change_applications,
            &self.border_overflow_applications,
            &self.layered_whitelist,
            &self.transparency_ignore_rules,
            &self.slow_application_identifiers,
        ]
    }
}

// ---------------------------------------------------------------------------
// applications.json
// ---------------------------------------------------------------------------

/// The rule lists one application contributes, in the current
/// `applications.json` shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ApplicationRules {
    /// Goes into [`RuleSets::ignore_rules`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<MatchingRule>,
    /// Goes into [`RuleSets::manage_rules`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub manage: Vec<MatchingRule>,
    /// Goes into [`RuleSets::floating_applications`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub floating: Vec<MatchingRule>,
    /// Goes into [`RuleSets::tray_and_multi_window_applications`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tray_and_multi_window: Vec<MatchingRule>,
    /// Goes into [`RuleSets::object_name_change_applications`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub object_name_change: Vec<MatchingRule>,
    /// Goes into [`RuleSets::border_overflow_applications`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub border_overflow: Vec<MatchingRule>,
    /// Goes into [`RuleSets::layered_whitelist`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layered: Vec<MatchingRule>,
    /// Goes into [`RuleSets::transparency_ignore_rules`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transparency_ignore: Vec<MatchingRule>,
    /// Goes into [`RuleSets::slow_application_identifiers`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slow_application: Vec<MatchingRule>,
}

/// One entry of the older `applications.json`, which was an array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LegacyApplicationEntry {
    /// The application's display name. Not used for matching.
    #[serde(default)]
    pub name: String,
    /// The condition that identifies the application.
    pub identifier: IdWithIdentifier,
    /// Which lists the identifier joins. Empty means `ignore`.
    #[serde(default)]
    pub options: Vec<LegacyApplicationOption>,
    /// Extra conditions that make a window float.
    #[serde(default)]
    pub float_identifiers: Vec<IdWithIdentifier>,
}

/// The option strings the older `applications.json` used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LegacyApplicationOption {
    /// Do not touch the window.
    Ignore,
    /// Manage it even though the heuristics say no.
    ForceManage,
    /// Manage it but keep it floating.
    Float,
    /// The application reuses one window and changes its title.
    ObjectNameChange,
    /// The application keeps a hidden window alive in the tray.
    TrayAndMultiWindow,
    /// The window is layered but should be managed anyway.
    Layered,
    /// The window's border overflows its rectangle.
    BorderOverflow,
    /// The application needs an extra beat before its window is ready.
    SlowApplication,
    /// The window must never be made transparent.
    TransparencyIgnore,
}

/// Either shape of `applications.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AppSpecificConfiguration {
    /// The current shape: a map of application name to its rule lists.
    Map(HashMap<String, ApplicationRules>),
    /// The older shape: an array of entries with an `options` list.
    Legacy(Vec<LegacyApplicationEntry>),
}

impl AppSpecificConfiguration {
    /// Folds every application into one set of rule lists.
    #[must_use]
    pub fn into_rule_sets(self) -> RuleSets {
        let mut sets = RuleSets::new();
        match self {
            Self::Map(apps) => {
                // A map has no order, so sort by name to keep the result stable.
                let mut apps: Vec<_> = apps.into_iter().collect();
                apps.sort_by(|a, b| a.0.cmp(&b.0));
                for (_, rules) in apps {
                    sets.ignore_rules.extend(rules.ignore);
                    sets.manage_rules.extend(rules.manage);
                    sets.floating_applications.extend(rules.floating);
                    sets.tray_and_multi_window_applications
                        .extend(rules.tray_and_multi_window);
                    sets.object_name_change_applications
                        .extend(rules.object_name_change);
                    sets.border_overflow_applications
                        .extend(rules.border_overflow);
                    sets.layered_whitelist.extend(rules.layered);
                    sets.transparency_ignore_rules
                        .extend(rules.transparency_ignore);
                    sets.slow_application_identifiers
                        .extend(rules.slow_application);
                }
            }
            Self::Legacy(entries) => {
                for entry in entries {
                    let rule = MatchingRule::Simple(entry.identifier);
                    if entry.options.is_empty() {
                        sets.ignore_rules.push(rule.clone());
                    }
                    for option in &entry.options {
                        let list = match option {
                            LegacyApplicationOption::Ignore => &mut sets.ignore_rules,
                            LegacyApplicationOption::ForceManage => &mut sets.manage_rules,
                            LegacyApplicationOption::Float => &mut sets.floating_applications,
                            LegacyApplicationOption::ObjectNameChange => {
                                &mut sets.object_name_change_applications
                            }
                            LegacyApplicationOption::TrayAndMultiWindow => {
                                &mut sets.tray_and_multi_window_applications
                            }
                            LegacyApplicationOption::Layered => &mut sets.layered_whitelist,
                            LegacyApplicationOption::BorderOverflow => {
                                &mut sets.border_overflow_applications
                            }
                            LegacyApplicationOption::SlowApplication => {
                                &mut sets.slow_application_identifiers
                            }
                            LegacyApplicationOption::TransparencyIgnore => {
                                &mut sets.transparency_ignore_rules
                            }
                        };
                        list.push(rule.clone());
                    }
                    sets.floating_applications.extend(
                        entry
                            .float_identifiers
                            .into_iter()
                            .map(MatchingRule::Simple),
                    );
                }
            }
        }
        sets
    }
}

/// Parses an `applications.json` in either shape into rule lists.
///
/// The `$schema` key and any other unknown top level key is skipped.
///
/// # Errors
///
/// Returns [`Error::Json`] when the document is not valid JSON or does not
/// match either shape.
pub fn load_app_specific_configuration(json: &str) -> Result<RuleSets> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let value = match value {
        serde_json::Value::Object(mut map) => {
            map.retain(|key, value| !key.starts_with('$') && value.is_object());
            serde_json::Value::Object(map)
        }
        other => other,
    };
    let parsed: AppSpecificConfiguration = serde_json::from_value(value)?;
    Ok(parsed.into_rule_sets())
}

// ---------------------------------------------------------------------------
// regex cache
// ---------------------------------------------------------------------------

fn regex_cache() -> &'static RwLock<HashMap<String, Option<Arc<regex::Regex>>>> {
    static CACHE: std::sync::OnceLock<RwLock<HashMap<String, Option<Arc<regex::Regex>>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Compiles a pattern, reusing the process-wide cache.
///
/// Returns `None` for a pattern that does not compile. A poisoned cache lock
/// falls back to compiling on the spot rather than panicking.
fn compile(pattern: &str) -> Option<Arc<regex::Regex>> {
    if let Ok(cache) = regex_cache().read()
        && let Some(hit) = cache.get(pattern)
    {
        return hit.clone();
    }
    let compiled = regex::Regex::new(pattern).ok().map(Arc::new);
    if let Ok(mut cache) = regex_cache().write() {
        cache.insert(pattern.to_string(), compiled.clone());
    }
    compiled
}

fn regex_matches(pattern: &str, value: &str) -> bool {
    compile(pattern).is_some_and(|re| re.is_match(value))
}

/// `true` when the string carries regular expression syntax.
fn looks_like_regex(id: &str) -> bool {
    id.contains([
        '.', '*', '+', '?', '[', ']', '(', ')', '{', '}', '^', '$', '|', '\\',
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> WindowInfo<'static> {
        WindowInfo::new(
            "README.md - Mochi - Visual Studio Code",
            "Chrome_WidgetWin_1",
            "Code.exe",
            r"C:\Users\domin\AppData\Local\Programs\Microsoft VS Code\Code.exe",
        )
    }

    #[test]
    fn every_strategy_behaves() {
        let w = code();
        let rule =
            |strategy, id: &str| IdWithIdentifier::new(ApplicationIdentifier::Exe, id, strategy);

        assert!(rule(MatchingStrategy::Equals, "Code.exe").matches(&w));
        assert!(!rule(MatchingStrategy::Equals, "code.exe").matches(&w));
        assert!(rule(MatchingStrategy::DoesNotEqual, "code.exe").matches(&w));
        assert!(rule(MatchingStrategy::StartsWith, "Code").matches(&w));
        assert!(rule(MatchingStrategy::DoesNotStartWith, "Vs").matches(&w));
        assert!(rule(MatchingStrategy::EndsWith, ".exe").matches(&w));
        assert!(rule(MatchingStrategy::DoesNotEndWith, ".dll").matches(&w));
        assert!(rule(MatchingStrategy::Contains, "ode").matches(&w));
        assert!(rule(MatchingStrategy::DoesNotContain, "firefox").matches(&w));
        assert!(rule(MatchingStrategy::Regex, "^Code\\.exe$").matches(&w));
        assert!(!rule(MatchingStrategy::Regex, "^firefox").matches(&w));
    }

    #[test]
    fn legacy_is_equals_for_a_plain_identifier() {
        let w = WindowInfo::new("", "AbletonVstPlugClass", "", "");
        let plain = IdWithIdentifier::new(
            ApplicationIdentifier::Class,
            "AbletonVstPlugClass",
            MatchingStrategy::Legacy,
        );
        assert!(plain.matches(&w));

        let partial = IdWithIdentifier::new(
            ApplicationIdentifier::Class,
            "Ableton",
            MatchingStrategy::Legacy,
        );
        assert!(
            !partial.matches(&w),
            "a plain legacy id is not a substring match"
        );
    }

    #[test]
    fn legacy_falls_back_to_regex_when_the_id_looks_like_one() {
        let w = WindowInfo::new("Picture in Picture", "", "", "");
        let re = IdWithIdentifier::new(
            ApplicationIdentifier::Title,
            "[Pp]icture.in.[Pp]icture",
            MatchingStrategy::Legacy,
        );
        assert!(re.matches(&w));
    }

    #[test]
    fn a_missing_strategy_is_legacy() {
        let rule: IdWithIdentifier =
            serde_json::from_str(r#"{"kind":"Exe","id":"a.exe"}"#).unwrap();
        assert_eq!(rule.strategy(), MatchingStrategy::Legacy);
        assert!(rule.matches(&WindowInfo::new("", "", "a.exe", "")));
    }

    #[test]
    fn a_broken_regex_never_matches_and_is_reported() {
        let broken = IdWithIdentifier::new(
            ApplicationIdentifier::Title,
            "([unclosed",
            MatchingStrategy::Regex,
        );
        assert!(!broken.matches(&code()));
        assert!(matches!(broken.validate(), Err(Error::InvalidRegex { .. })));
        let good = IdWithIdentifier::new(
            ApplicationIdentifier::Title,
            "^ok$",
            MatchingStrategy::Regex,
        );
        assert!(good.validate().is_ok());
    }

    #[test]
    fn composite_rules_need_every_condition() {
        let ableton = MatchingRule::Composite(vec![
            IdWithIdentifier::new(
                ApplicationIdentifier::Class,
                "Ableton Live Window Class",
                MatchingStrategy::Equals,
            ),
            IdWithIdentifier::new(
                ApplicationIdentifier::Title,
                "Ableton",
                MatchingStrategy::DoesNotContain,
            ),
        ]);
        assert!(ableton.matches(&WindowInfo::new(
            "Plugin",
            "Ableton Live Window Class",
            "",
            ""
        )));
        assert!(!ableton.matches(&WindowInfo::new(
            "Ableton Live 12",
            "Ableton Live Window Class",
            "",
            ""
        )));
        assert!(!ableton.matches(&WindowInfo::new("Plugin", "Other", "", "")));
        assert_eq!(ableton.conditions().len(), 2);
        assert!(!MatchingRule::Composite(vec![]).matches(&code()));
    }

    #[test]
    fn composite_rules_deserialise_from_a_nested_array() {
        let json = r#"[
            {"kind":"Class","id":"A","matching_strategy":"Equals"},
            {"kind":"Title","id":"B","matching_strategy":"DoesNotContain"}
        ]"#;
        let rule: MatchingRule = serde_json::from_str(json).unwrap();
        assert!(matches!(rule, MatchingRule::Composite(ref v) if v.len() == 2));
    }

    #[test]
    fn the_users_ignore_rules_deserialise_verbatim() {
        let json = r#"[
            { "kind": "Path", "id": "steamapps\\common", "matching_strategy": "Contains" },
            { "kind": "Exe", "id": "wallpaper64.exe", "matching_strategy": "Equals" },
            { "kind": "Title", "id": "[Pp]icture.in.[Pp]icture", "matching_strategy": "Regex" },
            { "kind": "Title", "id": " - Peek", "matching_strategy": "EndsWith" }
        ]"#;
        let rules: Vec<MatchingRule> = serde_json::from_str(json).unwrap();
        assert_eq!(rules.len(), 4);

        let steam = WindowInfo::new("Game", "", "game.exe", r"D:\steamapps\common\Game\game.exe");
        assert!(matches_any(&rules, &steam));

        let pip = WindowInfo::new("Picture-in-Picture", "", "firefox.exe", "");
        assert!(matches_any(&rules, &pip));

        let peek = WindowInfo::new("image.png - Peek", "", "Peek.exe", "");
        assert!(matches_any(&rules, &peek));

        assert!(!matches_any(&rules, &code()));
        assert!(rules.iter().all(|r| r.validate().is_ok()));
    }

    #[test]
    fn rule_sets_decide_in_priority_order() {
        let mut sets = RuleSets::new();
        sets.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "zebar.exe",
            MatchingStrategy::Equals,
        ));
        sets.floating_applications.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "Calculator.exe",
            MatchingStrategy::Equals,
        ));

        let zebar = WindowInfo::new("", "", "zebar.exe", "");
        let calc = WindowInfo::new("", "", "Calculator.exe", "");
        assert_eq!(sets.decide(&zebar), RuleDecision::Ignore);
        assert_eq!(sets.decide(&calc), RuleDecision::Float);
        assert_eq!(sets.decide(&code()), RuleDecision::Tile);
        assert!(sets.should_ignore(&zebar));
        assert!(sets.should_float(&calc));
        assert!(!sets.is_empty());
        assert_eq!(sets.len(), 2);
    }

    #[test]
    fn ignore_wins_over_float() {
        let mut sets = RuleSets::new();
        let rule = MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "a.exe",
            MatchingStrategy::Equals,
        );
        sets.ignore_rules.push(rule.clone());
        sets.floating_applications.push(rule);
        assert_eq!(
            sets.decide(&WindowInfo::new("", "", "a.exe", "")),
            RuleDecision::Ignore
        );
    }

    #[test]
    fn every_predicate_reads_its_own_list() {
        let rule = MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "a.exe",
            MatchingStrategy::Equals,
        );
        let w = WindowInfo::new("", "", "a.exe", "");
        let mut sets = RuleSets::new();
        sets.manage_rules.push(rule.clone());
        sets.tray_and_multi_window_applications.push(rule.clone());
        sets.object_name_change_applications.push(rule.clone());
        sets.border_overflow_applications.push(rule.clone());
        sets.layered_whitelist.push(rule.clone());
        sets.transparency_ignore_rules.push(rule.clone());
        sets.slow_application_identifiers.push(rule);
        assert!(sets.should_manage(&w));
        assert!(sets.is_tray_or_multi_window(&w));
        assert!(sets.changes_object_name(&w));
        assert!(sets.has_border_overflow(&w));
        assert!(sets.is_layered_whitelisted(&w));
        assert!(sets.ignores_transparency(&w));
        assert!(sets.is_slow(&w));
        assert_eq!(sets.len(), 7);
    }

    #[test]
    fn rule_sets_extend_and_validate() {
        let mut a = RuleSets::new();
        a.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "a.exe",
            MatchingStrategy::Equals,
        ));
        let mut b = RuleSets::new();
        b.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Title,
            "([",
            MatchingStrategy::Regex,
        ));
        a.extend(b);
        assert_eq!(a.ignore_rules.len(), 2);
        assert_eq!(a.validate().len(), 1);
        assert!(RuleSets::new().is_empty());
    }

    #[test]
    fn loads_the_current_applications_json_shape() {
        let json = r#"{
            "$schema": "https://example.invalid/schema.json",
            "1Password": {
                "ignore": [{ "kind": "Exe", "id": "1Password.exe", "matching_strategy": "Equals" }]
            },
            "Ableton Live": {
                "ignore": [
                    { "kind": "Class", "id": "Vst3PlugWindow", "matching_strategy": "Legacy" },
                    [
                        { "kind": "Class", "id": "Ableton Live Window Class", "matching_strategy": "Equals" },
                        { "kind": "Title", "id": "Ableton", "matching_strategy": "DoesNotContain" }
                    ]
                ]
            },
            "Android Studio": {
                "object_name_change": [{ "kind": "Exe", "id": "studio64.exe", "matching_strategy": "Equals" }]
            },
            "Affinity Designer 2": {
                "ignore": [{ "kind": "Exe", "id": "Designer.exe", "matching_strategy": "Equals" }],
                "manage": [{ "kind": "Title", "id": "Affinity Designer 2", "matching_strategy": "Equals" }]
            },
            "Discord": {
                "tray_and_multi_window": [{ "kind": "Exe", "id": "Discord.exe", "matching_strategy": "Equals" }],
                "layered": [{ "kind": "Exe", "id": "Discord.exe", "matching_strategy": "Equals" }],
                "slow_application": [{ "kind": "Exe", "id": "Discord.exe", "matching_strategy": "Equals" }],
                "transparency_ignore": [{ "kind": "Exe", "id": "Discord.exe", "matching_strategy": "Equals" }],
                "floating": [{ "kind": "Title", "id": "Discord Updater", "matching_strategy": "Equals" }],
                "border_overflow": [{ "kind": "Exe", "id": "Discord.exe", "matching_strategy": "Equals" }]
            }
        }"#;
        let sets = load_app_specific_configuration(json).unwrap();
        assert_eq!(sets.ignore_rules.len(), 4);
        assert_eq!(sets.manage_rules.len(), 1);
        assert_eq!(sets.object_name_change_applications.len(), 1);
        assert_eq!(sets.tray_and_multi_window_applications.len(), 1);
        assert_eq!(sets.layered_whitelist.len(), 1);
        assert_eq!(sets.slow_application_identifiers.len(), 1);
        assert_eq!(sets.transparency_ignore_rules.len(), 1);
        assert_eq!(sets.floating_applications.len(), 1);
        assert_eq!(sets.border_overflow_applications.len(), 1);

        assert!(sets.should_ignore(&WindowInfo::new("", "", "1Password.exe", "")));
        assert!(sets.should_ignore(&WindowInfo::new(
            "Plugin",
            "Ableton Live Window Class",
            "",
            ""
        )));
        assert!(!sets.should_ignore(&WindowInfo::new(
            "Ableton Live 12",
            "Ableton Live Window Class",
            "",
            ""
        )));
        assert!(sets.changes_object_name(&WindowInfo::new("", "", "studio64.exe", "")));
    }

    #[test]
    fn loads_the_legacy_applications_json_shape() {
        let json = r#"[
            {
                "name": "Firefox",
                "identifier": { "kind": "Exe", "id": "firefox.exe", "matching_strategy": "Equals" },
                "options": ["object_name_change", "tray_and_multi_window"]
            },
            {
                "name": "Old Ignored App",
                "identifier": { "kind": "Exe", "id": "old.exe", "matching_strategy": "Equals" }
            },
            {
                "name": "Forced",
                "identifier": { "kind": "Exe", "id": "forced.exe", "matching_strategy": "Equals" },
                "options": ["force_manage", "layered", "border_overflow", "slow_application", "transparency_ignore", "float", "ignore"],
                "float_identifiers": [{ "kind": "Title", "id": "Settings", "matching_strategy": "Equals" }]
            }
        ]"#;
        let sets = load_app_specific_configuration(json).unwrap();
        assert_eq!(sets.object_name_change_applications.len(), 1);
        assert_eq!(sets.tray_and_multi_window_applications.len(), 1);
        assert_eq!(sets.manage_rules.len(), 1);
        assert_eq!(sets.layered_whitelist.len(), 1);
        assert_eq!(sets.border_overflow_applications.len(), 1);
        assert_eq!(sets.slow_application_identifiers.len(), 1);
        assert_eq!(sets.transparency_ignore_rules.len(), 1);
        assert_eq!(sets.floating_applications.len(), 2);
        assert_eq!(
            sets.ignore_rules.len(),
            2,
            "one entry without options plus one explicit ignore"
        );
    }

    #[test]
    fn a_broken_applications_json_is_an_error() {
        assert!(load_app_specific_configuration("not json").is_err());
        assert!(load_app_specific_configuration(r#"{"App": 5}"#).is_ok());
    }

    #[test]
    fn rule_sets_round_trip_through_json() {
        let json = r#"{
            "ignore_rules": [{ "kind": "Exe", "id": "a.exe", "matching_strategy": "Equals" }],
            "floating_applications": [
                [
                    { "kind": "Exe", "id": "b.exe", "matching_strategy": "Equals" },
                    { "kind": "Title", "id": "Options", "matching_strategy": "StartsWith" }
                ]
            ]
        }"#;
        let sets: RuleSets = serde_json::from_str(json).unwrap();
        assert_eq!(sets.ignore_rules.len(), 1);
        assert_eq!(sets.floating_applications.len(), 1);

        let round = serde_json::to_string(&sets).unwrap();
        assert_eq!(serde_json::from_str::<RuleSets>(&round).unwrap(), sets);
    }

    #[test]
    fn unknown_option_strings_are_a_hard_error_not_a_silent_drop() {
        let json = r#"[{
            "name": "X",
            "identifier": { "kind": "Exe", "id": "x.exe" },
            "options": ["teleport"]
        }]"#;
        assert!(load_app_specific_configuration(json).is_err());
    }
}
