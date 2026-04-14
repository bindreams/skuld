//! Version computation and validation for the `skuld` workspace.
//!
//! - **Workspace version**: the strict `MAJOR.MINOR.PATCH` declared in each
//!   publishable member's `Cargo.toml`. All publishable members must agree.
//! - **Dep pin**: `skuld`'s `skuld-macros` dependency must carry exact pin
//!   `=<workspace-version>`; otherwise stage 2 would publish skuld against a
//!   stale skuld-macros version (or fail at crates.io verify).
//! - **Nearest ancestor tag set**: the set of version tags matching
//!   `v[0-9]+.[0-9]+.[0-9]+` that are ancestors of HEAD and have no
//!   tagged descendants in HEAD's history. Computed by BFS from HEAD
//!   (via `gix`), pruning at every tagged commit. When independent
//!   branches carry tags and merge, the set has multiple co-equal
//!   members; the validator treats them as alternatives.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use gix::ObjectId;
use semver::Version;
use serde::Deserialize;

/// Read all publishable workspace member Cargo.tomls, assert agreement on a
/// strict MAJOR.MINOR.PATCH version, assert skuld's skuld-macros dep is
/// pinned to `=<that-version>`, and return the shared version.
pub fn workspace_version(repo_root: &Path) -> Result<Version> {
    let root_toml_path = repo_root.join("Cargo.toml");
    let root_toml = read_toml::<RootManifest>(&root_toml_path)?;
    let members = root_toml
        .workspace
        .ok_or_else(|| anyhow!("no [workspace] in {}", root_toml_path.display()))?
        .members;

    let mut shared: Option<Version> = None;
    let mut shared_source: String = String::new();

    for member in &members {
        let cargo_path = repo_root.join(member).join("Cargo.toml");
        let manifest = read_toml::<MemberManifest>(&cargo_path)?;

        let Some(package) = manifest.package else {
            continue;
        };
        if matches!(package.publish, Some(false)) {
            continue;
        }
        let Some(v_str) = package.version else {
            return Err(anyhow!("no [package] version in {}", cargo_path.display()));
        };
        let v = Version::parse(&v_str)
            .with_context(|| format!("{} version '{v_str}' is not valid semver", cargo_path.display()))?;
        if !v.pre.is_empty() || !v.build.is_empty() {
            return Err(anyhow!(
                "{} version must be strict MAJOR.MINOR.PATCH (no pre-release/build): {v}",
                cargo_path.display()
            ));
        }
        match &shared {
            None => {
                shared = Some(v);
                shared_source = cargo_path.display().to_string();
            }
            Some(existing) if existing != &v => {
                return Err(anyhow!(
                    "workspace members disagree on version:\n  {shared_source}: {existing}\n  {}: {v}",
                    cargo_path.display()
                ));
            }
            Some(_) => {}
        }
    }

    let shared = shared.ok_or_else(|| anyhow!("no publishable workspace members"))?;
    assert_skuld_macros_pin(repo_root, &shared)?;
    Ok(shared)
}

fn assert_skuld_macros_pin(repo_root: &Path, expected: &Version) -> Result<()> {
    let path = repo_root.join("Cargo.toml");
    let manifest = read_toml::<MemberManifest>(&path)?;
    let Some(deps) = manifest.dependencies else {
        return Err(anyhow!("{} has no [dependencies] table", path.display()));
    };
    let Some(dep) = deps.get("skuld-macros") else {
        return Err(anyhow!("{} is missing the skuld-macros dependency", path.display()));
    };
    let req = match dep {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("skuld-macros dep in {} has no version field", path.display()))?
            .to_string(),
        _ => return Err(anyhow!("skuld-macros dep in {} has unexpected shape", path.display())),
    };
    let want = format!("={expected}");
    if req != want {
        return Err(anyhow!(
            "skuld-macros dep pin is '{req}', expected '{want}' (pin must match the workspace version exactly)"
        ));
    }
    Ok(())
}

/// Validate the workspace version against the set of nearest ancestor
/// tags. With `exact`, Cargo.toml must equal at least one nearest tag
/// (release CI); otherwise it must be a valid-next (same or one-bump
/// successor in patch/minor/major) of at least one.
///
/// Empty nearest set: non-exact passes (no baseline in dev mode); exact
/// errors (release workflow always creates a local tag first).
pub fn validate_against_tag(repo_root: &Path, exact: bool) -> Result<Version> {
    let cargo_ver = workspace_version(repo_root)?;
    let nearest = nearest_ancestor_version_tags(repo_root)?;
    validate_cargo_against_nearest(&cargo_ver, &nearest, exact)?;
    Ok(cargo_ver)
}

/// A version tag's name, resolved commit SHA, and parsed strict semver.
#[derive(Clone, Debug)]
pub struct TagInfo {
    pub name: String,
    pub sha: ObjectId,
    pub version: Version,
}

/// Pure validator. Easy to unit-test without any git.
pub fn validate_cargo_against_nearest(cargo_ver: &Version, nearest: &[TagInfo], exact: bool) -> Result<()> {
    if nearest.is_empty() {
        if exact {
            return Err(anyhow!("no ancestor version tags found; --exact requires at least one"));
        }
        return Ok(()); // no baseline — pass in dev mode
    }
    if exact {
        if !nearest.iter().any(|t| t.version == *cargo_ver) {
            return Err(anyhow!(
                "Cargo.toml version {cargo_ver} does not match any nearest ancestor tag ({})",
                format_tag_list(nearest)
            ));
        }
    } else if !nearest.iter().any(|t| is_valid_next(&t.version, cargo_ver)) {
        return Err(anyhow!(
            "Cargo.toml version {cargo_ver} is not a valid successor of any nearest ancestor tag ({}); \
             allowed: same, or one patch/minor/major bump from any",
            format_tag_list(nearest)
        ));
    }
    Ok(())
}

fn format_tag_list(tags: &[TagInfo]) -> String {
    tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// BFS from HEAD, backwards through parents. At each commit, if it has
/// any matching tags, record them all in the nearest set and stop
/// exploring past it. Multiple tags on one commit are all kept as
/// co-equal candidates.
pub fn nearest_ancestor_version_tags(repo_root: &Path) -> Result<Vec<TagInfo>> {
    let repo = gix::open(repo_root).context("failed to open git repository")?;
    let tag_map = build_tag_map(&repo)?;

    let Ok(head_ref) = repo.head() else {
        // No HEAD (unborn, orphan, or bare with no refs) — no ancestors.
        return Ok(Vec::new());
    };
    let Some(head_id) = head_ref.id() else {
        return Ok(Vec::new()); // symbolic ref with no target
    };

    let mut nearest = Vec::new();
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    queue.push_back(head_id.detach());

    while let Some(id) = queue.pop_front() {
        if !visited.insert(id) {
            continue;
        }

        if let Some(tags) = tag_map.get(&id) {
            nearest.extend(tags.iter().cloned());
            continue; // any tag on this commit hides everything behind it
        }

        // Not tagged — explore parents. Skip non-commit objects defensively.
        let Ok(commit) = repo.find_commit(id) else {
            continue;
        };
        for parent in commit.parent_ids() {
            let parent_id = parent.detach();
            if !visited.contains(&parent_id) {
                queue.push_back(parent_id);
            }
        }
    }

    Ok(nearest)
}

/// All tags matching `v<strict-semver>`, grouped by target commit. When a
/// single commit carries multiple version tags, all are retained.
fn build_tag_map(repo: &gix::Repository) -> Result<HashMap<ObjectId, Vec<TagInfo>>> {
    let mut map: HashMap<ObjectId, Vec<TagInfo>> = HashMap::new();
    let references = repo.references().context("failed to list references")?;
    for r in references.tags().context("failed to iterate tag refs")? {
        let reference = r.map_err(|e| anyhow!("bad tag reference: {e}"))?;
        let name = reference.name().shorten().to_string();
        let Some(s) = name.strip_prefix('v') else {
            continue;
        };
        let Ok(version) = Version::parse(s) else {
            continue;
        };
        if !version.pre.is_empty() || !version.build.is_empty() {
            continue;
        }

        // Peel any annotated-tag chain to the commit.
        let target = reference.into_fully_peeled_id().context("peel failed")?;
        let object = repo.find_object(target).context("find_object failed")?;
        let Ok(commit) = object.peel_to_kind(gix::object::Kind::Commit) else {
            continue; // tag pointed at a tree/blob — not a version
        };
        let sha = commit.id;

        map.entry(sha).or_default().push(TagInfo { name, sha, version });
    }
    Ok(map)
}

pub fn is_valid_next(tag: &Version, cur: &Version) -> bool {
    cur == tag
        || (cur.major == tag.major && cur.minor == tag.minor && cur.patch == tag.patch + 1)
        || (cur.major == tag.major && cur.minor == tag.minor + 1 && cur.patch == 0)
        || (cur.major == tag.major + 1 && cur.minor == 0 && cur.patch == 0)
}

// TOML shapes ========================================================================================================

#[derive(Deserialize)]
struct RootManifest {
    workspace: Option<Workspace>,
}

#[derive(Deserialize)]
struct Workspace {
    members: Vec<String>,
}

#[derive(Deserialize)]
struct MemberManifest {
    package: Option<Package>,
    dependencies: Option<toml::value::Table>,
}

#[derive(Deserialize)]
struct Package {
    version: Option<String>,
    publish: Option<bool>,
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}
