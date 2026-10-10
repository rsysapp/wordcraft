//! Installed system fonts: found by family name by every lookup, whatever ran before it (#22).

use std::path::{Path, PathBuf};

use super::*;

const FAMILY: &str = "Sysfont Sans3";

/// A bundled Source Sans 3 file renamed [`FAMILY`] (as long as the original name).
fn renamed(file: &str) -> Vec<u8> {
    renamed_as(file, FAMILY)
}

/// A bundled Source Sans 3 file renamed `family`, which must be as long as "Source Sans 3".
fn renamed_as(file: &str, family: &str) -> Vec<u8> {
    assert_eq!(family.len(), "Source Sans 3".len());
    let mut data = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts").join(file)).unwrap();
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    for (from, to) in [(utf16("Source Sans 3"), utf16(family)), (b"Source Sans 3".to_vec(), family.as_bytes().to_vec())] {
        let mut i = 0;
        while let Some(at) = data[i..].windows(from.len()).position(|w| w == from) {
            data[i + at..i + at + to.len()].copy_from_slice(&to);
            i += at + to.len();
        }
    }
    data
}

/// The fonts in `faces` as one collection (TTC) file.
fn collection(faces: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"ttcf".to_vec();
    out.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
    out.extend_from_slice(&(faces.len() as u32).to_be_bytes());
    let mut base = 12 + 4 * faces.len();
    for f in faces {
        out.extend_from_slice(&(base as u32).to_be_bytes());
        base += f.len();
    }
    for f in faces {
        // Table offsets count from the start of the collection.
        let start = out.len() as u32;
        let mut f = f.clone();
        let tables = u16::from_be_bytes([f[4], f[5]]) as usize;
        for r in 0..tables {
            let at = 12 + r * 16 + 8;
            let off = u32::from_be_bytes(f[at..at + 4].try_into().unwrap()) + start;
            f[at..at + 4].copy_from_slice(&off.to_be_bytes());
        }
        out.extend_from_slice(&f);
    }
    out
}

/// A font folder (with a subfolder, a damaged font and a file that isn't a font) holding
/// [`FAMILY`] Regular and Bold.
fn font_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dc-sysfonts-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Sub")).unwrap();
    std::fs::write(dir.join("Sysfont-Regular.ttf"), renamed("SourceSans3-Regular.ttf")).unwrap();
    std::fs::write(dir.join("Sub/Sysfont-Bold.TTF"), renamed("SourceSans3-Bold.ttf")).unwrap();
    std::fs::write(dir.join("damaged.ttf"), b"ttcf\0\x01\0\0\xff\xff\xff\xff").unwrap();
    std::fs::write(dir.join("readme.txt"), FAMILY).unwrap();
    dir
}

fn has(list: &[String], family: &str) -> bool {
    list.iter().any(|f| f == family)
}

/// A database whose font folders are `dirs` and that has no faces loaded (not even the bundled
/// ones), so a test decides exactly which loaded face covers what.
fn without_bundled(dirs: Vec<PathBuf>) -> FontDb {
    let db = FontDb::with_font_dirs(dirs);
    db.faces.write().unwrap().clear();
    db
}

/// A meeting point for `n` threads that fails the test instead of hanging when one of them never
/// arrives (a lookup that took another path, or panicked first).
struct Rendezvous {
    n: usize,
    /// (arrived in this round, rounds completed).
    state: std::sync::Mutex<(usize, usize)>,
    met: std::sync::Condvar,
}

impl Rendezvous {
    fn new(n: usize) -> Arc<Self> {
        Arc::new(Rendezvous { n, state: std::sync::Mutex::new((0, 0)), met: std::sync::Condvar::new() })
    }

    fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        let round = state.1;
        state.0 += 1;
        if state.0 == self.n {
            *state = (0, round + 1);
            self.met.notify_all();
            return;
        }
        let (_state, timeout) = self.met.wait_timeout_while(state, std::time::Duration::from_secs(10), |s| s.1 == round).unwrap();
        assert!(!timeout.timed_out(), "a thread never reached the meeting point");
    }

    /// A hook that meets the others `times` times.
    fn hook(self: &Arc<Self>, times: usize) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let meet = self.clone();
        Some(Arc::new(move || (0..times).for_each(|_| meet.wait())))
    }
}

#[test]
fn every_lookup_by_name_finds_installed_fonts_in_a_fresh_database() {
    let dir = font_dir("fresh");
    // Each lookup is the first thing a new database is asked. A folder listed twice is read once.
    let db = || FontDb::with_font_dirs(vec![dir.clone(), dir.join("Sub/..")]);
    let f = db().face("sysfont sans3", "Bold");
    assert_eq!((f.family.as_str(), f.style.as_str()), (FAMILY, "Bold"));
    assert!(db().has_family(FAMILY));
    assert!(has(&db().families(), FAMILY));
    assert_eq!(db().styles(FAMILY), ["Regular", "Bold"]);
    assert_eq!(db().load_system_fonts(), 2);
    // Missing fonts stay missing.
    let db = db();
    assert!(!db.has_family("No Such Font"));
    assert_eq!(db.face("No Such Font", "Regular").family, FALLBACK_FAMILY);
}

#[test]
fn the_scan_reads_collections_and_loads_fonts_only_when_used() {
    let dir = font_dir("collection");
    std::fs::remove_file(dir.join("Sysfont-Regular.ttf")).unwrap();
    std::fs::remove_file(dir.join("Sub/Sysfont-Bold.TTF")).unwrap();
    std::fs::write(dir.join("Sysfont.ttc"), collection(&[renamed("SourceSans3-Regular.ttf"), renamed("SourceSans3-Bold.ttf")])).unwrap();
    let db = FontDb::with_font_dirs(vec![dir.clone()]);
    assert_eq!(db.styles(FAMILY), ["Regular", "Bold"]);
    // Cataloged, not loaded.
    assert!(!db.is_loaded(FAMILY));
    let bold = db.face(FAMILY, "Bold");
    assert_eq!((bold.style.as_str(), bold.index()), ("Bold", 1));
    assert_eq!(db.face(FAMILY, "Regular").index(), 0, "the collection's faces load together");
}

#[test]
fn rescanning_finds_fonts_installed_since() {
    let dir = font_dir("rescan");
    let later = dir.join("Later");
    let db = FontDb::with_font_dirs(vec![later.clone()]);
    assert!(!db.has_family(FAMILY));
    std::fs::create_dir_all(&later).unwrap();
    std::fs::copy(dir.join("Sysfont-Regular.ttf"), later.join("Sysfont-Regular.ttf")).unwrap();
    assert!(!db.has_family(FAMILY), "the folders are scanned once, not on every miss");
    assert_eq!(db.load_system_fonts(), 1);
    assert!(db.has_family(FAMILY) && has(&db.families(), FAMILY));
}

#[test]
fn a_background_scan_serves_the_first_lookup() {
    let dir = font_dir("background");
    let db: &'static FontDb = Box::leak(Box::new(FontDb::with_font_dirs(vec![dir])));
    db.scan_in_background();
    // Waits for the scan when it is still running.
    assert!(has(&db.families(), FAMILY));
    assert_eq!(db.load_system_fonts(), 2, "a rescan catalogs the two faces again");
}

#[cfg(unix)]
#[test]
fn a_link_back_to_a_parent_folder_ends_the_scan() {
    let dir = font_dir("loop");
    std::os::unix::fs::symlink(&dir, dir.join("Sub/loop")).unwrap();
    let db = FontDb::with_font_dirs(vec![dir]);
    assert_eq!(db.load_system_fonts(), 2);
}

#[test]
fn the_scan_reads_names_without_loading_the_font() {
    let dir = font_dir("names");
    assert_eq!(file_face_names(&dir.join("Sysfont-Regular.ttf")), [(FAMILY.to_string(), "Regular".to_string())]);
    assert!(file_face_names(&dir.join("damaged.ttf")).is_empty());
    assert!(file_face_names(&dir.join("readme.txt")).is_empty());
    assert!(file_face_names(&dir.join("missing.ttf")).is_empty());
}

#[test]
fn lookups_racing_to_load_an_installed_font_all_find_it() {
    let dir = font_dir("race");
    for round in 0..20 {
        let db = FontDb::with_font_dirs(vec![dir.clone()]);
        db.load_system_fonts();
        let start = std::sync::Barrier::new(8);
        let found: Vec<String> = std::thread::scope(|s| {
            let lookups: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        db.face(FAMILY, "Regular").family.clone()
                    })
                })
                .collect();
            lookups.into_iter().map(|l| l.join().unwrap()).collect()
        });
        assert!(found.iter().all(|f| f == FAMILY), "round {round}: {found:?}");
        assert_eq!(db.read_faces().iter().filter(|f| f.family == FAMILY).count(), 2, "round {round}: each face loads once");
    }
}

#[test]
fn a_fallback_family_another_lookup_loaded_serves_the_character() {
    // Two preferred fallback families, each as long as the file's own name: "Hiragino Sans"
    // comes before "Apple Symbols" in the preferred order.
    let dir = std::env::temp_dir().join(format!("dc-sysfonts-{}-fallback-loaded", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Hiragino.ttf"), renamed_as("SourceSans3-Regular.ttf", "Hiragino Sans")).unwrap();
    std::fs::write(dir.join("Symbols.ttf"), renamed_as("SourceSans3-Regular.ttf", "Apple Symbols")).unwrap();
    let db = without_bundled(vec![dir]);
    db.load_system_fonts();
    assert!(!db.read_faces().iter().any(|f| f.covers('A')), "nothing loaded covers it yet");
    // Another lookup has loaded the first family, which covers the character.
    db.load_cataloged("Hiragino Sans");
    let attempts = db.load_attempts.load(Ordering::Relaxed);
    assert!(db.system_fallback('A', 0));
    assert!(!db.is_loaded("Apple Symbols"), "a later family is not loaded for nothing");
    assert_eq!(db.load_attempts.load(Ordering::Relaxed), attempts);
    assert!(!db.sys.lock().unwrap().misses.contains(&'A'), "no miss is remembered");
}

#[test]
fn the_runs_own_face_does_not_count_as_a_fallback() {
    let dir = font_dir("exclude");
    let db = without_bundled(vec![dir]);
    // The run's own face covers the character, but the fallback must be another face.
    db.add_font(renamed_as("SourceSans3-Regular.ttf", "Primary Sans3"));
    let own = db.face("Primary Sans3", "Regular");
    assert_eq!(own.family, "Primary Sans3");
    let fallback = db.fallback_for('A', own.id());
    assert_eq!(fallback.map(|f| f.family.clone()).as_deref(), Some(FAMILY), "the installed family is loaded and offered");
}

#[test]
fn a_family_that_fails_to_load_is_not_read_again_until_a_rescan() {
    let dir = font_dir("unloadable");
    let db = FontDb::with_font_dirs(vec![dir.clone()]);
    assert!(db.has_family(FAMILY));
    // Cataloged, then its files go before they load.
    let moved = dir.with_extension("moved");
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::rename(&dir, &moved).unwrap();
    let attempts = || db.load_attempts.load(Ordering::Relaxed);
    let (r, keep) = crate::word::resolve_in(&db, FAMILY, false, false);
    assert_eq!(r.face.family, FALLBACK_FAMILY);
    assert!(r.substituted && !keep, "the stand-in is marked and not kept");
    assert_eq!(attempts(), 1);
    // Later lookups go straight to the stand-in.
    let (r, keep) = crate::word::resolve_in(&db, FAMILY, true, false);
    assert!(r.substituted && !keep);
    assert_eq!(db.face(FAMILY, "Regular").family, FALLBACK_FAMILY);
    assert_eq!(attempts(), 1, "the failed family is not read again");
    // Fixed on disk: still remembered until a rescan, which lets it load.
    std::fs::rename(&moved, &dir).unwrap();
    assert_eq!(db.face(FAMILY, "Regular").family, FALLBACK_FAMILY);
    assert_eq!(attempts(), 1);
    db.load_system_fonts();
    let (r, keep) = crate::word::resolve_in(&db, FAMILY, false, false);
    assert_eq!(r.face.family, FAMILY);
    assert!(!r.substituted && keep);
    assert_eq!(attempts(), 2);
}

#[test]
fn a_recorded_failure_never_hides_a_loaded_face() {
    let dir = font_dir("failed-then-added");
    let db = FontDb::with_font_dirs(vec![dir.clone()]);
    db.load_system_fonts();
    let regular = std::fs::read(dir.join("Sysfont-Regular.ttf")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(db.face(FAMILY, "Regular").family, FALLBACK_FAMILY);
    assert!(db.failed.lock().unwrap().families.contains(&FAMILY.to_lowercase()));
    // Another load adds the family after the failure was recorded: the face is found.
    db.add_font(regular);
    assert_eq!(db.face(FAMILY, "Regular").family, FAMILY);
}

#[test]
fn a_failed_read_is_not_recorded_once_the_family_is_loaded() {
    let dir = font_dir("loaded-then-failed");
    let db = FontDb::with_font_dirs(vec![dir.clone()]);
    db.load_system_fonts();
    // A racing load succeeded; then this one's reads fail.
    db.add_font(std::fs::read(dir.join("Sysfont-Regular.ttf")).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
    db.load_cataloged(FAMILY);
    assert!(db.failed.lock().unwrap().families.is_empty(), "the success wins");
}

#[test]
fn a_failure_found_before_a_rescan_is_not_recorded_after_it() {
    let dir = font_dir("rescan-during-load");
    let db = FontDb::with_font_dirs(vec![dir.clone()]);
    db.load_system_fonts();
    let moved = dir.with_extension("moved");
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::rename(&dir, &moved).unwrap();
    let gate = Rendezvous::new(2);
    *db.load_gate.lock().unwrap() = gate.hook(2);
    std::thread::scope(|s| {
        // This lookup's reads fail; it waits before recording that.
        let lookup = s.spawn(|| db.face(FAMILY, "Regular").family.clone());
        gate.wait();
        // Meanwhile the files come back and the folders are scanned again.
        std::fs::rename(&moved, &dir).unwrap();
        *db.load_gate.lock().unwrap() = None;
        db.load_system_fonts();
        gate.wait();
        assert_eq!(lookup.join().unwrap(), FALLBACK_FAMILY);
    });
    assert_eq!(db.face(FAMILY, "Regular").family, FAMILY, "the stale failure was not recorded");
}

#[test]
fn a_rescan_between_the_scan_number_and_the_catalog_records_no_stale_failure() {
    // The files move within the font folder: the old catalog's paths stop working, the new
    // catalog's work.
    let dir = font_dir("rescan-before-snapshot");
    let db = FontDb::with_font_dirs(vec![dir.clone()]);
    db.load_system_fonts();
    let gate = Rendezvous::new(2);
    *db.snapshot_gate.lock().unwrap() = gate.hook(2);
    std::thread::scope(|s| {
        let lookup = s.spawn(|| db.load_cataloged(FAMILY));
        gate.wait();
        std::fs::rename(dir.join("Sysfont-Regular.ttf"), dir.join("Moved-Regular.ttf")).unwrap();
        std::fs::rename(dir.join("Sub"), dir.join("Moved")).unwrap();
        *db.snapshot_gate.lock().unwrap() = None;
        db.load_system_fonts();
        gate.wait();
        lookup.join().unwrap();
    });
    assert!(db.failed.lock().unwrap().families.is_empty(), "no failure recorded against the new catalog");
    assert_eq!(db.face(FAMILY, "Regular").family, FAMILY);
}

/// `data` (a single font) with an `fvar` table added: one `wght` axis, 200 to 900, default 400.
/// It then loads as a variable font, which no bundled font is. Outlines don't vary (no `gvar`),
/// which doesn't matter to how instances are made and kept.
fn with_weight_axis(data: &[u8]) -> Vec<u8> {
    let tables = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut fvar = Vec::new();
    // Version 1.0, axes at 16, reserved 2, one axis of 20 bytes, no named instances (size 8).
    for v in [1_u16, 0, 16, 2, 1, 20, 0, 8] {
        fvar.extend_from_slice(&v.to_be_bytes());
    }
    fvar.extend_from_slice(b"wght");
    for v in [200_i32, 400, 900] {
        fvar.extend_from_slice(&(v << 16).to_be_bytes());
    }
    fvar.extend_from_slice(&[0, 0, 1, 0]); // flags, name id 256
    // Every table moves down by the one new 16-byte directory record.
    let mut records: Vec<[u8; 16]> = (0..tables)
        .map(|r| {
            let mut rec: [u8; 16] = data[12 + r * 16..28 + r * 16].try_into().unwrap();
            let offset = u32::from_be_bytes(rec[8..12].try_into().unwrap()) + 16;
            rec[8..12].copy_from_slice(&offset.to_be_bytes());
            rec
        })
        .collect();
    let body = &data[12 + tables * 16..];
    let mut end = 12 + (tables + 1) * 16 + body.len();
    let pad = (4 - end % 4) % 4;
    end += pad;
    let mut rec = [0_u8; 16];
    rec[..4].copy_from_slice(b"fvar");
    rec[8..12].copy_from_slice(&(end as u32).to_be_bytes());
    rec[12..].copy_from_slice(&(fvar.len() as u32).to_be_bytes());
    records.push(rec);
    // Readers find tables by binary search over the sorted tags.
    records.sort_by_key(|r| [r[0], r[1], r[2], r[3]]);
    let mut out = data[..12].to_vec();
    out[4..6].copy_from_slice(&((tables + 1) as u16).to_be_bytes());
    records.iter().for_each(|r| out.extend_from_slice(r));
    out.extend_from_slice(body);
    out.extend(std::iter::repeat_n(0, pad));
    out.extend_from_slice(&fvar);
    out
}

#[test]
fn lookups_overlapping_to_make_a_variable_instance_make_it_once() {
    let db = FontDb::with_font_dirs(vec![]);
    assert_eq!(db.add_font(with_weight_axis(&renamed("SourceSans3-Regular.ttf"))), 1);
    assert!(db.axes(FAMILY, "Regular").iter().any(|a| a.0 == "wght"), "the font is variable");
    // Both lookups have missed the instance and made their own before either adds it.
    *db.instance_gate.lock().unwrap() = Rendezvous::new(2).hook(1);
    let style = "Regular {wght:650}";
    let ids: Vec<u32> = std::thread::scope(|s| {
        let lookups: Vec<_> = (0..2).map(|_| s.spawn(|| db.face(FAMILY, style).id())).collect();
        lookups.into_iter().map(|l| l.join().unwrap()).collect()
    });
    *db.instance_gate.lock().unwrap() = None;
    assert_eq!(ids[0], ids[1], "both lookups get the one instance");
    assert_eq!(db.read_faces().iter().filter(|f| f.style == style).count(), 1);
    let made = db.face(FAMILY, style);
    assert!(made.id() == ids[0] && made.coords.iter().any(|(t, v)| t == b"wght" && *v == 650.0));
}

#[test]
fn lookups_racing_to_make_a_variable_instance_make_it_once() {
    // An extra check on a real variable font, when one is installed.
    let Some(data) = ["/System/Library/Fonts/Supplemental/Skia.ttf", "/System/Library/Fonts/NewYork.ttf"].iter().find_map(|p| std::fs::read(p).ok())
    else {
        // No runtime skip in the test harness: say so loudly. The in-code variable font test
        // (`lookups_overlapping_to_make_a_variable_instance_make_it_once`) proves the fix.
        eprintln!("warning: SKIPPED: no variable system font installed; nothing checked here");
        return;
    };
    for round in 0..20 {
        let db = FontDb::with_font_dirs(vec![]);
        db.add_font(data.clone());
        let family = db.families().into_iter().find(|f| !FontDb::with_font_dirs(vec![]).has_family(f)).unwrap();
        let base = db.styles(&family)[0].clone();
        let style = format!("{base} {{wght:{}}}", 500 + round);
        let start = std::sync::Barrier::new(8);
        let ids: Vec<u32> = std::thread::scope(|s| {
            let lookups: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        db.face(&family, &style).id()
                    })
                })
                .collect();
            lookups.into_iter().map(|l| l.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|id| *id == ids[0]), "round {round}: {ids:?}");
        assert_eq!(db.read_faces().iter().filter(|f| f.style == style).count(), 1, "round {round}");
    }
}
