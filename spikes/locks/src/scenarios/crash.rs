//! A holder aborts while registered; the next registration must succeed and
//! leave nothing of the dead entry behind.

use super::shared::running;
use super::Worker;
use crate::coord::{Coordinator, Filter};
use crate::report::{check, info};

const S: &str = "crash";

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let mut holder = Worker::spawn("w-hold", &[d, "all", "-", "abort"]);
    holder.until(&["HELD"]);
    let before = running(dir.path());
    check(S, before.len() == 2, format!("holder's files while alive: {before:?}"));
    holder.release();
    let status = holder.wait();
    check(S, !status.success(), format!("holder aborted: {status}"));
    let coord = Coordinator::new(dir.path());
    let reg = coord.register("driver", &[], &Filter::All);
    info(
        S,
        format!("All registration after the holder died succeeded ({} waits)", reg.waits),
    );
    let during = running(dir.path());
    check(
        S,
        during.len() == 2 && during.iter().all(|f| !before.contains(f)),
        format!("dead entry's files are gone: {during:?}"),
    );
    drop(reg);
    let after = running(dir.path());
    check(S, after.is_empty(), format!("running/ empty afterwards: {after:?}"));
}
