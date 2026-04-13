//! Version computation and validation for the `skuld` workspace.
//!
//! - **Workspace version**: the strict `MAJOR.MINOR.PATCH` declared in each
//!   publishable member's `Cargo.toml`. All publishable members must agree.
//! - **Dep pin**: `skuld`'s `skuld-macros` dependency must carry exact pin
//!   `=<workspace-version>`; otherwise stage 2 would publish skuld against a
//!   stale skuld-macros version (or fail at crates.io verify).
//! - **Tag version**: the nearest ancestor tag matching `v[0-9]+.[0-9]+.[0-9]+`.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
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

/// Validate the workspace version against the nearest git tag. With `exact`,
/// require equality (release CI); otherwise allow one patch/minor/major bump.
pub fn validate_against_tag(repo_root: &Path, exact: bool) -> Result<Version> {
    let cargo_ver = workspace_version(repo_root)?;
    let tag_ver = nearest_tag_version(repo_root)?;

    if exact {
        if cargo_ver != tag_ver {
            return Err(anyhow!("Cargo.toml version ({cargo_ver}) != tag version ({tag_ver})"));
        }
    } else if !is_valid_next(&tag_ver, &cargo_ver) {
        return Err(anyhow!(
            "Cargo.toml version ({cargo_ver}) is not a valid successor of tag version ({tag_ver});\n\
             allowed: same, or one patch/minor/major bump"
        ));
    }
    Ok(cargo_ver)
}

fn nearest_tag_version(repo_root: &Path) -> Result<Version> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--match", "v[0-9]*.[0-9]*.[0-9]*", "--abbrev=0"])
        .current_dir(repo_root)
        .output()
        .context("failed to spawn git describe")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git describe failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let tag = String::from_utf8(out.stdout)?.trim().to_string();
    let s = tag
        .strip_prefix('v')
        .ok_or_else(|| anyhow!("tag '{tag}' missing 'v' prefix"))?;
    let v = Version::parse(s).with_context(|| format!("tag '{tag}' is not valid semver"))?;
    if !v.pre.is_empty() || !v.build.is_empty() {
        return Err(anyhow!("tag '{tag}' must be strict vMAJOR.MINOR.PATCH"));
    }
    Ok(v)
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
