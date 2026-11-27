// A path rule matches a folder whatever the case of its letters, non-ASCII ones included.
use mochi_core::rules::{ApplicationIdentifier, MatchingRule, MatchingStrategy, WindowInfo};

#[test]
fn a_path_rule_matches_a_folder_with_an_umlaut_in_any_case() {
    // Only the umlaut differs in case; ASCII letters already fold today.
    let window = WindowInfo::new(
        "Tool",
        "ToolWnd",
        "tool.exe",
        r"C:\Users\MÜLLER\Tools\tool.exe",
    );
    let rule = MatchingRule::simple(
        ApplicationIdentifier::Path,
        r"C:\Users\müller\Tools".to_owned(),
        MatchingStrategy::StartsWith,
    );
    assert!(rule.matches(&window));
}
