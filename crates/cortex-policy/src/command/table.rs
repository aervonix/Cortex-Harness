//! The built-in table of commands that only read
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Shell command
//! analysis", rule 4).
//!
//! A command on this table runs without asking, at every level, and is
//! scheduled as read-only. So the table is where a mistake would let a
//! command write or run something unasked, and it is built to fail closed:
//!
//! - **A command is on the table with its options, never as a whole.** Each
//!   entry lists the options that are safe, by their exact names. An option
//!   that is not listed takes the command off the table: `sort -o out`,
//!   `find -delete`, `git diff --output=x`, `rg --pre cmd`. So does an
//!   abbreviation of a long option, which GNU programs accept and this
//!   table does not.
//! - **Programs that are languages are not on it**, in any form but one:
//!   `awk`, `perl`, `python`, `xargs`, `env`, `tee`, `less`, `more` (whose
//!   interactive keys run commands). `sed` is on it only as
//!   `sed -n '<line>p'` or `'<from>,<to>p'`, which is what a model writes
//!   to print lines. Its script language can write files and run commands.
//! - **A word the shell would expand takes the command off the table.** A
//!   glob can expand to a name that begins with `-`, which the program
//!   then reads as an option.
//! - **No option that follows links is listed** (`grep -R`, `find -L`,
//!   `ls -L`, `tree -l`), and `diff` is not on the table: it reads through
//!   the links inside the directories it compares. The files a command
//!   names are checked one by one; what it finds by walking a tree must stay
//!   in the tree.
//! - **No search that reads the content of a whole tree is on it.** `grep
//!   -r` and `rg` read files nobody named, and what they leave out (hidden
//!   files, what an ignore file names) the workspace itself can change: a
//!   `.ignore` with `!.env` in it, or `rg -t sh`, whose type names `.env`.
//!   The `search` tool makes that search with every file put to the read
//!   rules. `grep` of named files is on the table. What remains is the
//!   tracked content that `git diff`, `git show`, `git log -p` and `git grep`
//!   print, stated in `docs/security.md`.
//! - **A value an option takes only attached is never the next word.** The
//!   table would skip that word as the value, and the program would read it
//!   as an option of its own: `git log --pretty --output=f` writes `f`. Each
//!   `SV` and `LV` entry has been checked against the real program, and a
//!   `git` line with a word holding `--out` is never on the table, whatever
//!   option comes before it.
//! - **No shell builtin that can run code is on it.** `printf` and `test`
//!   evaluate an array subscript in their arguments in bash, which runs a
//!   command substitution written there, quoted or not.
//! - **The program is named plainly.** `ls`, not `./ls` or `/tmp/ls`: a
//!   path names some other file.
//! - **`git` is on the table only in a trusted project.** A repository's
//!   own configuration can name programs that `git status` and `git diff`
//!   run (a file-system monitor, a filter, a text converter), and an
//!   untrusted repository can arrive with that configuration
//!   (`docs/security.md`, section 2).
//!
//! # What the caller still checks
//!
//! The table says what a command **names**: the files and directories among
//! its arguments ([`Named`]). `cat` is a read-only command, and `cat
//! ~/.ssh/id_rsa` is a read of a private key. The engine puts every named
//! path, and the directory the command runs in, to the read rules, and the
//! command is on the safe list only if each of those reads would be
//! allowed without asking. Redirections are the engine's too.

use super::powershell::command_name;
use super::{Part, ShellSyntax, Word};

/// What the value of an option, or an operand, is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Text the program does not open: a pattern, a number, a format.
    Text,
    /// A file or a directory the program reads.
    Path,
    /// A git revision or a path, or `revision:path`. Checked as a path
    /// where it can be read as one.
    RevisionOrPath,
}

/// One option a command may be given.
#[derive(Debug, Clone, Copy)]
enum Opt {
    /// `-x`. May share a `-` with others: `-la`.
    Short(char),
    /// `-x VALUE` or `-xVALUE`.
    ShortValue(char, Kind),
    /// `-x` or `-xVALUE`: the value only when attached.
    ShortOptional(char),
    /// `--name`.
    Long(&'static str),
    /// `--name=VALUE` or `--name VALUE`.
    LongValue(&'static str, Kind),
    /// `--name` or `--name=VALUE`.
    LongOptional(&'static str),
}

/// What a command may be given besides options.
#[derive(Debug, Clone, Copy)]
struct Operands {
    kind: Kind,
    /// The most there may be. `None`: any number.
    most: Option<usize>,
}

const NO_OPERANDS: Operands = Operands {
    kind: Kind::Text,
    most: Some(0),
};
const PATHS: Operands = Operands {
    kind: Kind::Path,
    most: None,
};
const TEXTS: Operands = Operands {
    kind: Kind::Text,
    most: None,
};
const REVISIONS_OR_PATHS: Operands = Operands {
    kind: Kind::RevisionOrPath,
    most: None,
};

/// The options and operands of a command in the usual Unix form.
#[derive(Debug, Clone, Copy)]
struct Spec {
    options: &'static [Opt],
    operands: Operands,
    /// Whether `-5` is a count, as for `head` and `tail`.
    count_option: bool,
    /// What the command reads beyond the files it names.
    searches: Searches,
}

/// What a command reads the content of, beyond the files it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Searches {
    /// Nothing: `cat`, `grep` of named files, and every command that
    /// prints names and not content (`ls -R`, `find`, `tree`).
    Nothing,
    /// The tracked files of the repository: `git diff`, `git grep`.
    Repository,
    /// The tracked files of the repository, when one of these options is
    /// given: `git log`, which prints content only with `-p` and the like.
    /// A short option is here by its letter.
    RepositoryWith(&'static [&'static str]),
}

/// The options that make `git log` print what files hold, or say whether
/// they hold a given text. Every other option on [`GIT_LOG`] prints commits
/// and names. An option added there is to be put on one side or the other.
const LOG_SHOWS_CONTENT: &[&str] = &[
    "p",
    "u",
    "U",
    "S",
    "G",
    "L",
    "patch",
    "unified",
    "word-diff",
    "pickaxe-all",
    "pickaxe-regex",
];

/// A parameter of a PowerShell command, by its full name in lower case.
#[derive(Debug, Clone, Copy)]
enum Param {
    /// `-Name`.
    Switch(&'static str),
    /// `-Name value`. (`-Name:value` is not modelled: PowerShell reads
    /// it as two words.)
    Value(&'static str, Kind),
}

/// The parameters and operands of a PowerShell command.
#[derive(Debug, Clone, Copy)]
struct PsSpec {
    params: &'static [Param],
    operands: Operands,
}

/// How a command's arguments are read.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Options and operands in the usual Unix form.
    Unix(Spec),
    /// Every argument is text the command prints: `echo`.
    AnyText,
    /// A first argument that selects one of these, each with its own shape.
    Subcommands(&'static [(&'static str, Shape)]),
    /// `find`: start points, then tests.
    Find,
    /// `sed -n '<lines>p' [file…]` and nothing else.
    SedPrint,
    /// `date`, with an optional `+format`.
    Date,
    /// A PowerShell command.
    PowerShell(PsSpec),
}

/// One row of the table.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// The names the command is called by.
    names: &'static [&'static str],
    shape: Shape,
    /// Whether the command is on the table only in a trusted project.
    needs_trust: bool,
}

/// A file or directory a read-only command names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// The path as the command has it: relative to the directory the
    /// command runs in, absolute, or under `~`.
    pub text: String,
    /// Whether it must be a path the harness accepts. `false` for a word
    /// that may be a git revision instead: if it cannot be read as a path,
    /// it is not one.
    pub must_resolve: bool,
}

/// What a command that only reads will read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reading {
    /// The files and directories it names.
    pub named: Vec<Named>,
    /// Whether it prints the content of the repository's tracked files,
    /// wherever in the repository it runs.
    pub repository: bool,
}

const fn unix(options: &'static [Opt], operands: Operands) -> Shape {
    Shape::Unix(Spec {
        options,
        operands,
        count_option: false,
        searches: Searches::Nothing,
    })
}

const fn counted(options: &'static [Opt], operands: Operands) -> Shape {
    Shape::Unix(Spec {
        options,
        operands,
        count_option: true,
        searches: Searches::Nothing,
    })
}

const fn searching(options: &'static [Opt], operands: Operands, searches: Searches) -> Shape {
    Shape::Unix(Spec {
        options,
        operands,
        count_option: false,
        searches,
    })
}

use Kind::{Path as P, Text as T};
use Opt::{
    Long as L, LongOptional as LO, LongValue as LV, Short as S, ShortOptional as SO,
    ShortValue as SV,
};

#[rustfmt::skip]
const LS: &[Opt] = &[
    S('a'), S('A'), S('l'), S('h'), S('R'), S('t'), S('r'), S('S'), S('1'), S('d'), S('F'),
    S('G'), S('g'), S('o'), S('i'), S('s'), S('p'), S('n'), S('C'), S('x'), S('m'), S('q'),
    S('u'), S('c'), S('U'), S('X'), S('v'), S('B'), S('k'), S('Z'),
    L("all"), L("almost-all"), L("human-readable"), L("recursive"), L("reverse"),
    L("directory"), L("classify"), L("no-group"), L("group-directories-first"), L("si"),
    L("size"), L("inode"), L("numeric-uid-gid"), L("full-time"),
    L("literal"), L("escape"), L("author"),
    LO("color"), LV("time", T), LV("sort", T), LV("format", T), LV("time-style", T),
    LV("block-size", T), LV("hide", T), LV("ignore", T), SV('I', T), SV('w', T), LV("width", T),
];

#[rustfmt::skip]
const CAT: &[Opt] = &[
    S('n'), S('b'), S('s'), S('A'), S('E'), S('T'), S('v'), S('e'), S('t'), S('u'),
    L("number"), L("number-nonblank"), L("squeeze-blank"), L("show-all"), L("show-ends"),
    L("show-tabs"), L("show-nonprinting"),
];

#[rustfmt::skip]
const HEAD: &[Opt] = &[
    SV('n', T), SV('c', T), S('q'), S('v'), S('z'),
    LV("lines", T), LV("bytes", T), L("quiet"), L("silent"), L("verbose"), L("zero-terminated"),
];

#[rustfmt::skip]
const WC: &[Opt] = &[
    S('l'), S('w'), S('c'), S('m'), S('L'),
    L("lines"), L("words"), L("bytes"), L("chars"), L("max-line-length"),
];

#[rustfmt::skip]
/// Not here: `-r` and `--recursive`. A search of a whole tree reads every
/// file in it, a `.env` or a file a rule protects included, and none of
/// those is named on the command line to be checked. The `search` tool
/// does that search with the gate asked about each file.
const GREP: &[Opt] = &[
    S('n'), S('i'), S('l'), S('L'), S('c'), S('v'), S('w'), S('x'), S('H'),
    S('h'), S('o'), S('q'), S('s'), S('E'), S('F'), S('P'), S('G'), S('a'), S('I'), S('z'),
    S('y'), S('b'), S('T'), S('U'),
    SV('A', T), SV('B', T), SV('C', T), SV('m', T), SV('e', T),
    L("line-number"), L("ignore-case"),
    L("no-ignore-case"), L("files-with-matches"), L("files-without-match"), L("count"),
    L("invert-match"), L("word-regexp"), L("line-regexp"), L("with-filename"),
    L("no-filename"), L("only-matching"), L("quiet"), L("silent"), L("no-messages"),
    L("extended-regexp"), L("fixed-strings"), L("perl-regexp"), L("basic-regexp"), L("text"),
    L("null"), L("null-data"), L("byte-offset"), L("initial-tab"), L("binary"),
    LO("color"), LO("colour"),
    LV("after-context", T), LV("before-context", T), LV("context", T), LV("max-count", T),
    LV("regexp", T), LV("include", T), LV("exclude", T), LV("exclude-dir", T),
    LV("binary-files", T), LV("label", T),
];

#[rustfmt::skip]
const FD: &[Opt] = &[
    S('H'), S('I'), S('u'), S('s'), S('i'), S('g'), S('F'), S('a'), S('p'), S('0'),
    S('1'), S('q'),
    SV('t', T), SV('e', T), SV('d', T), SV('E', T), SV('S', T), SV('c', T), SV('j', T),
    L("hidden"), L("no-ignore"), L("no-ignore-vcs"), L("unrestricted"), L("case-sensitive"),
    L("ignore-case"), L("glob"), L("regex"), L("fixed-strings"), L("absolute-path"),
    L("full-path"), L("print0"), L("quiet"), L("prune"), L("relative-path"),
    L("show-errors"), L("one-file-system"),
    LV("type", T), LV("extension", T), LV("max-depth", T), LV("min-depth", T),
    LV("exact-depth", T), LV("exclude", T), LV("size", T), LV("color", T), LV("threads", T),
    LV("changed-within", T), LV("changed-before", T), LV("max-results", T),
    LV("search-path", P), LV("base-directory", P), LV("path-separator", T),
];

#[rustfmt::skip]
const SORT: &[Opt] = &[
    S('r'), S('n'), S('u'), S('f'), S('b'), S('h'), S('V'), S('g'), S('M'), S('c'), S('C'),
    S('s'), S('z'), S('d'), S('i'), S('R'),
    SV('k', T), SV('t', T),
    L("reverse"), L("numeric-sort"), L("unique"), L("ignore-case"), L("ignore-leading-blanks"),
    L("human-numeric-sort"), L("version-sort"), L("general-numeric-sort"), L("month-sort"),
    L("stable"), L("zero-terminated"), L("dictionary-order"), L("ignore-nonprinting"),
    L("random-sort"), LO("check"), LV("key", T), LV("field-separator", T),
];

#[rustfmt::skip]
const UNIQ: &[Opt] = &[
    S('c'), S('d'), S('D'), S('u'), S('i'), S('z'),
    SV('f', T), SV('s', T), SV('w', T),
    L("count"), L("repeated"), L("unique"), L("ignore-case"), L("zero-terminated"),
    LV("skip-fields", T), LV("skip-chars", T), LV("check-chars", T),
];

#[rustfmt::skip]
const STAT: &[Opt] = &[
    S('L'), S('f'), S('t'), SV('c', T),
    L("dereference"), L("file-system"), L("terse"), LV("format", T), LV("printf", T),
];

#[rustfmt::skip]
const FILE: &[Opt] = &[
    S('b'), S('i'), S('L'), S('h'), S('k'), S('p'), S('r'), S('N'), S('n'), S('0'),
    L("brief"), L("mime"), L("mime-type"), L("mime-encoding"), L("dereference"),
    L("no-dereference"), L("keep-going"), L("preserve-date"), L("raw"), L("no-pad"),
    L("no-buffer"), L("print0"), L("extension"),
];

#[rustfmt::skip]
const DU: &[Opt] = &[
    S('s'), S('h'), S('a'), S('c'), S('k'), S('m'), S('b'), S('x'), S('P'),
    S('S'), S('0'), S('l'), SV('d', T),
    L("summarize"), L("human-readable"), L("all"), L("total"), L("apparent-size"),
    L("one-file-system"), L("separate-dirs"), L("si"), L("bytes"),
    L("count-links"), L("null"), LV("max-depth", T), LV("block-size", T), LV("exclude", T),
    LV("threshold", T), LO("time"), LV("time-style", T),
];

#[rustfmt::skip]
const DF: &[Opt] = &[
    S('h'), S('H'), S('k'), S('m'), S('T'), S('i'), S('a'), S('l'), S('P'),
    L("human-readable"), L("si"), L("print-type"), L("inodes"), L("all"), L("local"),
    L("portability"), L("total"), LV("block-size", T), LV("type", T), LV("exclude-type", T),
    LO("output"),
];

#[rustfmt::skip]
const TREE: &[Opt] = &[
    S('a'), S('d'), S('f'), S('i'), S('s'), S('h'), S('C'), S('n'), S('p'), S('u'),
    S('g'), S('D'), S('F'), S('q'), S('N'), S('r'), S('t'), S('v'), S('x'), S('A'), S('S'),
    SV('L', T), SV('I', T), SV('P', T),
    L("gitignore"), L("dirsfirst"), L("filesfirst"), L("noreport"), L("prune"), L("du"),
    L("si"), L("inodes"), L("device"), L("matchdirs"), L("ignore-case"),
    LV("filelimit", T), LV("sort", T), LV("charset", T), LV("timefmt", T),
];

#[rustfmt::skip]
const READLINK: &[Opt] = &[
    S('f'), S('e'), S('m'), S('n'), S('q'), S('s'), S('v'), S('z'),
    L("canonicalize"), L("canonicalize-existing"), L("canonicalize-missing"),
    L("no-newline"), L("quiet"), L("silent"), L("verbose"), L("zero"),
];

#[rustfmt::skip]
const REALPATH: &[Opt] = &[
    S('e'), S('m'), S('L'), S('P'), S('q'), S('s'), S('z'),
    L("canonicalize-existing"), L("canonicalize-missing"), L("logical"), L("physical"),
    L("quiet"), L("strip"), L("no-symlinks"), L("zero"),
    LV("relative-to", P), LV("relative-base", P),
];

#[rustfmt::skip]
const ID: &[Opt] = &[
    S('u'), S('g'), S('G'), S('n'), S('r'), S('z'),
    L("user"), L("group"), L("groups"), L("name"), L("real"), L("zero"),
];

#[rustfmt::skip]
const UNAME: &[Opt] = &[
    S('a'), S('s'), S('n'), S('r'), S('v'), S('m'), S('p'), S('i'), S('o'),
    L("all"), L("kernel-name"), L("nodename"), L("kernel-release"), L("kernel-version"),
    L("machine"), L("processor"), L("hardware-platform"), L("operating-system"),
];

#[rustfmt::skip]
const HOSTNAME: &[Opt] = &[
    S('f'), S('s'), S('d'), S('i'), S('I'), S('A'),
    L("fqdn"), L("long"), L("short"), L("domain"), L("ip-address"), L("all-ip-addresses"),
    L("all-fqdns"),
];

const WHICH: &[Opt] = &[S('a'), L("all")];
const NAME_PARTS: &[Opt] = &[
    S('a'),
    S('z'),
    SV('s', T),
    L("multiple"),
    L("zero"),
    LV("suffix", T),
];
const PWD: &[Opt] = &[S('L'), S('P'), L("logical"), L("physical")];
const NONE: &[Opt] = &[];
const VERSION_ONLY: &[Opt] = &[L("version"), L("help")];
// ------------------------------------------------------------------- git

#[rustfmt::skip]
const GIT_STATUS: &[Opt] = &[
    S('s'), S('b'), S('z'), S('v'), SO('u'),
    L("short"), L("branch"), L("long"), L("show-stash"), L("ahead-behind"),
    L("no-ahead-behind"), L("renames"), L("no-renames"), L("verbose"), L("null"),
    L("no-column"), LO("porcelain"), LO("untracked-files"), LO("ignored"),
    LO("ignore-submodules"),
];

/// The options of `git diff`, `git show` and `git log` that choose what is
/// shown. Not here, on purpose: `--output` (writes a file), `--ext-diff`
/// and `--textconv` (run a program the repository's configuration names;
/// their `--no-` forms are listed), `-O<file>`, and `--no-index`, which
/// compares any two paths and so prints ignored and hidden files, a `.env`
/// included.
///
/// **An option that takes its value only attached is `SO` or `LO`**, never
/// `SV` or `LV`: the table would skip the next word as the value, and git
/// would read it as an option of its own (`git diff -U --output=f`). Checked
/// against git 2.53 by putting `--output=<file>` after each option that
/// takes a value: `-U`, `--unified` and `--relative` take theirs only
/// attached, and every `SV` and `LV` here takes the next word.
#[rustfmt::skip]
const GIT_DIFF: &[Opt] = &[
    S('p'), S('u'), S('s'), S('w'), S('b'), S('z'), S('R'), S('a'),
    SO('U'), SO('M'), SO('C'), SV('S', T), SV('G', T),
    L("patch"), L("no-patch"), L("stat"), L("shortstat"), L("numstat"), L("summary"),
    L("name-only"), L("name-status"), L("cached"), L("staged"), L("merge-base"), L("check"),
    L("no-color"), L("no-ext-diff"), L("no-textconv"), L("no-renames"),
    L("ignore-all-space"), L("ignore-space-change"), L("ignore-space-at-eol"),
    L("ignore-blank-lines"), L("ignore-cr-at-eol"), L("minimal"), L("patience"),
    L("histogram"), L("text"), L("raw"), L("patch-with-stat"), L("compact-summary"),
    L("full-index"), L("binary"), L("exit-code"), L("quiet"),
    L("function-context"), L("pickaxe-all"), L("pickaxe-regex"), L("no-prefix"),
    LO("color"), LO("word-diff"), LO("color-words"), LO("color-moved"), LO("abbrev"),
    LO("find-renames"), LO("find-copies"), LO("dirstat"), LO("submodule"), LO("unified"),
    LO("relative"),
    LV("diff-filter", T), LV("diff-algorithm", T), LV("inter-hunk-context", T),
    LV("word-diff-regex", T), LV("src-prefix", T), LV("dst-prefix", T),
    LV("line-prefix", T), LV("ignore-matching-lines", T),
];

/// The options of `git log` and `git show`. As for [`GIT_DIFF`], checked
/// against git 2.53: `-U`, `--unified`, `--pretty`, `--format`,
/// `--min-parents` and `--max-parents` take their value only attached
/// (`--pretty` and `--format` are read by hand in git's `revision.c`).
#[rustfmt::skip]
const GIT_LOG: &[Opt] = &[
    S('p'), S('u'), S('s'), S('w'), S('b'), S('z'), S('i'), S('E'), S('F'),
    SV('n', T), SO('U'), SV('S', T), SV('G', T), SV('L', T), SO('M'), SO('C'),
    L("oneline"), L("graph"), L("all"), L("branches"), L("tags"), L("remotes"),
    L("patch"), L("no-patch"), L("stat"), L("shortstat"), L("numstat"), L("summary"),
    L("name-only"), L("name-status"), L("no-color"), L("no-ext-diff"), L("no-textconv"),
    L("no-renames"), L("follow"), L("merges"), L("no-merges"), L("first-parent"),
    L("reverse"), L("abbrev-commit"), L("no-abbrev-commit"), L("no-decorate"),
    L("source"), L("left-right"), L("cherry-pick"), L("cherry-mark"), L("topo-order"),
    L("date-order"), L("author-date-order"), L("full-history"), L("simplify-merges"),
    L("ancestry-path"), L("boundary"), L("raw"), L("show-signature"), L("no-walk"),
    L("ignore-all-space"), L("ignore-space-change"), L("regexp-ignore-case"),
    L("extended-regexp"), L("fixed-strings"), L("all-match"), L("invert-grep"),
    L("relative-date"), L("parents"), L("children"), L("pickaxe-all"), L("pickaxe-regex"),
    LO("decorate"), LO("color"), LO("abbrev"), LO("word-diff"), LO("find-renames"),
    LO("show-notes"), LO("notes"), LO("format"), LO("pretty"), LO("unified"),
    LO("min-parents"), LO("max-parents"),
    LV("since", T), LV("until", T), LV("after", T),
    LV("before", T), LV("author", T), LV("committer", T), LV("grep", T), LV("date", T),
    LV("max-count", T), LV("skip", T), LV("diff-filter", T), LV("decorate-refs", T),
    LV("decorate-refs-exclude", T), LV("diff-algorithm", T),
];

/// `git branch` only lists: with a name it would create a branch, so it
/// takes no operand, and none of the options that delete, move or copy.
#[rustfmt::skip]
const GIT_BRANCH: &[Opt] = &[
    S('a'), S('r'), S('v'), S('l'),
    L("all"), L("remotes"), L("verbose"), L("list"), L("show-current"), L("no-color"),
    L("no-column"), L("ignore-case"), LO("color"), LO("abbrev"), LO("column"),
    LV("contains", T), LV("no-contains", T), LV("merged", T), LV("no-merged", T),
    LV("points-at", T), LV("sort", T), LV("format", T),
];

#[rustfmt::skip]
const GIT_TAG: &[Opt] = &[
    S('l'), SO('n'), L("list"), L("no-column"), L("ignore-case"), LO("column"),
    LV("contains", T), LV("no-contains", T), LV("merged", T), LV("no-merged", T),
    LV("points-at", T), LV("sort", T), LV("format", T),
];

#[rustfmt::skip]
const GIT_REV_PARSE: &[Opt] = &[
    L("show-toplevel"), L("show-prefix"), L("show-cdup"), L("git-dir"), L("git-common-dir"),
    L("absolute-git-dir"), L("is-inside-work-tree"), L("is-inside-git-dir"),
    L("is-bare-repository"), L("is-shallow-repository"), L("verify"), L("quiet"),
    L("symbolic"), L("symbolic-full-name"), L("all"), L("branches"), L("tags"),
    L("remotes"), L("show-superproject-working-tree"), LO("abbrev-ref"), LO("short"),
];

#[rustfmt::skip]
const GIT_LS_FILES: &[Opt] = &[
    S('c'), S('d'), S('m'), S('o'), S('s'), S('u'), S('k'), S('i'), S('z'), S('t'), S('v'),
    SV('x', T),
    L("cached"), L("deleted"), L("modified"), L("others"), L("stage"), L("unmerged"),
    L("killed"), L("ignored"), L("exclude-standard"), L("full-name"), L("directory"),
    L("no-empty-directory"), L("error-unmatch"), L("deduplicate"), L("eol"),
    LV("exclude", T), LO("abbrev"),
];

#[rustfmt::skip]
const GIT_BLAME: &[Opt] = &[
    S('w'), S('s'), S('e'), S('l'), S('t'), S('f'), S('n'), S('c'), S('b'), S('k'),
    SV('L', T), SO('M'), SO('C'),
    L("show-email"), L("show-name"), L("show-number"), L("root"), L("porcelain"),
    L("line-porcelain"), L("incremental"), L("minimal"), L("first-parent"),
    L("no-color"), LO("abbrev"), LO("color-lines"), LO("color-by-age"), LV("date", T),
];

#[rustfmt::skip]
const GIT_DESCRIBE: &[Opt] = &[
    L("tags"), L("all"), L("always"), L("long"), L("contains"), L("exact-match"),
    L("first-parent"), LO("dirty"), LO("abbrev"), LV("match", T), LV("exclude", T),
    LV("candidates", T),
];

/// The options of `git grep`. Not here: `-O` and `--open-files-in-pager`,
/// which start a program, `-f`, and `--untracked` and `--no-index`, which
/// search files git does not track, ignored and hidden ones included.
#[rustfmt::skip]
const GIT_GREP: &[Opt] = &[
    S('n'), S('i'), S('l'), S('L'), S('w'), S('c'), S('v'), S('h'), S('H'), S('E'), S('F'),
    S('G'), S('P'), S('I'), S('a'), S('o'), S('q'), S('z'), S('p'), S('W'),
    SV('e', T), SV('A', T), SV('B', T), SV('C', T), SV('m', T),
    L("line-number"), L("ignore-case"), L("files-with-matches"), L("name-only"),
    L("files-without-match"), L("word-regexp"), L("count"), L("invert-match"),
    L("extended-regexp"), L("fixed-strings"), L("basic-regexp"), L("perl-regexp"),
    L("text"), L("only-matching"), L("quiet"), L("null"), L("show-function"),
    L("function-context"), L("cached"), L("no-color"),
    L("full-name"), L("column"), L("heading"), L("break"), L("all-match"), L("and"),
    L("or"), L("not"), L("no-textconv"), L("recursive"), L("no-recursive"),
    LO("color"), LV("max-depth", T), LV("max-count", T), LV("threads", T),
    LV("after-context", T), LV("before-context", T), LV("context", T),
];

const GIT_REMOTE: &[Opt] = &[S('v'), L("verbose")];
const GIT_STASH_LIST: &[Opt] = &[
    L("oneline"),
    L("stat"),
    SV('n', T),
    LO("format"),
    LO("pretty"),
];

#[rustfmt::skip]
const GIT: &[(&str, Shape)] = &[
    ("status", unix(GIT_STATUS, PATHS)),
    ("diff", searching(GIT_DIFF, REVISIONS_OR_PATHS, Searches::Repository)),
    ("show", searching(GIT_LOG, REVISIONS_OR_PATHS, Searches::Repository)),
    ("log", Shape::Unix(Spec {
        options: GIT_LOG,
        operands: REVISIONS_OR_PATHS,
        count_option: true,
        searches: Searches::RepositoryWith(LOG_SHOWS_CONTENT),
    })),
    ("branch", unix(GIT_BRANCH, NO_OPERANDS)),
    ("tag", unix(GIT_TAG, NO_OPERANDS)),
    ("rev-parse", unix(GIT_REV_PARSE, TEXTS)),
    ("ls-files", unix(GIT_LS_FILES, PATHS)),
    ("blame", unix(GIT_BLAME, REVISIONS_OR_PATHS)),
    ("describe", unix(GIT_DESCRIBE, TEXTS)),
    ("grep", searching(GIT_GREP, REVISIONS_OR_PATHS, Searches::Repository)),
    ("remote", unix(GIT_REMOTE, NO_OPERANDS)),
    ("stash", Shape::Subcommands(&[("list", unix(GIT_STASH_LIST, NO_OPERANDS))])),
    ("version", unix(NONE, NO_OPERANDS)),
    ("--version", unix(NONE, NO_OPERANDS)),
];

// ------------------------------------------------------------ PowerShell

const fn ps(params: &'static [Param], operands: Operands) -> Shape {
    Shape::PowerShell(PsSpec { params, operands })
}

use Param::{Switch as PS_SWITCH, Value as PS_VALUE};

#[rustfmt::skip]
const PS_CHILD_ITEM: &[Param] = &[
    PS_VALUE("path", P), PS_VALUE("literalpath", P), PS_VALUE("filter", T),
    PS_VALUE("include", T), PS_VALUE("exclude", T), PS_VALUE("depth", T),
    PS_VALUE("attributes", T), PS_SWITCH("recurse"), PS_SWITCH("force"), PS_SWITCH("name"),
    PS_SWITCH("file"), PS_SWITCH("directory"), PS_SWITCH("hidden"), PS_SWITCH("readonly"),
    PS_SWITCH("system"),
];

/// Not here: `-Wait`, which never ends, and `-Stream`, which reads an
/// alternate data stream.
#[rustfmt::skip]
const PS_CONTENT: &[Param] = &[
    PS_VALUE("path", P), PS_VALUE("literalpath", P), PS_VALUE("totalcount", T),
    PS_VALUE("head", T), PS_VALUE("first", T), PS_VALUE("tail", T), PS_VALUE("last", T),
    PS_VALUE("readcount", T), PS_VALUE("encoding", T), PS_VALUE("delimiter", T),
    PS_SWITCH("raw"), PS_SWITCH("force"),
];

#[rustfmt::skip]
const PS_SELECT_STRING: &[Param] = &[
    PS_VALUE("pattern", T), PS_VALUE("path", P), PS_VALUE("literalpath", P),
    PS_VALUE("context", T), PS_VALUE("encoding", T), PS_VALUE("include", T),
    PS_VALUE("exclude", T), PS_SWITCH("simplematch"), PS_SWITCH("casesensitive"),
    PS_SWITCH("list"), PS_SWITCH("quiet"), PS_SWITCH("notmatch"), PS_SWITCH("allmatches"),
    PS_SWITCH("raw"), PS_SWITCH("noemphasis"),
];

#[rustfmt::skip]
const PS_TEST_PATH: &[Param] = &[
    PS_VALUE("path", P), PS_VALUE("literalpath", P), PS_VALUE("pathtype", T),
    PS_VALUE("filter", T), PS_VALUE("include", T), PS_VALUE("exclude", T),
    PS_SWITCH("isvalid"),
];

#[rustfmt::skip]
const PS_RESOLVE_PATH: &[Param] = &[
    PS_VALUE("path", P), PS_VALUE("literalpath", P), PS_SWITCH("relative"),
];

#[rustfmt::skip]
const PS_ITEM: &[Param] = &[
    PS_VALUE("path", P), PS_VALUE("literalpath", P), PS_VALUE("filter", T),
    PS_VALUE("include", T), PS_VALUE("exclude", T), PS_SWITCH("force"),
];

const PS_LOCATION: &[Param] = &[];

#[rustfmt::skip]
const PS_DATE: &[Param] = &[
    PS_VALUE("format", T), PS_VALUE("uformat", T), PS_VALUE("date", T), PS_SWITCH("asutc"),
];

const PS_OUTPUT: &[Param] = &[PS_VALUE("inputobject", T), PS_SWITCH("noenumerate")];

#[rustfmt::skip]
const PS_HOST: &[Param] = &[
    PS_VALUE("object", T), PS_VALUE("foregroundcolor", T), PS_VALUE("backgroundcolor", T),
    PS_VALUE("separator", T), PS_SWITCH("nonewline"),
];

#[rustfmt::skip]
const PS_FILE_HASH: &[Param] = &[
    PS_VALUE("path", P), PS_VALUE("literalpath", P), PS_VALUE("algorithm", T),
];

// ------------------------------------------------------------- the table

/// The commands of a POSIX shell.
#[rustfmt::skip]
const POSIX: &[Entry] = &[
    row(&["ls"], unix(LS, PATHS)),
    row(&["cat"], unix(CAT, PATHS)),
    row(&["head"], counted(HEAD, PATHS)),
    // `tail -f` never ends, so it is not listed.
    row(&["tail"], counted(HEAD, PATHS)),
    row(&["wc"], unix(WC, PATHS)),
    row(&["grep", "egrep", "fgrep"], unix(GREP, PATHS)),
    row(&["fd", "fdfind"], unix(FD, PATHS)),
    row(&["find"], Shape::Find),
    row(&["sort"], unix(SORT, PATHS)),
    // A second operand of `uniq` is a file it writes.
    row(&["uniq"], unix(UNIQ, Operands { kind: Kind::Path, most: Some(1) })),
    row(&["stat"], unix(STAT, PATHS)),
    row(&["file"], unix(FILE, PATHS)),
    row(&["du"], unix(DU, PATHS)),
    row(&["df"], unix(DF, PATHS)),
    row(&["tree"], unix(TREE, PATHS)),
    row(&["readlink"], unix(READLINK, PATHS)),
    row(&["realpath"], unix(REALPATH, PATHS)),
    row(&["basename", "dirname"], unix(NAME_PARTS, TEXTS)),
    row(&["pwd"], unix(PWD, NO_OPERANDS)),
    row(&["whoami"], unix(VERSION_ONLY, NO_OPERANDS)),
    row(&["id"], unix(ID, Operands { kind: Kind::Text, most: Some(1) })),
    row(&["uname"], unix(UNAME, NO_OPERANDS)),
    // With an operand `hostname` sets the name.
    row(&["hostname"], unix(HOSTNAME, NO_OPERANDS)),
    row(&["which"], unix(WHICH, TEXTS)),
    row(&["true", "false"], unix(NONE, TEXTS)),
    // Not `printf` or `test`: in bash both evaluate an array subscript in
    // their arguments (`printf -v 'a[$(cmd)]'`, `test -v 'a[$(cmd)]'`),
    // and that runs the command, quoted or not.
    row(&["echo"], Shape::AnyText),
    row(&["date"], Shape::Date),
    row(&["sed"], Shape::SedPrint),
    Entry { names: &["git"], shape: Shape::Subcommands(GIT), needs_trust: true },
];

/// The commands of PowerShell: its own, by name and by alias, and the few
/// programs whose names no alias takes.
#[rustfmt::skip]
const POWERSHELL: &[Entry] = &[
    row(&["get-childitem", "gci", "ls", "dir"], ps(PS_CHILD_ITEM, PATHS)),
    row(&["get-content", "gc", "cat", "type"], ps(PS_CONTENT, PATHS)),
    row(&["select-string", "sls"], ps(PS_SELECT_STRING, PATHS)),
    row(&["test-path"], ps(PS_TEST_PATH, PATHS)),
    row(&["resolve-path", "rvpa"], ps(PS_RESOLVE_PATH, PATHS)),
    row(&["get-item", "gi"], ps(PS_ITEM, PATHS)),
    row(&["get-location", "gl", "pwd"], ps(PS_LOCATION, NO_OPERANDS)),
    row(&["get-date"], ps(PS_DATE, NO_OPERANDS)),
    row(&["write-output", "echo", "write"], ps(PS_OUTPUT, TEXTS)),
    row(&["write-host"], ps(PS_HOST, TEXTS)),
    row(&["get-filehash"], ps(PS_FILE_HASH, PATHS)),
    row(&["whoami"], unix(NONE, NO_OPERANDS)),
    row(&["hostname"], unix(NONE, NO_OPERANDS)),
    row(&["fd"], unix(FD, PATHS)),
    Entry { names: &["git"], shape: Shape::Subcommands(GIT), needs_trust: true },
];

const fn row(names: &'static [&'static str], shape: Shape) -> Entry {
    Entry {
        names,
        shape,
        needs_trust: false,
    }
}

/// The names on the table for a shell, for the documentation and for
/// `cortex permissions`.
#[must_use]
pub fn names(syntax: ShellSyntax) -> Vec<&'static str> {
    let entries = match syntax {
        ShellSyntax::Posix => POSIX,
        ShellSyntax::PowerShell => POWERSHELL,
        ShellSyntax::Cmd | ShellSyntax::Unknown => &[],
    };
    entries
        .iter()
        .flat_map(|entry| entry.names.iter().copied())
        .collect()
}

/// Whether a simple command is on the table of commands that only read,
/// and if it is, the files and directories it names.
///
/// `None` means the command is not known to only read. It may still be on
/// another safe list, or be asked about.
///
/// Redirections are not looked at: a command that is on the table can
/// still write through `>`, which the caller checks.
#[must_use]
pub fn read_only(part: &Part, syntax: ShellSyntax, trusted: bool) -> Option<Reading> {
    let program = part.words.first()?;
    if !program.is_fixed() || program.home {
        return None;
    }
    let (entries, name) = match syntax {
        ShellSyntax::Posix => (POSIX, program.text.clone()),
        ShellSyntax::PowerShell => {
            // A path names some other file than the one a bare name finds.
            if program.text.contains(['/', '\\']) {
                return None;
            }
            (POWERSHELL, command_name(&program.text))
        }
        ShellSyntax::Cmd | ShellSyntax::Unknown => return None,
    };
    let entry = entries
        .iter()
        .find(|entry| entry.names.contains(&name.as_str()))?;
    if entry.needs_trust && !trusted {
        return None;
    }
    // `git diff`, `git log` and `git show` write any file with `--output`,
    // and the table's reading of what comes before it could be wrong: one
    // option misread as taking the next word is enough to hide it. So a git
    // line with `--out` anywhere in a word is off the table, whatever the
    // options around it.
    if entry.names.contains(&"git")
        && part
            .arguments()
            .iter()
            .any(|word| word.text.contains("--out"))
    {
        return None;
    }
    let mut reading = Reading::default();
    shape_allows(&entry.shape, part.arguments(), &mut reading).then_some(reading)
}

fn shape_allows(shape: &Shape, arguments: &[Word], reading: &mut Reading) -> bool {
    if arguments.iter().any(|word| !word.is_fixed()) {
        return false;
    }
    let named = &mut reading.named;
    match shape {
        Shape::Unix(spec) => unix_allows(spec, arguments, reading),
        Shape::AnyText => true,
        Shape::Subcommands(subcommands) => {
            let Some((first, rest)) = arguments.split_first() else {
                return false;
            };
            subcommands
                .iter()
                .find(|(name, _)| *name == first.text)
                .is_some_and(|(_, shape)| shape_allows(shape, rest, reading))
        }
        Shape::Find => find_allows(arguments, named),
        Shape::SedPrint => sed_allows(arguments, named),
        Shape::Date => date_allows(arguments),
        Shape::PowerShell(spec) => powershell_allows(spec, arguments, named),
    }
}

fn record(kind: Kind, text: &str, named: &mut Vec<Named>) {
    match kind {
        Kind::Text => {}
        Kind::Path => named.push(Named {
            text: text.to_owned(),
            must_resolve: true,
        }),
        Kind::RevisionOrPath => {
            named.push(Named {
                text: text.to_owned(),
                must_resolve: false,
            });
            // `HEAD:src/main.rs` shows the file `src/main.rs`.
            if let Some((_, path)) = text.rsplit_once(':')
                && !path.is_empty()
            {
                named.push(Named {
                    text: path.to_owned(),
                    must_resolve: false,
                });
            }
        }
    }
}

fn unix_allows(spec: &Spec, arguments: &[Word], reading: &mut Reading) -> bool {
    let named = &mut reading.named;
    // Whether an option was given that makes the command print content.
    let mut content = false;
    let shows_content = |name: &str| matches!(spec.searches, Searches::RepositoryWith(names) if names.contains(&name));
    let mut operands: Vec<&Word> = Vec::new();
    let mut only_operands = false;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let text = word.text.as_str();
        // Quoting does not hide an option from the program, so `'-o'` is
        // read as `-o`. A word for the home directory is never an option.
        if only_operands || word.home || text == "-" || !text.starts_with('-') {
            operands.push(word);
            continue;
        }
        if text == "--" {
            only_operands = true;
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let found = spec.options.iter().find(|option| {
                matches!(option, Opt::Long(known) | Opt::LongValue(known, _) | Opt::LongOptional(known)
                    if *known == name)
            });
            content |= shows_content(name);
            match (found, attached) {
                (Some(Opt::Long(_) | Opt::LongOptional(_)), None)
                | (Some(Opt::LongOptional(_)), Some(_)) => {}
                (Some(Opt::LongValue(_, kind)), Some(value)) => record(*kind, value, named),
                (Some(Opt::LongValue(_, kind)), None) => {
                    let Some(value) = words.next() else {
                        return false;
                    };
                    record(*kind, &value.text, named);
                }
                _ => return false,
            }
            continue;
        }
        let cluster = text.get(1..).unwrap_or("");
        if spec.count_option && cluster.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let letters = cluster.char_indices();
        for (at, letter) in letters {
            content |= shows_content(letter.encode_utf8(&mut [0; 4]));
            let found = spec.options.iter().find(|option| {
                matches!(option, Opt::Short(known) | Opt::ShortValue(known, _) | Opt::ShortOptional(known)
                    if *known == letter)
            });
            match found {
                Some(Opt::Short(_)) => {}
                Some(Opt::ShortOptional(_)) => break,
                Some(Opt::ShortValue(_, kind)) => {
                    let rest = cluster.get(at + letter.len_utf8()..).unwrap_or("");
                    if rest.is_empty() {
                        let Some(value) = words.next() else {
                            return false;
                        };
                        record(*kind, &value.text, named);
                    } else {
                        record(*kind, rest, named);
                    }
                    break;
                }
                _ => return false,
            }
        }
    }
    if spec.operands.most.is_some_and(|most| operands.len() > most) {
        return false;
    }
    for operand in &operands {
        if operand.text != "-" {
            record(spec.operands.kind, &operand.text, named);
        }
    }
    match spec.searches {
        Searches::Nothing => {}
        Searches::Repository => reading.repository = true,
        Searches::RepositoryWith(_) => reading.repository = content,
    }
    true
}

/// The tests and actions of `find` that only look, with whether each takes
/// a value and what the value is. Not here: `-exec`, `-execdir`, `-ok`,
/// `-okdir` (run a program), `-delete`, and `-fprint`, `-fprint0`,
/// `-fprintf`, `-fls` (write a file).
#[rustfmt::skip]
const FIND_TESTS: &[(&str, Option<Kind>)] = &[
    ("-name", Some(T)), ("-iname", Some(T)), ("-path", Some(T)), ("-ipath", Some(T)),
    ("-wholename", Some(T)), ("-iwholename", Some(T)), ("-regex", Some(T)),
    ("-iregex", Some(T)), ("-regextype", Some(T)), ("-type", Some(T)), ("-xtype", Some(T)),
    ("-maxdepth", Some(T)), ("-mindepth", Some(T)), ("-mtime", Some(T)), ("-mmin", Some(T)),
    ("-atime", Some(T)), ("-amin", Some(T)), ("-ctime", Some(T)), ("-cmin", Some(T)),
    ("-size", Some(T)), ("-perm", Some(T)), ("-user", Some(T)), ("-group", Some(T)),
    ("-uid", Some(T)), ("-gid", Some(T)), ("-links", Some(T)), ("-inum", Some(T)),
    ("-newer", Some(P)), ("-anewer", Some(P)), ("-cnewer", Some(P)), ("-samefile", Some(P)),
    ("-printf", Some(T)),
    ("-empty", None), ("-readable", None), ("-writable", None), ("-executable", None),
    ("-print", None), ("-print0", None), ("-ls", None), ("-prune", None), ("-depth", None),
    ("-xdev", None), ("-mount", None), ("-daystart", None),
    ("-noleaf", None), ("-true", None), ("-false", None), ("-nouser", None),
    ("-nogroup", None), ("-quit", None), ("-not", None), ("-a", None), ("-and", None),
    ("-o", None), ("-or", None),
];

fn find_allows(arguments: &[Word], named: &mut Vec<Named>) -> bool {
    let mut words = arguments.iter().peekable();
    // How links are followed, before anything else. Not `-L`, which follows
    // every link under the start points.
    while words
        .peek()
        .is_some_and(|word| matches!(word.text.as_str(), "-H" | "-P"))
    {
        words.next();
    }
    // The start points: everything before the first test.
    while let Some(word) = words.peek() {
        if word.text.starts_with('-') && !word.home {
            break;
        }
        record(Kind::Path, &word.text, named);
        words.next();
    }
    while let Some(word) = words.next() {
        let Some((_, value)) = FIND_TESTS.iter().find(|(name, _)| *name == word.text) else {
            return false;
        };
        if let Some(kind) = value {
            let Some(value) = words.next() else {
                return false;
            };
            record(*kind, &value.text, named);
        }
    }
    true
}

/// `sed -n '<lines>p' [file…]`: print some lines, and nothing else. The
/// lines are a number, or two with a comma, where the second may be `$`.
fn sed_allows(arguments: &[Word], named: &mut Vec<Named>) -> bool {
    let [quiet, script, files @ ..] = arguments else {
        return false;
    };
    if quiet.text != "-n" && quiet.text != "--quiet" && quiet.text != "--silent" {
        return false;
    }
    let Some(lines) = script.text.strip_suffix('p') else {
        return false;
    };
    let is_number = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let lines_ok = match lines.split_once(',') {
        None => is_number(lines),
        Some((from, to)) => is_number(from) && (is_number(to) || to == "$"),
    };
    if !lines_ok {
        return false;
    }
    for file in files {
        if file.text.starts_with('-') && !file.home {
            return false;
        }
        record(Kind::Path, &file.text, named);
    }
    true
}

/// `date`, in UTC or not, with at most one `+format`. Setting the clock
/// (`-s`) and reading a file's time (`-r`, `-f`) are not listed.
fn date_allows(arguments: &[Word]) -> bool {
    let mut formats = 0;
    for word in arguments {
        match word.text.as_str() {
            "-u" | "--utc" | "--universal" | "-R" | "--rfc-email" | "-I" | "--iso-8601" => {}
            text if text.starts_with("--iso-8601=") || text.starts_with("--rfc-3339=") => {}
            text if text.starts_with("-I") && text.len() <= 9 => {
                if !text
                    .get(2..)
                    .unwrap_or("")
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase())
                {
                    return false;
                }
            }
            text if text.starts_with('+') && !word.home => formats += 1,
            _ => return false,
        }
    }
    formats <= 1
}

fn powershell_allows(spec: &PsSpec, arguments: &[Word], named: &mut Vec<Named>) -> bool {
    let mut operands: Vec<&Word> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        // A quoted `-x` is a text to PowerShell, not a parameter.
        if word.quoted || word.home || !word.text.starts_with('-') {
            operands.push(word);
            continue;
        }
        // `-Name:value` never gets here: the tokenizer refuses a
        // parameter with a `:` in it.
        let name = word.text.get(1..).unwrap_or("").to_ascii_lowercase();
        let found = spec.params.iter().find(|param| {
            matches!(param, Param::Switch(known) | Param::Value(known, _) if *known == name)
        });
        match found {
            Some(Param::Switch(_)) => {}
            Some(Param::Value(_, kind)) => {
                let Some(value) = words.next() else {
                    return false;
                };
                if names_a_provider(*kind, &value.text) {
                    return false;
                }
                record(*kind, &value.text, named);
            }
            _ => return false,
        }
    }
    if spec.operands.most.is_some_and(|most| operands.len() > most) {
        return false;
    }
    for operand in operands {
        if names_a_provider(spec.operands.kind, &operand.text) {
            return false;
        }
        record(spec.operands.kind, &operand.text, named);
    }
    true
}

/// Whether a path a PowerShell command is given is not the name of one
/// file, so that checking the name would check nothing.
///
/// - **A pattern.** PowerShell's commands expand `*`, `?` and `[…]` in a
///   path themselves, quoted or not: `Get-Content '.en?'` reads `.env`.
///   (Only `-LiteralPath` does not, and it is held to the same rule: asking
///   once too often is the safe side.)
/// - **Something other than a file.** `Env:PATH` is the environment,
///   `HKLM:\Software` the registry, `Function:prompt` a function's text. A
///   colon belongs in a file's path only after a drive letter. Windows
///   refuses such names as paths; on other systems PowerShell still reads
///   them as its own drives, where a file system would take them for a
///   file's name.
fn names_a_provider(kind: Kind, text: &str) -> bool {
    if kind == Kind::Text {
        return false;
    }
    if text.contains(['*', '?', '[', ']']) {
        return true;
    }
    let mut chars = text.chars();
    let drive = matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(letter), Some(':'), Some('/' | '\\')) if letter.is_ascii_alphabetic()
    );
    let rest = if drive {
        text.get(2..).unwrap_or("")
    } else {
        text
    };
    rest.contains(':')
}

#[cfg(test)]
mod tests;
