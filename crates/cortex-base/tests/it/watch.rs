//! The modification-time check, on real files. Nothing here waits for a
//! clock to tick: file times are set, and the harness's clock is the fake.

use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use cortex_base::{AtomicWriteOptions, FakeClock, FileStamp, UtcTime, WatchList, write_atomic};

use crate::Scratch;

/// A time well in the past, so that a file stamped "now" is settled.
fn long_ago() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn set_modified(path: &Path, time: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .expect("open the file to set its time")
        .set_modified(time)
        .expect("set the modification time");
}

/// A file with known content and a modification time long before the
/// clock's.
fn settled_file(scratch: &Scratch, name: &str, contents: &str) -> std::path::PathBuf {
    let path = scratch.file(name, contents);
    set_modified(&path, long_ago());
    path
}

/// A clock that reads a day after [`long_ago`], and a list that uses it.
fn list() -> (FakeClock, WatchList) {
    let now = UtcTime::from_system(long_ago() + Duration::from_secs(86_400)).unwrap();
    let clock = FakeClock::new(now);
    let list = WatchList::new(Arc::new(clock.clone()));
    (clock, list)
}

#[test]
fn an_untouched_file_is_never_reported() {
    let scratch = Scratch::new("watch_untouched");
    let config = settled_file(&scratch, "config.toml", "a = 1");
    let (clock, mut list) = list();
    list.watch(&config);
    for _ in 0..5 {
        clock.advance(Duration::from_secs(60));
        assert!(list.check().is_empty());
    }
    assert_eq!(FileStamp::of(&config), FileStamp::of(&config));
    assert!(FileStamp::of(&config).exists());
}

#[test]
fn a_change_is_reported_once() {
    let scratch = Scratch::new("watch_changed");
    let config = settled_file(&scratch, "config.toml", "a = 1");
    let theme = settled_file(&scratch, "theme.toml", "accent = 1");
    let (_, mut list) = list();
    list.watch(&config);
    list.watch(&theme);
    assert_eq!(list.len(), 2);
    assert_eq!(
        list.paths().collect::<Vec<_>>(),
        [config.as_path(), theme.as_path()]
    );

    // New content of another length, written at the old time.
    fs::write(&config, "a = 1000").unwrap();
    set_modified(&config, long_ago());
    assert_eq!(
        list.check(),
        std::slice::from_ref(&config),
        "the size gave it away"
    );
    assert!(list.check().is_empty(), "reported once");

    // The same length at a later time.
    fs::write(&theme, "accent = 2").unwrap();
    set_modified(&theme, long_ago() + Duration::from_secs(5));
    assert_eq!(
        list.check(),
        std::slice::from_ref(&theme),
        "the time gave it away"
    );
    assert!(list.check().is_empty());

    // Both at once, in the order they were added.
    set_modified(&theme, long_ago() + Duration::from_secs(9));
    set_modified(&config, long_ago() + Duration::from_secs(9));
    assert_eq!(list.check(), [config, theme]);
}

#[test]
fn a_file_that_appears_or_disappears_is_a_change() {
    let scratch = Scratch::new("watch_missing");
    let optional = scratch.join("optional.toml");
    let (_, mut list) = list();
    list.watch(&optional);
    assert!(!FileStamp::of(&optional).exists());
    assert_eq!(FileStamp::of(&optional), FileStamp::Missing);
    assert!(list.check().is_empty(), "still missing");

    settled_file(&scratch, "optional.toml", "x");
    assert_eq!(
        list.check(),
        std::slice::from_ref(&optional),
        "it was created"
    );
    assert!(list.check().is_empty());

    fs::remove_file(&optional).unwrap();
    assert_eq!(
        list.check(),
        std::slice::from_ref(&optional),
        "it was removed"
    );
    assert!(list.check().is_empty());

    fs::create_dir(&optional).unwrap();
    assert_eq!(list.check(), [optional], "a directory took its place");
}

#[test]
fn a_file_replaced_by_an_atomic_write_is_a_change() {
    let scratch = Scratch::new("watch_replaced");
    let config = settled_file(&scratch, "config.toml", "a = 1");
    let (clock, mut list) = list();
    list.watch(&config);

    let unsynced = AtomicWriteOptions {
        sync: false,
        ..AtomicWriteOptions::default()
    };
    write_atomic(&config, b"a = 2", &unsynced, &clock).unwrap();
    // Same size. The new file's time is the real "now", which is not the
    // old file's.
    assert_eq!(list.check(), [config]);
}

/// On Unix the stamp also knows which file it is, so a replacement shows
/// even when the size and the modification time are made the same.
#[cfg(unix)]
#[test]
fn a_replacement_with_the_same_size_and_time_is_still_a_change() {
    let scratch = Scratch::new("watch_same_size_and_time");
    let config = settled_file(&scratch, "config.toml", "a = 1");
    let (_, mut list) = list();
    list.watch(&config);

    let replacement = settled_file(&scratch, "config.toml.new", "a = 2");
    fs::rename(&replacement, &config).unwrap();
    assert_eq!(
        fs::metadata(&config).unwrap().modified().unwrap(),
        long_ago(),
        "the times are the same"
    );
    assert_eq!(list.check(), [config]);
}

#[test]
fn a_stamp_taken_before_reading_catches_a_change_made_during_the_read() {
    let scratch = Scratch::new("watch_stamp_first");
    let config = settled_file(&scratch, "config.toml", "a = 1");
    let (_, mut list) = list();

    let stamp = FileStamp::of(&config);
    // The file changes while it is being read and parsed.
    fs::write(&config, "a = 22").unwrap();
    set_modified(&config, long_ago() + Duration::from_secs(30));
    list.watch_with(&config, stamp);

    assert_eq!(list.check(), [config], "the next check reloads it");
}

#[test]
fn a_file_written_just_now_is_reported_until_it_has_settled() {
    let scratch = Scratch::new("watch_unsettled");
    let config = scratch.file("config.toml", "a = 1");
    // The harness's clock reads exactly the file's time: it was written
    // "just now", and a second write could hide in the same tick.
    let written = fs::metadata(&config).unwrap().modified().unwrap();
    let clock = FakeClock::new(UtcTime::from_system(written).unwrap());
    let mut list = WatchList::new(Arc::new(clock.clone()));
    list.watch(&config);

    clock.advance(Duration::from_millis(500));
    assert_eq!(
        list.check(),
        std::slice::from_ref(&config),
        "unchanged, but too fresh to trust"
    );
    clock.advance(Duration::from_secs(3));
    assert_eq!(
        list.check(),
        std::slice::from_ref(&config),
        "the last stamp was taken while it was fresh"
    );
    clock.advance(Duration::from_secs(1));
    assert!(list.check().is_empty(), "settled");
    assert!(list.check().is_empty());
}

#[test]
fn checks_can_be_held_to_once_a_second() {
    let scratch = Scratch::new("watch_throttle");
    let config = settled_file(&scratch, "config.toml", "a = 1");
    let (clock, mut list) = list();
    list.watch(&config);
    let second = Duration::from_secs(1);

    assert_eq!(
        list.check_at_most_every(second),
        Some(vec![]),
        "the first check runs"
    );
    fs::write(&config, "a = 1000").unwrap();
    set_modified(&config, long_ago());
    assert_eq!(
        list.check_at_most_every(second),
        None,
        "too soon: nothing is looked at"
    );
    clock.advance(Duration::from_millis(999));
    assert_eq!(list.check_at_most_every(second), None);
    clock.advance(Duration::from_millis(1));
    assert_eq!(list.check_at_most_every(second), Some(vec![config.clone()]));
    assert_eq!(list.check_at_most_every(second), None);

    // A plain check always runs, and counts as the last one.
    set_modified(&config, long_ago() + Duration::from_secs(1));
    assert_eq!(list.check(), [config]);
    assert_eq!(list.check_at_most_every(second), None);
    assert!(clock.sleeps().is_empty(), "nothing here ever sleeps");
}

#[test]
fn the_list_can_be_changed() {
    let scratch = Scratch::new("watch_list");
    let first = settled_file(&scratch, "first.toml", "x");
    let second = settled_file(&scratch, "second.toml", "x");
    let (_, mut list) = list();
    assert!(list.is_empty());
    list.watch(&first);
    list.watch(&second);
    list.watch(&first);
    assert_eq!(list.len(), 2, "watching twice is watching once");

    // Watching again takes a fresh stamp, so an earlier change is forgotten.
    set_modified(&first, long_ago() + Duration::from_secs(7));
    list.watch(&first);
    assert!(list.check().is_empty());

    list.unwatch(&first);
    set_modified(&first, long_ago() + Duration::from_secs(8));
    set_modified(&second, long_ago() + Duration::from_secs(8));
    assert_eq!(list.check(), [second]);

    list.clear();
    assert!(list.is_empty());
    assert!(list.check().is_empty());
}
