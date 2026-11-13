//! Paths as text: the rules of both styles, tested on every OS. What needs a
//! real file system is in `tests/it/paths.rs`.

use std::collections::HashSet;
use std::ffi::OsStr;

use super::lexical::fold_char;
use super::*;
use crate::cases::{self, ensure};
use crate::rng::{Rng, SeededRng};

use PathStyle::{Posix, Windows};

fn abs(input: &str, style: PathStyle) -> AbsPath {
    AbsPath::parse(input, style).unwrap_or_else(|error| panic!("{input:?}: {error}"))
}

fn rel(input: &str, style: PathStyle) -> RelPath {
    RelPath::parse(input, style).unwrap_or_else(|error| panic!("{input:?}: {error}"))
}

#[test]
fn posix_paths_are_tidied() {
    let cases = [
        ("/", "/"),
        ("//", "/"),
        ("/srv/dev/project", "/srv/dev/project"),
        ("/srv//dev/./project/", "/srv/dev/project"),
        ("/srv/dev/../other", "/srv/other"),
        ("/../..", "/"),
        ("/a/b/../../../c", "/c"),
        ("/a/./././b", "/a/b"),
        ("/tmp/..", "/"),
        ("/a/b/..", "/a"),
        ("/with space/and\ttab", "/with space/and\ttab"),
        ("/ünïcödé/日本語", "/ünïcödé/日本語"),
    ];
    for (input, expected) in cases {
        let path = abs(input, Posix);
        assert_eq!(path.as_str(), expected, "{input:?}");
        assert_eq!(path.key(), expected, "POSIX compares the text itself");
        assert_eq!(path.to_string(), expected);
        assert_eq!(path.to_native_string(), expected);
        assert_eq!(path.to_fs_path(), std::path::PathBuf::from(expected));
    }
}

#[test]
fn windows_paths_are_tidied_and_shown_with_forward_slashes() {
    let cases = [
        (r"C:\", "C:/"),
        ("c:/", "C:/"),
        (r"C:\Work\Dev\Project", "C:/Work/Dev/Project"),
        ("C:/Work/Dev/Project", "C:/Work/Dev/Project"),
        (r"C:\Work/Dev\\Project\", "C:/Work/Dev/Project"),
        (r"C:\Work\.\Dev\..\Other", "C:/Work/Other"),
        (r"C:\..\..\Windows", "C:/Windows"),
        (r"d:\a\b\..\..\..\c", "D:/c"),
        (r"\\server\share", "//server/share/"),
        (r"\\server\share\", "//server/share/"),
        ("//server/share/dir/file.txt", "//server/share/dir/file.txt"),
        (r"\\server\share\dir\..\..\..\x", "//server/share/x"),
        (
            r"C:\with space\ünï\日本語.txt",
            "C:/with space/ünï/日本語.txt",
        ),
        (r"C:\.hidden\..config", "C:/.hidden/..config"),
        (r"C:\ leading space", "C:/ leading space"),
    ];
    for (input, expected) in cases {
        let path = abs(input, Windows);
        assert_eq!(path.as_str(), expected, "{input:?}");
        assert_eq!(path.to_string(), expected);
    }
}

#[test]
fn windows_refuses_what_could_name_a_device_or_another_file() {
    type Check = fn(&PathError) -> bool;
    let device: Check = |error| matches!(error, PathError::DevicePath { .. });
    let reserved: Check = |error| matches!(error, PathError::ReservedName { .. });
    let trailing: Check = |error| matches!(error, PathError::TrailingDotOrSpace { .. });
    let character: Check = |error| matches!(error, PathError::InvalidCharacter { .. });
    let drive_relative: Check = |error| matches!(error, PathError::DriveRelative { .. });
    let root_relative: Check = |error| matches!(error, PathError::RootRelative { .. });
    let share: Check = |error| matches!(error, PathError::MalformedShare { .. });

    let cases: &[(&str, Check)] = &[
        // The device namespace and the extended-length form, in every
        // spelling of the separators.
        (r"\\.\C:\Windows", device),
        (r"\\.\NUL", device),
        (r"\\.\PhysicalDrive0", device),
        (r"\\.\pipe\name", device),
        (r"\\?\C:\Windows", device),
        (r"\\?\UNC\server\share", device),
        (r"\\?\Volume{6f2a}\x", device),
        ("//./C:/Windows", device),
        ("//?/C:/Windows", device),
        (r"\/.\C:\Windows", device),
        (r"/\?\C:\Windows", device),
        (r"\\.", device),
        (r"\\?", device),
        // Device names, alone and in any position.
        ("NUL", reserved),
        ("nul", reserved),
        ("CON", reserved),
        ("con.txt", reserved),
        ("Nul.tar.gz", reserved),
        ("NUL .txt", reserved),
        ("PRN", reserved),
        ("aux.c", reserved),
        ("COM1", reserved),
        ("com9.log", reserved),
        ("COM0", reserved),
        ("LPT1", reserved),
        ("lpt0", reserved),
        ("COM¹", reserved),
        ("lpt³.txt", reserved),
        ("CONIN$", reserved),
        ("conout$", reserved),
        (r"C:\dir\NUL", reserved),
        (r"C:\NUL\file.txt", reserved),
        (r"src\aux.rs", reserved),
        ("a/b/con/c", reserved),
        (r"\\server\share\prn", reserved),
        (r"\\nul\share\x", reserved),
        // Names Windows would quietly read as another name.
        ("secret.env.", trailing),
        ("secret.env ", trailing),
        ("secret.env. .", trailing),
        ("dir./file", trailing),
        (r"C:\dir \file", trailing),
        ("...", trailing),
        (r"a\...\b", trailing),
        (". .", trailing),
        (" ", trailing),
        ("NUL.", trailing),
        ("COM1 ", trailing),
        // Alternate data streams, wildcards and control characters.
        ("secret.env::$DATA", character),
        ("secret.env:stream", character),
        ("dir:stream/file", character),
        (r"C:\dir\file.txt:zone.identifier", character),
        (r"C:\a\D:\b", character),
        ("what?.txt", character),
        ("*.rs", character),
        ("a<b", character),
        ("a>b", character),
        ("a|b", character),
        ("say \"hi\"", character),
        ("line\nbreak", character),
        ("tab\there", character),
        ("bell\u{7}", character),
        ("\\??\\C:\\x", root_relative),
        // Paths that depend on a current directory nobody stated.
        ("C:", drive_relative),
        ("C:file.txt", drive_relative),
        (r"c:dir\file", drive_relative),
        ("C:..", drive_relative),
        (r"\Windows", root_relative),
        ("/etc/passwd", root_relative),
        (r"\", root_relative),
        // Network paths that are not complete.
        (r"\\server", share),
        (r"\\server\", share),
        (r"\\\share\x", share),
        ("//", share),
        (r"\\server\\share", share),
    ];
    for (input, check) in cases {
        for (kind, outcome) in [
            ("absolute", AbsPath::parse(input, Windows).map(|_| ())),
            ("relative", RelPath::parse(input, Windows).map(|_| ())),
        ] {
            match outcome {
                // A relative path read as absolute, or the reverse, is
                // refused for that reason, which is also a refusal.
                Err(PathError::NotAbsolute { .. } | PathError::NotRelative { .. }) => {}
                Err(error) => {
                    assert!(check(&error), "{input:?} as {kind}: wrong refusal: {error}");
                    assert!(!error.to_string().is_empty());
                }
                Ok(()) => panic!("{input:?} was accepted as {kind}"),
            }
        }
        // And at least one of the two readings gave the precise reason.
        let precise = [
            AbsPath::parse(input, Windows).err(),
            RelPath::parse(input, Windows).err(),
        ]
        .into_iter()
        .flatten()
        .any(|error| check(&error));
        assert!(precise, "{input:?}: the reason was never given");
    }
}

#[test]
fn names_that_only_look_like_devices_are_ordinary() {
    for name in [
        "COM",
        "COM10",
        "COM1x",
        "COMA",
        "LPT",
        "LPT12",
        "NULL",
        "nul1",
        "CONSOLE",
        "con1",
        "a.NUL",
        ".nul",
        "AUXILIARY",
        "PRN2",
        "CONIN",
        "CONOUT$$",
        "COM⁴",
        "COM١",
        "xCOM1",
        "my con.txt",
        "~",
        "..config",
        ".git",
        "a b",
        "名前",
        "$Recycle.Bin",
        "file.txt.bak",
        "-dash",
        "a~1",
        "#hash",
        "100%",
        "a,b;c",
        "(x)[y]{z}",
        "a&b",
        "it's",
    ] {
        let path = rel(name, Windows);
        assert_eq!(path.as_str(), name);
        assert_eq!(abs(&format!(r"C:\{name}"), Windows).file_name(), Some(name));
    }
}

#[test]
fn the_windows_rules_do_not_apply_to_posix_paths() {
    // Every one of these is an ordinary file name on Linux.
    for name in [
        "notes:2026",
        "name.",
        "name ",
        "NUL",
        "con.txt",
        "COM1",
        "a\\b",
        "what?.txt",
        "*.rs",
        "a<b>c",
        "a|b",
        "say \"hi\"",
        "...",
        "C:",
        "C:file",
        "\\\\server\\share",
        "\\\\.\\NUL",
        "\\\\?\\C:\\x",
        "tab\there",
        "line\nbreak",
        " ",
        "~",
    ] {
        let path = rel(name, Posix);
        assert_eq!(path.as_str(), name, "{name:?} is one name");
        assert_eq!(path.names().count(), 1);
        assert_eq!(abs(&format!("/dir/{name}"), Posix).file_name(), Some(name));
    }
    // A backslash is not a separator there.
    assert_eq!(
        abs("/a\\..\\b", Posix).names().collect::<Vec<_>>(),
        ["a\\..\\b"]
    );
}

#[test]
fn every_style_refuses_an_empty_path_and_a_nul() {
    for style in [Posix, Windows] {
        assert!(matches!(AbsPath::parse("", style), Err(PathError::Empty)));
        assert!(matches!(RelPath::parse("", style), Err(PathError::Empty)));
        for input in ["a\0b", "\0", "dir/file\0.txt"] {
            let error = RelPath::parse(input, style).unwrap_err();
            assert!(matches!(error, PathError::ContainsNul { .. }), "{input:?}");
            assert!(
                !error.to_string().contains('\0'),
                "the message is printable"
            );
        }
    }
    assert!(matches!(
        AbsPath::parse("relative/path", Posix),
        Err(PathError::NotAbsolute { .. })
    ));
    assert!(matches!(
        AbsPath::parse(r"relative\path", Windows),
        Err(PathError::NotAbsolute { .. })
    ));
    assert!(matches!(
        RelPath::parse("/absolute", Posix),
        Err(PathError::NotRelative { .. })
    ));
    assert!(matches!(
        RelPath::parse(r"C:\absolute", Windows),
        Err(PathError::NotRelative { .. })
    ));
    assert!(matches!(
        RelPath::parse(r"\\server\share\x", Windows),
        Err(PathError::NotRelative { .. })
    ));
}

#[test]
fn relative_paths_stay_inside() {
    for style in [Posix, Windows] {
        assert_eq!(rel("src/main.rs", style).as_str(), "src/main.rs");
        assert_eq!(rel("./src//main.rs", style).as_str(), "src/main.rs");
        assert_eq!(rel("src/../docs/./a.md", style).as_str(), "docs/a.md");
        assert_eq!(rel("a/b/../..", style).as_str(), "");
        assert_eq!(rel(".", style).to_string(), ".");
        assert!(rel("./", style).is_empty());
        assert_eq!(rel("a/b", style).names().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(rel("a", style).style(), style);
        for escaping in [
            "..",
            "../x",
            "a/../..",
            "a/../../b",
            "./../a",
            "a/b/../../../c",
        ] {
            assert!(
                matches!(
                    RelPath::parse(escaping, style),
                    Err(PathError::Escapes { .. })
                ),
                "{escaping:?} climbs out"
            );
        }
    }
    assert_eq!(rel(r"src\sub\..\main.rs", Windows).as_str(), "src/main.rs");
    assert!(matches!(
        RelPath::parse(r"..\x", Windows),
        Err(PathError::Escapes { .. })
    ));
    assert_eq!(format!("{:?}", rel("a/b", Posix)), "RelPath(\"a/b\")");
}

#[test]
fn windows_compares_without_case_and_keeps_the_spelling() {
    let written = abs(r"C:\Work\Dev\Project\README.md", Windows);
    let other = abs("c:/work/DEV/project/readme.MD", Windows);
    assert_eq!(written, other);
    assert_eq!(
        written.as_str(),
        "C:/Work/Dev/Project/README.md",
        "its own spelling"
    );
    assert_eq!(other.as_str(), "C:/work/DEV/project/readme.MD");
    assert_eq!(written.key(), other.key());
    assert_eq!(written.key(), "C:/WORK/DEV/PROJECT/README.MD");

    let set: HashSet<AbsPath> = [written.clone(), other.clone()].into();
    assert_eq!(set.len(), 1, "they hash alike");
    assert_eq!(written.cmp(&other), std::cmp::Ordering::Equal);
    assert_eq!(written.partial_cmp(&other), Some(std::cmp::Ordering::Equal));

    assert_eq!(rel("SRC/Main.rs", Windows), rel("src/main.RS", Windows));
    assert_eq!(rel("Straße/É", Windows), rel("STRAßE/é", Windows));
    assert_ne!(rel("Strasse", Windows), rel("Straße", Windows));
    let set: HashSet<RelPath> = [rel("A/b", Windows), rel("a/B", Windows)].into();
    assert_eq!(set.len(), 1);

    // POSIX compares exactly.
    assert_ne!(abs("/Work/Dev", Posix), abs("/work/dev", Posix));
    assert_ne!(rel("SRC", Posix), rel("src", Posix));
    assert!(abs("/A", Posix) < abs("/a", Posix));
    // Paths of different styles are never the same path.
    assert_ne!(rel("a", Posix), rel("a", Windows));
}

#[test]
fn case_folding_follows_windows_one_to_one_upper_casing() {
    let cases = [
        ('a', 'A'),
        ('Z', 'Z'),
        ('é', 'É'),
        ('ø', 'Ø'),
        ('я', 'Я'),
        ('ω', 'Ω'),
        ('ǆ', 'Ǆ'),
        ('ǅ', 'Ǆ'),
        // Unicode says `I`. Windows keeps the dotless i apart.
        ('ı', 'ı'),
        // No one-to-one upper case: left alone.
        ('ß', 'ß'),
        ('ŉ', 'ŉ'),
        // Already upper case, and not the letter it looks like.
        ('İ', 'İ'),
        ('\u{212a}', '\u{212a}'),
        // Outside the Basic Multilingual Plane, Windows does not fold.
        ('𐐨', '𐐨'),
        ('1', '1'),
        ('/', '/'),
        ('日', '日'),
    ];
    for (character, expected) in cases {
        assert_eq!(fold_char(character), expected, "{character:?}");
    }
    assert_eq!(Windows.fold("src/Main.rs"), "SRC/MAIN.RS");
    assert!(matches!(Windows.fold("SRC/MAIN.RS"), Cow::Borrowed(_)));
    assert!(matches!(
        Posix.fold("src/Main.rs"),
        Cow::Borrowed("src/Main.rs")
    ));
    assert!(Windows.same_name("README.md", "readme.MD"));
    assert!(!Windows.same_name("README.md", "README.md2"));
    assert!(
        !Windows.same_name("K", "\u{212a}"),
        "the Kelvin sign is not K"
    );
    assert!(Posix.same_name("a", "a") && !Posix.same_name("a", "A"));
}

#[test]
fn containment_is_by_whole_names() {
    for (style, root) in [(Posix, "/work/app"), (Windows, r"C:\Work\App")] {
        let base = abs(root, style);
        let inside = base.join("src/main.rs").unwrap();
        assert!(inside.starts_with(&base));
        assert_eq!(inside.strip_prefix(&base).unwrap().as_str(), "src/main.rs");
        assert_eq!(
            base.strip_prefix(&base).unwrap().as_str(),
            "",
            "a path contains itself"
        );
        assert!(!base.starts_with(&inside));

        // A sibling whose name merely begins the same way is outside.
        let sibling = base.parent().unwrap().join("app2/src").unwrap();
        assert!(!sibling.starts_with(&base), "{sibling} is not under {base}");
        let sibling = base.parent().unwrap().join("ap").unwrap();
        assert!(!sibling.starts_with(&base));
        assert!(!base.parent().unwrap().starts_with(&base));
        assert_eq!(
            inside.strip_prefix_exactly(&base).unwrap().as_str(),
            "src/main.rs"
        );
        assert!(sibling.strip_prefix_exactly(&base).is_none());

        // Everything is under its root.
        assert!(inside.starts_with(&base.root()));
        assert_eq!(
            inside.strip_prefix(&inside.root()).unwrap().names().count(),
            inside.names().count()
        );
    }
    let base = abs(r"C:\Work\App", Windows);
    assert!(
        abs("c:/WORK/app/SRC", Windows).starts_with(&base),
        "case does not matter"
    );
    // The exact form is for two paths the file system spelled: there a
    // difference in case is a difference. The drive letter has no spelling
    // on disk and is compared as ever.
    assert!(
        abs("c:/WORK/app/SRC", Windows)
            .strip_prefix_exactly(&base)
            .is_none()
    );
    assert_eq!(
        abs("c:/Work/App/SRC", Windows)
            .strip_prefix_exactly(&base)
            .unwrap()
            .as_str(),
        "SRC"
    );
    assert!(
        abs("/Work/App/x", Posix)
            .strip_prefix_exactly(&base)
            .is_none()
    );
    assert_eq!(
        abs("c:/WORK/app/SRC/Main.rs", Windows)
            .strip_prefix(&base)
            .unwrap()
            .as_str(),
        "SRC/Main.rs",
        "the remainder keeps its spelling"
    );
    assert!(
        !abs(r"D:\Work\App\src", Windows).starts_with(&base),
        "another drive"
    );
    assert!(!abs(r"\\work\app\src", Windows).starts_with(&base));
    assert!(
        !abs("/Work/App/src", Posix).starts_with(&base),
        "another style"
    );
    assert!(!abs("/work/APP/src", Posix).starts_with(&abs("/work/app", Posix)));
    assert!(abs(r"\\Server\Share\Dir\x", Windows).starts_with(&abs("//server/share/dir", Windows)));
    assert!(!abs(r"\\server\share2\dir", Windows).starts_with(&abs(r"\\server\share", Windows)));
}

#[test]
fn a_path_can_be_taken_apart_and_extended() {
    let path = abs("/srv/dev/project/src", Posix);
    assert_eq!(
        path.names().collect::<Vec<_>>(),
        ["srv", "dev", "project", "src"]
    );
    assert_eq!(path.file_name(), Some("src"));
    assert_eq!(path.parent().unwrap().as_str(), "/srv/dev/project");
    assert_eq!(path.root().as_str(), "/");
    assert!(path.root().is_root());
    assert_eq!(path.root().parent(), None);
    assert_eq!(path.root().file_name(), None);
    assert_eq!(abs("/a", Posix).parent().unwrap().as_str(), "/");
    assert_eq!(path.style(), Posix);
    assert_eq!(path.share(), None);
    assert_eq!(format!("{path:?}"), "AbsPath(\"/srv/dev/project/src\")");

    let drive = abs(r"C:\a\b", Windows);
    assert_eq!(drive.parent().unwrap().as_str(), "C:/a");
    assert_eq!(drive.parent().unwrap().parent().unwrap().as_str(), "C:/");
    assert_eq!(drive.root().as_str(), "C:/");
    assert_eq!(drive.share(), None);

    let share = abs(r"\\server\share\dir", Windows);
    assert_eq!(share.share(), Some(("server", "share")));
    assert_eq!(share.parent().unwrap().as_str(), "//server/share/");
    assert!(share.parent().unwrap().is_root());
    assert_eq!(share.names().collect::<Vec<_>>(), ["dir"]);

    assert_eq!(
        path.join("a/../b").unwrap().as_str(),
        "/srv/dev/project/src/b"
    );
    assert_eq!(path.join(".").unwrap(), path);
    assert_eq!(path.root().join("etc").unwrap().as_str(), "/etc");
    assert!(matches!(path.join("../x"), Err(PathError::Escapes { .. })));
    assert!(matches!(
        path.join("/etc"),
        Err(PathError::NotRelative { .. })
    ));
    assert!(matches!(
        drive.join("nul"),
        Err(PathError::ReservedName { .. })
    ));
    assert_eq!(drive.join(r"c\d").unwrap().as_str(), "C:/a/b/c/d");
}

#[test]
fn a_directory_entry_is_checked_like_any_other_name() {
    let directory = abs("/work", Posix);
    assert_eq!(
        directory.child(OsStr::new("file.txt")).unwrap().as_str(),
        "/work/file.txt"
    );
    assert_eq!(
        directory.root().child(OsStr::new("etc")).unwrap().as_str(),
        "/etc"
    );
    assert_eq!(
        directory.child(OsStr::new("a:b.")).unwrap().as_str(),
        "/work/a:b."
    );
    for bad in ["", ".", "..", "a/b", "a\0b"] {
        assert!(
            matches!(
                directory.child(OsStr::new(bad)),
                Err(PathError::NotAName { .. })
            ),
            "{bad:?}"
        );
    }

    let directory = abs(r"C:\Work", Windows);
    assert_eq!(
        directory.child(OsStr::new("File.txt")).unwrap().as_str(),
        "C:/Work/File.txt"
    );
    assert!(matches!(
        directory.child(OsStr::new(r"a\b")),
        Err(PathError::NotAName { .. })
    ));
    // An entry with a name Windows itself would not open is refused, as one
    // entry: the walk that found it goes on.
    assert!(matches!(
        directory.child(OsStr::new("aux.txt")),
        Err(PathError::ReservedName { .. })
    ));
    assert!(matches!(
        directory.child(OsStr::new("trailing.")),
        Err(PathError::TrailingDotOrSpace { .. })
    ));
}

#[cfg(unix)]
#[test]
fn a_name_that_is_not_unicode_is_a_typed_refusal() {
    use std::os::unix::ffi::OsStrExt;
    let directory = abs("/work", Posix);
    let odd = OsStr::from_bytes(b"caf\xe9.txt");
    let error = directory.child(odd).unwrap_err();
    assert!(matches!(error, PathError::NotUnicode { .. }));
    assert!(error.to_string().contains("/work/caf"), "{error}");
    assert!(error.to_string().contains("not valid Unicode"), "{error}");

    let error =
        AbsPath::from_os(std::path::Path::new(OsStr::from_bytes(b"/work/\xff"))).unwrap_err();
    assert!(matches!(error, PathError::NotUnicode { .. }));
}

#[test]
fn windows_paths_have_a_native_and_an_extended_length_form() {
    let cases = [
        (r"C:\", r"C:\", r"\\?\C:\"),
        (r"c:\Work\Dev", r"C:\Work\Dev", r"\\?\C:\Work\Dev"),
        (
            r"\\server\share",
            r"\\server\share\",
            r"\\?\UNC\server\share\",
        ),
        (
            "//server/share/a/b",
            r"\\server\share\a\b",
            r"\\?\UNC\server\share\a\b",
        ),
    ];
    for (input, native, extended) in cases {
        let path = abs(input, Windows);
        assert_eq!(path.to_native_string(), native, "{input:?}");
        assert_eq!(
            path.to_fs_path(),
            std::path::PathBuf::from(extended),
            "{input:?}"
        );
        // What the OS hands back in that form reads as the same path.
        assert_eq!(AbsPath::from_os_text(extended, Windows).unwrap(), path);
        assert_eq!(AbsPath::from_os_text(native, Windows).unwrap(), path);
    }
    // Only the OS's own answers are read in the extended form, and only for
    // drives and shares.
    for refused in [
        r"\\?\Volume{6f2a-11}\x",
        r"\\?\GLOBALROOT\Device\x",
        r"\\?\C:",
        r"\\?\",
    ] {
        assert!(
            AbsPath::from_os_text(refused, Windows).is_err(),
            "{refused:?} should be refused"
        );
    }
    assert!(matches!(
        AbsPath::from_os_text(r"\\?\C:\dir\nul", Windows),
        Err(PathError::ReservedName { .. })
    ));
    assert_eq!(
        AbsPath::from_os_text("/a/b", Posix).unwrap().as_str(),
        "/a/b"
    );
}

// ---------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------

fn any_style(rng: &mut SeededRng) -> PathStyle {
    *cases::pick(rng, &[Posix, Windows])
}

/// Names that are valid in both styles, with what makes comparison hard.
const NAMES: &[&str] = &[
    "a",
    "b",
    "src",
    "Main.rs",
    "README.md",
    ".git",
    "..hidden",
    "with space",
    "ünï",
    "日本語",
    "x-1_2",
    "~",
    "a~1",
    "É",
    "ß",
    "COM10",
    "nul1",
];

/// Steps that change where a path leads without being names.
const NOISE: &[&str] = &[".", "", "..", "..", "."];

/// Things that have no business in a path, for the "never accepted
/// wrongly" properties.
const HOSTILE: &[&str] = &[
    "NUL",
    "con.txt",
    "COM1",
    "LPT¹",
    "CONOUT$",
    "trailing.",
    "trailing ",
    "...",
    "a:b",
    "x::$DATA",
    "what?",
    "*",
    "a<b",
    "a|b",
    "q\"q",
    "ctl\u{1}",
    "\\\\.\\x",
    "\\\\?\\x",
    "C:",
    "C:rel",
    "\\",
    "/",
    "//",
    "\\\\",
    "..",
    ".",
    "",
    " ",
    "\0",
    "~",
    "\u{feff}",
];

fn root_text(rng: &mut SeededRng, style: PathStyle) -> &'static str {
    match style {
        Posix => "/",
        Windows => cases::pick(rng, &[r"C:\", "c:/", r"\\server\share\", "//Server/Share/"]),
    }
}

fn separator(rng: &mut SeededRng, style: PathStyle) -> &'static str {
    match style {
        Posix => "/",
        Windows => cases::pick(rng, &["/", "\\"]),
    }
}

/// What a path of names and `.`/`..` steps should come to, worked out with
/// a plain stack. `None` when it climbs above where it started.
fn reference(steps: &[&str], may_climb_out: bool) -> Option<Vec<String>> {
    let mut stack: Vec<String> = Vec::new();
    for step in steps {
        match *step {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() && !may_climb_out {
                    return None;
                }
            }
            name => stack.push(name.to_owned()),
        }
    }
    Some(stack)
}

#[test]
fn every_short_path_matches_the_reference() {
    // Every sequence of up to six steps from a small alphabet, absolute and
    // relative, in both styles.
    let alphabet = ["a", "B", ".", "..", ""];
    let mut checked = 0;
    for length in 0..=6u32 {
        for number in 0..alphabet.len().pow(length) {
            let steps: Vec<&str> = (0..length)
                .map(|position| alphabet[number / alphabet.len().pow(position) % alphabet.len()])
                .collect();
            for style in [Posix, Windows] {
                let joined = steps.join("/");
                let root = if style == Posix { "/" } else { "C:/" };
                let absolute = AbsPath::parse(&format!("{root}{joined}"), style).unwrap();
                let expected = reference(&steps, true).unwrap();
                assert_eq!(
                    absolute.names().collect::<Vec<_>>(),
                    expected,
                    "{root}{joined}"
                );

                if joined.is_empty() {
                    continue;
                }
                // A first step that is empty would make the path absolute.
                let relative_input = format!("./{joined}");
                match (
                    RelPath::parse(&relative_input, style),
                    reference(&steps, false),
                ) {
                    (Ok(relative), Some(expected)) => {
                        assert_eq!(relative.names().collect::<Vec<_>>(), expected, "{joined}");
                    }
                    (Err(PathError::Escapes { .. }), None) => {}
                    (got, expected) => panic!("{joined:?}: got {got:?}, expected {expected:?}"),
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 30_000, "{checked} paths checked");
}

#[test]
fn property_tidying_is_idempotent_and_leaves_only_names() {
    cases::check(
        "path_idempotent",
        3000,
        |rng| {
            let style = any_style(rng);
            let absolute = rng.below(2) == 0;
            let mut input = String::new();
            if absolute {
                input.push_str(root_text(rng, style));
            }
            for index in 0..cases::small(rng, 8) {
                if index > 0 || rng.below(4) == 0 {
                    input.push_str(separator(rng, style));
                }
                let step = if rng.below(3) == 0 {
                    cases::pick(rng, NOISE)
                } else {
                    cases::pick(rng, NAMES)
                };
                input.push_str(step);
            }
            if rng.below(4) == 0 {
                input.push_str(separator(rng, style));
            }
            (style, input)
        },
        |(style, input)| {
            let style = *style;
            let check_names = |names: Vec<&str>| -> cases::Outcome {
                for name in names {
                    ensure!(
                        !name.is_empty() && name != "." && name != "..",
                        "step {name:?} left in"
                    );
                    ensure!(!name.contains('/'), "separator inside {name:?}");
                    ensure!(
                        style == Posix || !name.contains('\\'),
                        "separator inside {name:?}"
                    );
                }
                Ok(())
            };
            if let Ok(path) = AbsPath::parse(input, style) {
                check_names(path.names().collect())?;
                let again = AbsPath::parse(path.as_str(), style)?;
                ensure!(again.as_str() == path.as_str(), "{path} became {again}");
                ensure!(again == path, "not equal to itself after tidying");
                // The native and file-system forms read back as the same path.
                let native = AbsPath::from_os_text(&path.to_native_string(), style)?;
                ensure!(
                    native.as_str() == path.as_str(),
                    "native form reads as {native}"
                );
                let fs = AbsPath::from_os_text(&path.fs_string(), style)?;
                ensure!(
                    fs.as_str() == path.as_str(),
                    "file-system form reads as {fs}"
                );
            }
            if let Ok(path) = RelPath::parse(input, style) {
                check_names(path.names().collect())?;
                if !path.is_empty() {
                    let again = RelPath::parse(path.as_str(), style)?;
                    ensure!(again.as_str() == path.as_str(), "{path} became {again}");
                }
            }
            Ok(())
        },
    );
}

#[test]
fn property_noise_and_spelling_do_not_change_where_a_path_leads() {
    cases::check(
        "path_noise",
        3000,
        |rng| {
            let style = any_style(rng);
            let root = root_text(rng, style);
            let names: Vec<&str> = (0..cases::small(rng, 6))
                .map(|_| *cases::pick(rng, NAMES))
                .collect();
            // The same path, written untidily: `.` steps, doubled
            // separators, a detour through a directory and back, a trailing
            // separator, and on Windows either separator and any case.
            let mut noisy = String::from(root);
            for name in &names {
                match rng.below(5) {
                    0 => noisy.push_str("./"),
                    1 => {
                        noisy.push_str(cases::pick(rng, NAMES));
                        noisy.push_str("/../");
                    }
                    2 => noisy.push('/'),
                    _ => {}
                }
                if style == Windows && rng.below(2) == 0 {
                    noisy.push_str(&name.to_uppercase().replace("SS", "ß"));
                } else {
                    noisy.push_str(name);
                }
                noisy.push_str(separator(rng, style));
            }
            // With or without the trailing separator.
            if !names.is_empty() && rng.below(2) == 0 {
                noisy.pop();
            }
            (style, root, names, noisy)
        },
        |(style, root, names, noisy)| {
            let tidy = AbsPath::parse(&format!("{root}{}", names.join("/")), *style)?;
            let other = AbsPath::parse(noisy, *style)?;
            ensure!(tidy == other, "{noisy:?} read as {other}, not {tidy}");
            ensure!(tidy.key() == other.key(), "keys differ");
            ensure!(
                tidy.starts_with(&other) && other.starts_with(&tidy),
                "not within each other"
            );
            let relative = other
                .strip_prefix(&tidy.root())
                .ok_or("not under its root")?;
            ensure!(
                relative.names().count() == names.len(),
                "{} names became {}",
                names.len(),
                relative.names().count()
            );
            Ok(())
        },
    );
}

#[test]
fn property_dot_dot_follows_the_reference() {
    cases::check(
        "path_dot_dot",
        3000,
        |rng| {
            let style = any_style(rng);
            let steps: Vec<&str> = (0..cases::small(rng, 12))
                .map(|_| {
                    if rng.below(5) < 2 {
                        *cases::pick(rng, NOISE)
                    } else {
                        *cases::pick(rng, NAMES)
                    }
                })
                .collect();
            (style, root_text(rng, style), steps)
        },
        |(style, root, steps)| {
            let joined = steps.join("/");
            let absolute = AbsPath::parse(&format!("{root}{joined}"), *style)?;
            let expected = reference(steps, true).ok_or("the reference cannot climb out")?;
            ensure!(
                absolute.names().map(str::to_owned).collect::<Vec<_>>() == expected,
                "{joined:?} came to {absolute}"
            );
            match (
                RelPath::parse(&format!("./{joined}"), *style),
                reference(steps, false),
            ) {
                (Ok(relative), Some(expected)) => ensure!(
                    relative.names().map(str::to_owned).collect::<Vec<_>>() == expected,
                    "{joined:?} came to {relative}"
                ),
                (Err(PathError::Escapes { .. }), None) => {}
                (got, expected) => {
                    return Err(format!("got {got:?}, expected {expected:?}").into());
                }
            }
            Ok(())
        },
    );
}

#[test]
fn property_joining_stays_inside_and_comes_back_out() {
    cases::check(
        "path_containment",
        3000,
        |rng| {
            let style = any_style(rng);
            let mut base = String::from(root_text(rng, style));
            for _ in 0..cases::small(rng, 4) {
                base.push_str(cases::pick(rng, NAMES));
                base.push('/');
            }
            let relative: Vec<&str> = (0..cases::small(rng, 8))
                .map(|_| {
                    if rng.below(4) == 0 {
                        *cases::pick(rng, NOISE)
                    } else {
                        *cases::pick(rng, NAMES)
                    }
                })
                .collect();
            (style, base, relative.join("/"))
        },
        |(style, base, relative)| {
            let base = AbsPath::parse(base, *style)?;
            let relative_input = format!("./{relative}");
            match (
                base.join(&relative_input),
                RelPath::parse(&relative_input, *style),
            ) {
                (Ok(joined), Ok(relative)) => {
                    ensure!(joined.starts_with(&base), "{joined} is not under {base}");
                    let back = joined.strip_prefix(&base).ok_or("no way back")?;
                    ensure!(back == relative, "{back} is not {relative}");
                    ensure!(back.as_str() == relative.as_str(), "spelling changed");
                    // Reading the joined path as text gives the same place.
                    let reread = AbsPath::parse(joined.as_str(), *style)?;
                    ensure!(reread == joined, "{joined} reread as {reread}");
                    // A longer name is a sibling, not a descendant.
                    if let (Some(parent), Some(name)) = (base.parent(), base.file_name()) {
                        let sibling = parent.child(OsStr::new(&format!("{name}x")))?;
                        ensure!(
                            !sibling.starts_with(&base),
                            "{sibling} counted as under {base}"
                        );
                        ensure!(!joined.starts_with(&sibling), "{joined} under {sibling}");
                    }
                }
                (Err(PathError::Escapes { .. }), Err(PathError::Escapes { .. })) => {}
                (joined, relative) => {
                    return Err(format!("join gave {joined:?} and parse gave {relative:?}").into());
                }
            }
            Ok(())
        },
    );
}

#[test]
fn property_a_hostile_step_is_never_accepted_on_windows() {
    // A sound path with one bad step put anywhere in it. Whatever the
    // parser makes of the result, it must not be a path that still has the
    // bad step in it.
    const BAD: &[&str] = &[
        "NUL",
        "nul.txt",
        "COM1",
        "lpt9.dat",
        "LPT¹",
        "CONIN$",
        "aux",
        "PRN.x.y",
        "trailing.",
        "trailing ",
        "...",
        "a:b",
        "x::$DATA",
        "what?",
        "*",
        "a<b",
        "a>b",
        "a|b",
        "q\"q",
        "ctl\u{1}",
        "new\nline",
    ];
    cases::check(
        "path_hostile_step",
        3000,
        |rng| {
            let mut steps: Vec<&str> = (0..cases::small(rng, 6))
                .map(|_| *cases::pick(rng, NAMES))
                .collect();
            let bad = *cases::pick(rng, BAD);
            let position = rng.below(steps.len() as u64 + 1) as usize;
            steps.insert(position, bad);
            let separator = *cases::pick(rng, &["/", "\\"]);
            let root = *cases::pick(rng, &["", r"C:\", r"\\server\share\"]);
            (format!("{root}{}", steps.join(separator)), bad)
        },
        |(input, bad)| {
            ensure!(
                AbsPath::parse(input, Windows).is_err(),
                "accepted as an absolute path, with {bad:?} in it"
            );
            ensure!(
                RelPath::parse(input, Windows).is_err(),
                "accepted as a relative path, with {bad:?} in it"
            );
            Ok(())
        },
    );
}

#[test]
fn property_whatever_is_accepted_is_made_of_sound_names() {
    // Arbitrary text, most of it hostile. Reading it must never panic, and
    // anything it accepts must be made only of names that pass on their own.
    cases::check(
        "path_garbage",
        4000,
        |rng| {
            let style = any_style(rng);
            let mut input = String::new();
            for _ in 0..cases::small(rng, 8) {
                let piece = match rng.below(4) {
                    0 => cases::pick(rng, NAMES),
                    1 => cases::pick(rng, &["/", "\\", "/", "\\", ":", ".", " "]),
                    _ => cases::pick(rng, HOSTILE),
                };
                input.push_str(piece);
            }
            (style, input)
        },
        |(style, input)| {
            let style = *style;
            let sound = |names: Vec<&str>| -> cases::Outcome {
                for name in names {
                    ensure!(
                        !name.is_empty() && name != "." && name != "..",
                        "step {name:?}"
                    );
                    ensure!(!name.contains(['/', '\0']), "{name:?} is not one name");
                    if style == Windows {
                        ensure!(
                            lexical::check_name(name, Windows).is_ok(),
                            "{name:?} was let through"
                        );
                        ensure!(!name.contains('\\'), "{name:?} is not one name");
                    }
                }
                Ok(())
            };
            if let Ok(path) = AbsPath::parse(input, style) {
                sound(path.names().collect())?;
                if let Some((server, share)) = path.share() {
                    sound(vec![server, share])?;
                }
                ensure!(
                    AbsPath::parse(path.as_str(), style)?.as_str() == path.as_str(),
                    "not stable"
                );
            }
            if let Ok(path) = RelPath::parse(input, style) {
                sound(path.names().collect())?;
            }
            Ok(())
        },
    );
}

#[test]
fn property_folding_agrees_with_comparison() {
    cases::check(
        "path_fold",
        3000,
        |rng| {
            let name = |rng: &mut SeededRng| -> String {
                (0..1 + cases::small(rng, 5))
                    .map(|_| {
                        *cases::pick(
                            rng,
                            &[
                                "a", "A", "é", "É", "ß", "ı", "I", "i", "İ", "K", "\u{212a}", "k",
                                "日", "ω", "Ω", "1",
                            ],
                        )
                    })
                    .collect()
            };
            (name(rng), name(rng))
        },
        |(first, second)| {
            let same = Windows.same_name(first, second);
            ensure!(
                same == (Windows.fold(first) == Windows.fold(second)),
                "same_name and fold disagree"
            );
            let (a, b) = (
                RelPath::parse(first, Windows)?,
                RelPath::parse(second, Windows)?,
            );
            ensure!((a == b) == same, "RelPath equality disagrees");
            ensure!(Windows.same_name(first, first), "a name is itself");
            ensure!(
                Windows.fold(&Windows.fold(first)) == Windows.fold(first),
                "folding twice changes it"
            );
            // Folding never changes how many names there are or their order.
            ensure!(
                Posix.same_name(first, second) == (first == second),
                "POSIX is exact"
            );
            Ok(())
        },
    );
}

#[test]
fn a_project_key_is_128_bits_of_the_paths_hash() {
    let root = abs("/srv/dev/project", Posix);
    let key = root.project_key();
    assert_eq!(key.as_str().len(), 32);
    assert!(
        key.as_str()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{key}"
    );
    // Pinned: data is filed under the key, so it must never change for a
    // path. This is the start of the path's SHA-256.
    assert_eq!(key.as_str(), "43bdcbc5c0603ceb6f23b67b57b73863");
    assert_ne!(abs("/srv/dev/project2", Posix).project_key(), key);
    assert_ne!(abs("/srv/dev", Posix).project_key(), key);
}

#[test]
fn a_project_key_keeps_the_case_of_each_name_and_folds_the_root() {
    // The names are hashed as they are spelled: in a case-sensitive NTFS
    // directory `Project` and `PROJECT` are two folders, and must be two
    // projects. One folder reached under two casings still gets one key,
    // because the resolver spells it one way (`tests/it/paths_windows.rs`).
    let stored = abs(r"C:\Work\Project", Windows);
    assert_ne!(
        stored.project_key(),
        abs("C:/work/PROJECT", Windows).project_key()
    );
    assert_ne!(
        stored.project_key(),
        abs("C:/Work/project", Windows).project_key()
    );
    // A drive letter and a share's root have no spelling on disk.
    assert_eq!(
        stored.project_key(),
        abs("c:/Work/Project/", Windows).project_key()
    );
    assert_eq!(
        abs(r"\\Server\Share\Work", Windows).project_key(),
        abs("//SERVER/share/Work", Windows).project_key()
    );
    assert_ne!(
        abs(r"\\Server\Share\Work", Windows).project_key(),
        abs("//server/share/work", Windows).project_key()
    );
    // Pinned, like the POSIX key: data is filed under it. This is the start
    // of the SHA-256 of `C:/Work/Project`.
    let mut expected = crate::ContentHash::of(b"C:/Work/Project").to_hex();
    expected.truncate(32);
    assert_eq!(stored.project_key().as_str(), expected);
    // A POSIX path that differs only in case is another project.
    assert_ne!(
        abs("/work/project", Posix).project_key(),
        abs("/work/Project", Posix).project_key()
    );
}
