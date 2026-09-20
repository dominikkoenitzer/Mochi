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

use std::borrow::Cow;
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
    /// The identifier has to describe the whole value: it matches when it is
    /// equal to the value, or when it is a regular expression that matches the
    /// value from end to end. An identifier that carries regex punctuation but
    /// was meant literally, like `EVERYTHING_(1.5a)`, therefore still matches.
    #[default]
    Legacy,
    /// Exact. Byte exact for `Class` and `Title`, and case insensitive for
    /// `Exe` and `Path`, which Windows itself compares case insensitively.
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
    ///
    /// An empty `id` never matches. It would otherwise be a wildcard for
    /// `StartsWith`, `EndsWith`, `Contains` and `Regex`, so a single empty
    /// entry in `ignore_rules` would switch the whole window manager off.
    ///
    /// `Exe` and `Path` are compared without regard to ASCII case, the way
    /// Windows compares file names, so `discord.exe` in a hand written rule
    /// finds the real `Discord.exe`. `Class` and `Title` stay byte exact.
    #[must_use]
    pub fn matches(&self, window: &WindowInfo<'_>) -> bool {
        if self.id.is_empty() {
            return false;
        }
        let raw = window.value_for(self.kind);
        let fold = folds_case(self.kind);
        let value = fold_case(raw, fold);
        let id = fold_case(&self.id, fold);
        let (value, id) = (value.as_ref(), id.as_ref());
        match self.strategy() {
            MatchingStrategy::Equals => value == id,
            MatchingStrategy::DoesNotEqual => value != id,
            MatchingStrategy::StartsWith => value.starts_with(id),
            MatchingStrategy::DoesNotStartWith => !value.starts_with(id),
            MatchingStrategy::EndsWith => value.ends_with(id),
            MatchingStrategy::DoesNotEndWith => !value.ends_with(id),
            MatchingStrategy::Contains => value.contains(id),
            MatchingStrategy::DoesNotContain => !value.contains(id),
            MatchingStrategy::Regex => regex_matches(&self.pattern(), raw),
            // Exact first, because that is what a legacy id nearly always
            // means, and only then the anchored regular expression. A legacy
            // id that carries punctuation but was meant literally still
            // matches, which the other honest option, anchoring alone, would
            // not do: `EVERYTHING_(1.5a)` anchored is still a capture group
            // and a `.` that eats any character.
            MatchingStrategy::Legacy => {
                value == id || (looks_like_regex(&self.id) && regex_matches(&self.pattern(), raw))
            }
        }
    }

    /// The regular expression this condition compiles, if it uses one.
    ///
    /// `Legacy` is anchored so an id describes the whole value rather than a
    /// part of it, and both forms fold case for the identifiers Windows itself
    /// treats case insensitively.
    fn pattern(&self) -> String {
        let flag = if folds_case(self.kind) { "(?i)" } else { "" };
        match self.strategy() {
            MatchingStrategy::Legacy => format!("{flag}^(?:{})$", self.id),
            _ => format!("{flag}{}", self.id),
        }
    }

    /// Reports a rule that cannot do what it says, so a config reload can drop
    /// it and warn about it.
    ///
    /// A `Legacy` id that does not compile is not an error: it falls back to an
    /// exact comparison, which is what such an id nearly always meant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyRuleId`] for an empty `id` and
    /// [`Error::InvalidRegex`] when the strategy is [`MatchingStrategy::Regex`]
    /// and `id` does not compile.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(Error::EmptyRuleId { kind: self.kind });
        }
        if self.strategy() == MatchingStrategy::Regex {
            let pattern = self.pattern();
            if compile(&pattern).is_none() {
                return Err(Error::InvalidRegex {
                    pattern: self.id.clone(),
                    message: regex::Regex::new(&pattern)
                        .err()
                        .map_or_else(|| "unknown".to_string(), |e| e.to_string()),
                });
            }
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
    /// The subset of [`RuleSets::ignore_rules`] the user wrote themselves, in
    /// their own configuration file, as opposed to a community rule file they
    /// merely pointed at.
    ///
    /// These cannot be overridden by a manage rule. A community file is
    /// thousands of lines written for everybody, and it uses a broad ignore
    /// with a narrow manage to rescue an application's main window; that idiom
    /// is why a manage rule beats an ignore rule at all. But the same mechanism
    /// would let a stranger's manage rule cancel the one line the user wrote to
    /// keep their games off the tiling grid, and being overruled on your own
    /// machine by a file you did not write is not a trade anybody would accept.
    /// Your own configuration is the last word.
    pub own_ignore_rules: Vec<MatchingRule>,
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
        // Deliberately not extended: what arrives through here is a community
        // rule file, and only the user's own file speaks with that authority.
        // See [`RuleSets::own_ignore_rules`].
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

    /// The verdict for a window.
    ///
    /// The ladder, from the top:
    ///
    /// 1. An `ignore_rules` entry from the user's own configuration file is
    ///    final. Nothing overrides it, see [`RuleSets::own_ignore_rules`].
    /// 2. A `manage_rules` entry wins over every other `ignore_rules` entry. Rule
    ///    files are written in the "broad ignore, narrow manage" idiom: an
    ///    application drops a whole class or executable and then names the one
    ///    window it wants back, and both rules match that window. A manage rule
    ///    is the only rule a user writes to say "this window, specifically,
    ///    yes", so it has to be able to say it. Keep manage rules narrow: a
    ///    broad one now defeats an ignore rule that was meant to hold.
    /// 3. Otherwise an `ignore_rules` entry means the window is left alone.
    /// 4. A window that is managed floats when a `floating_applications` entry
    ///    matches. Floating is a kind of managing, so a manage rule does not
    ///    override it.
    /// 5. Everything else is tiled.
    #[must_use]
    pub fn decide(&self, window: &WindowInfo<'_>) -> RuleDecision {
        // The user's own ignore is final; every other ignore yields to a
        // manage rule naming the same window.
        let ignored = matches_any(&self.own_ignore_rules, window)
            || (self.should_ignore(window) && !self.should_manage(window));
        if ignored {
            RuleDecision::Ignore
        } else if self.should_float(window) {
            RuleDecision::Float
        } else {
            RuleDecision::Tile
        }
    }

    /// Reports every rule that cannot do what it says: a broken regular
    /// expression, or an empty identifier.
    ///
    /// The rules stay in place. Use [`RuleSets::drop_invalid`] to remove them.
    #[must_use]
    pub fn validate(&self) -> Vec<Error> {
        self.lists()
            .into_iter()
            .flatten()
            .filter_map(|rule| rule.validate().err())
            .collect()
    }

    /// Removes every rule [`RuleSets::validate`] complains about and returns
    /// the complaints.
    ///
    /// A rule with a broken regular expression can never match, and one with an
    /// empty identifier would match everything, so keeping either only means
    /// carrying a rule that lies about what it does. Dropping them is what the
    /// daemon already tells the user it does.
    pub fn drop_invalid(&mut self) -> Vec<Error> {
        let mut errors = Vec::new();
        for list in self.lists_mut() {
            list.retain(|rule| match rule.validate() {
                Ok(()) => true,
                Err(error) => {
                    errors.push(error);
                    false
                }
            });
        }
        errors
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

    fn lists_mut(&mut self) -> [&mut Vec<MatchingRule>; 9] {
        [
            &mut self.ignore_rules,
            &mut self.manage_rules,
            &mut self.floating_applications,
            &mut self.tray_and_multi_window_applications,
            &mut self.object_name_change_applications,
            &mut self.border_overflow_applications,
            &mut self.layered_whitelist,
            &mut self.transparency_ignore_rules,
            &mut self.slow_application_identifiers,
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

/// The keys one application may use in the current `applications.json`.
///
/// The same lists have longer names in `mochi.json`, and writing a config name
/// here is the mistake [`load_app_specific_configuration_reporting_unknown_keys`]
/// exists to catch. Every config spelling starts with its app file spelling,
/// which is what the suggestion below is built on.
const APPLICATION_RULE_KEYS: [&str; 9] = [
    "ignore",
    "manage",
    "floating",
    "tray_and_multi_window",
    "object_name_change",
    "border_overflow",
    "layered",
    "transparency_ignore",
    "slow_application",
];

/// Parses an `applications.json` in either shape into rule lists.
///
/// The `$schema` key and any other unknown top level key is skipped. Use
/// [`load_app_specific_configuration_reporting_unknown_keys`] to hear about
/// what was skipped.
///
/// # Errors
///
/// Returns [`Error::Json`] when the document is not valid JSON or does not
/// match either shape.
pub fn load_app_specific_configuration(json: &str) -> Result<RuleSets> {
    load_app_specific_configuration_reporting_unknown_keys(json).map(|(sets, _)| sets)
}

/// [`load_app_specific_configuration`] plus a line about every key that was
/// skipped instead of being turned into rules.
///
/// A mistyped list name, `floating_applications` where the app file wants
/// `floating`, parses into no rules at all because every list defaults to
/// empty. That is not worth refusing the whole file over, but the user has to
/// hear about it, so it comes back as a message rather than an error.
///
/// # Errors
///
/// Returns [`Error::Json`] when the document is not valid JSON or does not
/// match either shape.
pub fn load_app_specific_configuration_reporting_unknown_keys(
    json: &str,
) -> Result<(RuleSets, Vec<String>)> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let mut warnings = Vec::new();
    let value = match value {
        serde_json::Value::Object(mut map) => {
            for (app, rules) in &map {
                if app.starts_with('$') {
                    continue;
                }
                let Some(rules) = rules.as_object() else {
                    warnings.push(format!(
                        "{app:?} is not a set of rule lists, it was skipped"
                    ));
                    continue;
                };
                warnings.extend(
                    rules
                        .keys()
                        .filter(|key| !APPLICATION_RULE_KEYS.contains(&key.as_str()))
                        .map(|key| unknown_key_warning(app, key)),
                );
            }
            map.retain(|key, value| !key.starts_with('$') && value.is_object());
            serde_json::Value::Object(map)
        }
        other => other,
    };
    let parsed: AppSpecificConfiguration = serde_json::from_value(value)?;
    Ok((parsed.into_rule_sets(), warnings))
}

fn unknown_key_warning(app: &str, key: &str) -> String {
    APPLICATION_RULE_KEYS
        .iter()
        .find(|known| key.starts_with(*known))
        .map_or_else(
            || format!("{app:?} has no list called {key:?}, the rules in it were skipped"),
            |known| {
                format!(
                    "{app:?} has no list called {key:?}, did you mean {known:?}? \
                     The rules in it were skipped"
                )
            },
        )
}

// ---------------------------------------------------------------------------
// regex cache
// ---------------------------------------------------------------------------

fn regex_cache() -> &'static RwLock<HashMap<String, Arc<regex::Regex>>> {
    static CACHE: std::sync::OnceLock<RwLock<HashMap<String, Arc<regex::Regex>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Compiles a pattern, reusing the process-wide cache.
///
/// Returns `None` for a pattern that does not compile. Only patterns that do
/// compile are cached: memoising a failure would keep a broken pattern in the
/// cache for the life of the process, so the same failure could never be
/// reported again after a config reload. A poisoned cache lock falls back to
/// compiling on the spot rather than panicking.
fn compile(pattern: &str) -> Option<Arc<regex::Regex>> {
    if let Ok(cache) = regex_cache().read()
        && let Some(hit) = cache.get(pattern)
    {
        return Some(Arc::clone(hit));
    }
    let compiled = Arc::new(regex::Regex::new(pattern).ok()?);
    if let Ok(mut cache) = regex_cache().write() {
        cache.insert(pattern.to_string(), Arc::clone(&compiled));
    }
    Some(compiled)
}

fn regex_matches(pattern: &str, value: &str) -> bool {
    compile(pattern).is_some_and(|re| re.is_match(value))
}

/// `true` for the identifiers Windows itself compares case insensitively.
///
/// File names and paths are case insensitive on Windows, so a rule that writes
/// `discord.exe` has to find `Discord.exe`. A window class and a window title
/// are content rather than file names: a class is reported verbatim by the
/// platform layer and community rule files spell it exactly, and a title is
/// text the user matches deliberately, so both stay byte exact.
const fn folds_case(kind: ApplicationIdentifier) -> bool {
    matches!(
        kind,
        ApplicationIdentifier::Exe | ApplicationIdentifier::Path
    )
}

/// Lower cases `value` when the comparison folds case, and borrows it otherwise.
fn fold_case(value: &str, fold: bool) -> Cow<'_, str> {
    if fold {
        Cow::Owned(value.to_ascii_lowercase())
    } else {
        Cow::Borrowed(value)
    }
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
        // Changed behaviour: an Exe is a file name, so its case does not count.
        assert!(rule(MatchingStrategy::Equals, "code.exe").matches(&w));
        assert!(rule(MatchingStrategy::DoesNotEqual, "notepad.exe").matches(&w));
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

    // -----------------------------------------------------------------
    // regressions
    // -----------------------------------------------------------------

    /// The six entries of the user's `applications.json` that pair a broad
    /// `ignore` with a narrow `manage`, copied verbatim.
    const RESCUED: &str = r#"{
        "Affinity Designer 2": {
            "ignore": [{ "kind": "Exe", "id": "Designer.exe", "matching_strategy": "Equals" }],
            "manage": [{ "kind": "Title", "id": "Affinity Designer 2", "matching_strategy": "Equals" }]
        },
        "Affinity Photo 2": {
            "ignore": [{ "kind": "Exe", "id": "Photo.exe", "matching_strategy": "Equals" }],
            "manage": [{ "kind": "Title", "id": "Affinity Photo 2", "matching_strategy": "Equals" }]
        },
        "Affinity Publisher 2": {
            "ignore": [{ "kind": "Exe", "id": "Publisher.exe", "matching_strategy": "Equals" }],
            "manage": [{ "kind": "Title", "id": "Affinity Publisher 2", "matching_strategy": "Equals" }]
        },
        "GOG Galaxy": {
            "ignore": [{ "kind": "Class", "id": "Chrome_RenderWidgetHostHWND", "matching_strategy": "Legacy" }],
            "manage": [{ "kind": "Exe", "id": "GalaxyClient.exe", "matching_strategy": "Equals" }],
            "tray_and_multi_window": [{ "kind": "Exe", "id": "GalaxyClient.exe", "matching_strategy": "Equals" }]
        },
        "visio": {
            "ignore": [
                { "kind": "Class", "id": "VISIOS", "matching_strategy": "Equals" },
                { "kind": "Class", "id": "VISIOQ", "matching_strategy": "Equals" }
            ],
            "manage": [{ "kind": "Class", "id": "VISIOA", "matching_strategy": "Equals" }]
        },
        "WeChat": {
            "ignore": [
                { "kind": "Class", "id": "WeChatLoginWndForPC", "matching_strategy": "Equals" },
                { "kind": "Class", "id": "ChatWnd", "matching_strategy": "Equals" },
                { "kind": "Exe", "id": "WeChatAppEx.exe", "matching_strategy": "Equals" }
            ],
            "manage": [{ "kind": "Class", "id": "WeChatMainWndForPC", "matching_strategy": "Equals" }],
            "tray_and_multi_window": [{ "kind": "Class", "id": "WeChatMainWndForPC", "matching_strategy": "Equals" }]
        }
    }"#;

    /// One representative main window per rescued application.
    fn rescued_main_windows() -> Vec<(&'static str, WindowInfo<'static>)> {
        vec![
            (
                "Affinity Designer 2",
                WindowInfo::new(
                    "Affinity Designer 2",
                    "Qt5152QWindowIcon",
                    "Designer.exe",
                    r"C:\Program Files\Affinity\Designer 2\Designer.exe",
                ),
            ),
            (
                "Affinity Photo 2",
                WindowInfo::new(
                    "Affinity Photo 2",
                    "Qt5152QWindowIcon",
                    "Photo.exe",
                    r"C:\Program Files\Affinity\Photo 2\Photo.exe",
                ),
            ),
            (
                "Affinity Publisher 2",
                WindowInfo::new(
                    "Affinity Publisher 2",
                    "Qt5152QWindowIcon",
                    "Publisher.exe",
                    r"C:\Program Files\Affinity\Publisher 2\Publisher.exe",
                ),
            ),
            (
                "GOG Galaxy",
                WindowInfo::new(
                    "GOG GALAXY",
                    "Chrome_RenderWidgetHostHWND",
                    "GalaxyClient.exe",
                    r"C:\Program Files (x86)\GOG Galaxy\GalaxyClient.exe",
                ),
            ),
            (
                "visio",
                WindowInfo::new(
                    "Drawing1 - Visio",
                    "VISIOA",
                    "VISIO.EXE",
                    r"C:\Program Files\Microsoft Office\root\Office16\VISIO.EXE",
                ),
            ),
            (
                "WeChat",
                WindowInfo::new(
                    "WeChat",
                    "WeChatMainWndForPC",
                    "WeChat.exe",
                    r"C:\Program Files\Tencent\WeChat\WeChat.exe",
                ),
            ),
        ]
    }

    #[test]
    fn an_ignore_rule_the_user_wrote_themselves_cannot_be_overruled() {
        // A community rule file is written for everybody and uses a broad
        // ignore with a narrow manage to rescue an application's main window.
        // That is why a manage rule beats an ignore rule at all. It must not
        // also let a stranger's file cancel the line the user wrote to keep a
        // game off the tiling grid.
        let game = WindowInfo {
            exe: "StarRail.exe",
            class: "UnityWndClass",
            title: "Honkai: Star Rail",
            path: r"C:\Games\StarRail\StarRail.exe",
        };

        let community = MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "StarRail.exe".to_owned(),
            MatchingStrategy::Equals,
        );

        // Only the community file says anything: its manage rule rescues the
        // window from its own ignore rule, which is the idiom working.
        let mut rules = RuleSets::new();
        rules.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Class,
            "UnityWndClass".to_owned(),
            MatchingStrategy::Equals,
        ));
        rules.manage_rules.push(community.clone());
        assert_eq!(rules.decide(&game), RuleDecision::Tile);

        // Now the user has written the same ignore in their own file. Their
        // line is the last word, whatever the community file wants.
        rules.own_ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Path,
            r"C:\Games".to_owned(),
            MatchingStrategy::StartsWith,
        ));
        assert_eq!(
            rules.decide(&game),
            RuleDecision::Ignore,
            "a rule file the user merely pointed at overruled the one they wrote themselves"
        );
    }

    #[test]
    fn a_narrow_manage_rule_rescues_a_window_from_a_broad_ignore_rule() {
        let sets = load_app_specific_configuration(RESCUED).unwrap();
        for (name, window) in rescued_main_windows() {
            assert!(
                sets.should_manage(&window),
                "{name}: the manage rule should name this window"
            );
            assert_ne!(
                sets.decide(&window),
                RuleDecision::Ignore,
                "{name}: a manage rule has to beat the broad ignore rule"
            );
        }
    }

    #[test]
    fn the_users_real_applications_json_no_longer_drops_the_rescued_main_windows() {
        let path = std::env::var_os("USERPROFILE")
            .map(|home| std::path::Path::new(&home).join("applications.json"));
        let Some(text) = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
        else {
            eprintln!("no applications.json next to the user profile, nothing to check");
            return;
        };
        let (sets, warnings) =
            load_app_specific_configuration_reporting_unknown_keys(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        for (name, window) in rescued_main_windows() {
            assert!(
                sets.should_manage(&window),
                "{name}: no manage rule names this window"
            );
            assert_ne!(
                sets.decide(&window),
                RuleDecision::Ignore,
                "{name}: still dropped by the merged rule lists"
            );
        }

        // The same file, the same two rules, the other two defects: a legacy id
        // with literal parentheses, and an exe spelled `StarRail.Exe`.
        let everything = WindowInfo::new("Everything", "EVERYTHING_(1.5a)", "Everything.exe", "");
        assert!(sets.should_manage(&everything));
        assert!(sets.is_tray_or_multi_window(&everything));
        assert!(sets.should_manage(&WindowInfo::new(
            "Honkai: Star Rail",
            "UnityWndClass",
            "StarRail.exe",
            ""
        )));
    }

    #[test]
    fn a_legacy_id_with_literal_parentheses_matches_the_real_window_class() {
        let everything = IdWithIdentifier::new(
            ApplicationIdentifier::Class,
            "EVERYTHING_(1.5a)",
            MatchingStrategy::Legacy,
        );
        assert!(
            everything.matches(&WindowInfo::new("Everything", "EVERYTHING_(1.5a)", "", "")),
            "the real class, with the parentheses in it"
        );
        assert!(
            !everything.matches(&WindowInfo::new("Everything", "EVERYTHING_", "", "")),
            "and not every class that starts like it"
        );

        let android = IdWithIdentifier::new(
            ApplicationIdentifier::Class,
            "android(splash)",
            MatchingStrategy::Legacy,
        );
        assert!(android.matches(&WindowInfo::new("", "android(splash)", "", "")));

        // A legacy id stays ambiguous on purpose: `EVERYTHING_(1.5a)` read as a
        // regular expression also matches `EVERYTHING_1x5a`, and a rule file
        // that meant the pattern keeps working. No such class exists; the class
        // that does exist is the one that used to be missed.
        assert!(everything.matches(&WindowInfo::new("", "EVERYTHING_1x5a", "", "")));
    }

    #[test]
    fn a_legacy_id_is_never_a_substring_match() {
        let rule = IdWithIdentifier::new(
            ApplicationIdentifier::Exe,
            "a.exe",
            MatchingStrategy::Legacy,
        );
        assert!(rule.matches(&WindowInfo::new("", "", "a.exe", "")));
        assert!(
            !rule.matches(&WindowInfo::new("", "", "extra.exec", "")),
            "a legacy id describes the whole value, not a part of it"
        );
    }

    #[test]
    fn an_empty_id_matches_no_window_and_is_reported() {
        for strategy in [
            MatchingStrategy::Legacy,
            MatchingStrategy::Equals,
            MatchingStrategy::StartsWith,
            MatchingStrategy::EndsWith,
            MatchingStrategy::Contains,
            MatchingStrategy::Regex,
            MatchingStrategy::DoesNotEqual,
            MatchingStrategy::DoesNotContain,
        ] {
            let rule = IdWithIdentifier::new(ApplicationIdentifier::Exe, "", strategy);
            assert!(
                !rule.matches(&code()),
                "{strategy:?} with an empty id must not match"
            );
            assert!(
                rule.validate().is_err(),
                "{strategy:?} with an empty id must be reported"
            );
        }

        let mut sets = RuleSets::new();
        sets.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Title,
            "",
            MatchingStrategy::Contains,
        ));
        assert_eq!(
            sets.decide(&code()),
            RuleDecision::Tile,
            "an empty ignore rule must not disable the window manager"
        );
        assert_eq!(sets.validate().len(), 1);
    }

    #[test]
    fn an_exe_or_path_rule_ignores_the_case_windows_does_not_care_about() {
        let w = code();
        let exe = |id: &str, strategy| {
            IdWithIdentifier::new(ApplicationIdentifier::Exe, id, strategy).matches(&w)
        };
        assert!(exe("code.exe", MatchingStrategy::Equals));
        assert!(exe("CODE.EXE", MatchingStrategy::Legacy));
        assert!(exe("code", MatchingStrategy::StartsWith));
        assert!(exe(".EXE", MatchingStrategy::EndsWith));
        assert!(exe("ODE", MatchingStrategy::Contains));
        assert!(exe("^code\\.exe$", MatchingStrategy::Regex));
        assert!(!exe("code.exe", MatchingStrategy::DoesNotEqual));

        let path = |id: &str, strategy| {
            IdWithIdentifier::new(ApplicationIdentifier::Path, id, strategy).matches(&w)
        };
        assert!(path(r"c:\users\domin", MatchingStrategy::StartsWith));
        assert!(path("microsoft vs code", MatchingStrategy::Contains));

        // Class and Title stay byte exact: they are content, not file names.
        let title = IdWithIdentifier::new(
            ApplicationIdentifier::Title,
            "readme.md - mochi - visual studio code",
            MatchingStrategy::Equals,
        );
        assert!(!title.matches(&w));
        let class = IdWithIdentifier::new(
            ApplicationIdentifier::Class,
            "chrome_widgetwin_1",
            MatchingStrategy::Equals,
        );
        assert!(!class.matches(&w));
    }

    #[test]
    fn a_rule_that_is_reported_as_dropped_is_really_dropped() {
        let mut sets = RuleSets::new();
        sets.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Title,
            "([unclosed",
            MatchingStrategy::Regex,
        ));
        sets.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "",
            MatchingStrategy::Contains,
        ));
        let keeper = MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "zebar.exe",
            MatchingStrategy::Equals,
        );
        sets.ignore_rules.push(keeper.clone());

        let dropped = sets.drop_invalid();
        assert_eq!(dropped.len(), 2);
        assert!(
            dropped
                .iter()
                .any(|e| matches!(e, Error::InvalidRegex { .. }))
        );
        assert!(
            dropped
                .iter()
                .any(|e| matches!(e, Error::EmptyRuleId { .. }))
        );
        assert_eq!(sets.ignore_rules, vec![keeper]);
        assert!(sets.validate().is_empty(), "nothing broken is left behind");
        assert!(sets.drop_invalid().is_empty(), "a second pass is quiet");
    }

    #[test]
    fn a_mistyped_list_name_is_reported_instead_of_being_dropped_in_silence() {
        let json = r#"{
            "$schema": "https://example.invalid/schema.json",
            "Calculator": {
                "floating_applications": [{ "kind": "Exe", "id": "Calculator.exe", "matching_strategy": "Equals" }]
            },
            "Steam": {
                "ignore": [{ "kind": "Exe", "id": "steam.exe", "matching_strategy": "Equals" }]
            },
            "Nonsense": 5
        }"#;
        let (sets, warnings) =
            load_app_specific_configuration_reporting_unknown_keys(json).unwrap();
        assert_eq!(sets.floating_applications.len(), 0);
        assert_eq!(sets.ignore_rules.len(), 1, "the good app still loads");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("floating_applications")
                    && w.contains("did you mean \"floating\"")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("Nonsense")),
            "{warnings:?}"
        );
        assert!(load_app_specific_configuration(json).is_ok(), "not fatal");
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_not_memoised_as_a_failure() {
        let pattern = "([never memoised";
        assert!(compile(pattern).is_none());
        let cached = regex_cache().read().unwrap().contains_key(pattern);
        assert!(!cached, "a compile failure must not be cached forever");
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
