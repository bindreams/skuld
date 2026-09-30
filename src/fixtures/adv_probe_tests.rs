//! THROWAWAY adversarial probe for PR #97. Do not merge.
use super::temp_dir::{create_dir, TempDir};
use crate::win_nt::{open_dir, standard_info, test_support::mark_for_deletion};
use std::path::{Path, PathBuf};

fn listing(p: &Path) -> Vec<String> {
    match std::fs::read_dir(p) {
        Ok(r) => r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(),
        Err(e) => vec![format!("<{e}>")],
    }
}

#[test]
fn adv_probe_reserved_names() {
    for prefix in ["NUL.x", "con .y", "COM1.z", "CONIN$.q", "COM\u{b9}.w", "aux", "nul.json", "plain.json"] {
        let parent = TempDir::new().unwrap();
        match TempDir::with_prefix_in(prefix, parent.path()) {
            Ok(d) => {
                let p = d.path().to_path_buf();
                let is_dir = p.is_dir();
                let canon = p.canonicalize();
                let write = std::fs::write(p.join("f"), b"x");
                let r = d.close();
                eprintln!(
                    "PROBE reserved skuld {prefix:?}: name={:?} is_dir={is_dir} canon={canon:?} write_inner={write:?} close={r:?} parent_after={:?}",
                    p.file_name().unwrap(),
                    listing(parent.path())
                );
            }
            Err(e) => eprintln!("PROBE reserved skuld {prefix:?}: create err {e}"),
        }
        let parent2 = TempDir::new().unwrap();
        match tempfile::Builder::new().prefix(&format!("{prefix}-")).tempdir_in(parent2.path()) {
            Ok(d) => {
                let p = d.path().to_path_buf();
                let is_dir = p.is_dir();
                let canon = p.canonicalize();
                let r = d.close();
                eprintln!(
                    "PROBE reserved tempfile {prefix:?}: name={:?} is_dir={is_dir} canon={canon:?} close={r:?} parent_after={:?}",
                    p.file_name().unwrap(),
                    listing(parent2.path())
                );
            }
            Err(e) => eprintln!("PROBE reserved tempfile {prefix:?}: create err {e}"),
        }
    }
}

#[test]
fn adv_probe_deleted_parent() {
    let tmp = TempDir::new().unwrap();
    for variant in ["posix-mark", "std-remove_dir", "classic-mark"] {
        let parent = tmp.join(variant);
        std::fs::create_dir(&parent).unwrap();
        let h = open_dir(&parent).unwrap();
        let _classic = match variant {
            "posix-mark" => {
                drop(mark_for_deletion(&parent, true));
                None
            }
            "std-remove_dir" => {
                eprintln!("PROBE {variant}: remove_dir -> {:?}", std::fs::remove_dir(&parent));
                None
            }
            _ => Some(mark_for_deletion(&parent, false)),
        };
        eprintln!(
            "PROBE {variant}: exists={} pending={:?}",
            parent.exists(),
            standard_info(&h).map(|i| i.DeletePending)
        );
        let r = create_dir(&h, &parent, &parent.join("p"));
        eprintln!("PROBE {variant}: create_dir -> {:?}", r.as_ref().map_err(|e| (e.kind(), e.to_string())));
        let mut n = 0u64;
        let made = tempfile::Builder::new()
            .prefix("x-")
            .rand_bytes(6)
            .disable_cleanup(true)
            .make_in(&parent, |p| {
                n += 1;
                create_dir(&h, &parent, p)
            });
        eprintln!(
            "PROBE {variant}: make_in calls={n} -> {:?}",
            made.map(|_| ()).map_err(|e| (e.kind(), e.to_string()))
        );
    }
}

fn acl(p: &Path) -> Vec<String> {
    let out = std::process::Command::new("icacls").arg(p).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let ps = p.display().to_string();
    text.lines()
        .map(|l| l.replace(&ps, "").trim().to_owned())
        .filter(|l| !l.is_empty() && !l.starts_with("Successfully"))
        .collect()
}

#[test]
fn adv_probe_acl() {
    for base in [std::env::temp_dir(), PathBuf::from(r"C:\Windows\Temp")] {
        let s = TempDir::new_in(&base).unwrap();
        let t = tempfile::Builder::new().tempdir_in(&base).unwrap();
        let (pa, sa, ta) = (acl(&base), acl(s.path()), acl(t.path()));
        eprintln!("PROBE acl base {base:?}\n  parent  : {pa:?}\n  skuld   : {sa:?}\n  tempfile: {ta:?}\n  skuld==tempfile: {}", sa == ta);
    }
}

#[test]
fn adv_probe_drive_relative() {
    let cwd = std::env::current_dir().unwrap();
    let rel = PathBuf::from(format!("{}rel", &cwd.to_str().unwrap()[..2]));
    let j = cwd.join(&rel);
    eprintln!("PROBE drive-relative: cwd={cwd:?} rel={rel:?} cwd.join(rel)={j:?} is_absolute={}", j.is_absolute());
    std::fs::create_dir_all(cwd.join("rel")).unwrap();
    let d = TempDir::new_in(&rel).unwrap();
    eprintln!("PROBE drive-relative skuld: {:?} is_absolute={}", d.path(), d.path().is_absolute());
    drop(d);
    let _ = std::fs::remove_dir(cwd.join("rel"));
}
