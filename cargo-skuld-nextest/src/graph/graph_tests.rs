use super::*;

fn t(binary_id: &str, name: &str, labels: &[&str], serial_filter: &str) -> TestMetadata {
    TestMetadata {
        binary_id: binary_id.into(),
        name: name.into(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        serial_filter: serial_filter.into(),
    }
}

#[test]
fn no_conflicts_yields_no_groups() {
    assert!(build_groups(&[t("bin", "a", &[], ""), t("bin", "b", &[], "")]).is_empty());
}

#[test]
fn global_serial_groups_transitively_with_everything() {
    let tests = vec![t("bin", "a", &[], "*"), t("bin", "b", &[], ""), t("bin", "c", &[], "")];
    let groups = build_groups(&tests);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].members.len(), 3);
}

#[test]
fn label_filter_only_groups_matching_pair() {
    let tests = vec![
        t("bin", "locker", &[], "shared"),
        t("bin", "user", &["shared"], ""),
        t("bin", "unrelated", &[], ""),
    ];
    let groups = build_groups(&tests);
    assert_eq!(groups.len(), 1);
    let names: Vec<&str> = groups[0].members.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"locker") && names.contains(&"user") && !names.contains(&"unrelated"));
}

#[test]
fn disjoint_label_filters_stay_isolated() {
    let tests = vec![
        t("bin", "gpu_locker", &[], "gpu"),
        t("bin", "gpu_user", &["gpu"], ""),
        t("bin", "db_locker", &[], "db"),
        t("bin", "db_user", &["db"], ""),
    ];
    assert_eq!(build_groups(&tests).len(), 2);
}

#[test]
fn cross_binary_conflict_groups_across_binary_ids() {
    let tests = vec![t("bin-a", "locker", &[], "shared"), t("bin-b", "user", &["shared"], "")];
    let groups = build_groups(&tests);
    let binary_ids: Vec<&str> = groups[0].members.iter().map(|(b, _)| b.as_str()).collect();
    assert!(binary_ids.contains(&"bin-a") && binary_ids.contains(&"bin-b"));
}

#[test]
fn deterministic_regardless_of_input_order() {
    let forward = vec![
        t("bin", "a_locker", &[], "shared"),
        t("bin", "b_user", &["shared"], ""),
        t("bin", "c_isolated", &[], ""),
        t("bin", "d_locker2", &[], "shared2"),
        t("bin", "e_user2", &["shared2"], ""),
    ];
    let mut shuffled = forward.clone();
    shuffled.reverse();
    shuffled.swap(0, 2);
    assert_eq!(build_groups(&forward), build_groups(&shuffled));
}

/// A group's name must depend only on its own membership, never on
/// which other groups happen to exist — otherwise an unrelated group
/// change would rename this one and show up as spurious diff noise in
/// the committed `gen` output.
#[test]
fn group_name_is_stable_when_unrelated_groups_change() {
    let base = vec![t("bin", "locker", &[], "shared"), t("bin", "user", &["shared"], "")];
    let mut with_extra = base.clone();
    with_extra.push(t("bin", "other_locker", &[], "other"));
    with_extra.push(t("bin", "other_user", &["other"], ""));

    let name_alone = build_groups(&base)[0].name.clone();
    let name_with_extra = build_groups(&with_extra)
        .into_iter()
        .find(|g| g.members.iter().any(|(_, n)| n == "locker"))
        .expect("the original group must still be present")
        .name;
    assert_eq!(name_alone, name_with_extra);
}
