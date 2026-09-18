# Changelog

## 0.3.9

- **A Cowork session running in Claude Desktop no longer demands
  `--force` for a Claude Code switch.** Claude Code records who started
  it, and a switch is refused while a session that logs in through the
  store is alive. The Desktop's agent-mode sessions record `local-agent`,
  not `claude-desktop`, and were being counted as the store's -- in the
  app's own bundle that entrypoint sits in the same environment as the
  Desktop's token, so those sessions never read the store at all.

## 0.3.8

- **A Claude Desktop from the Microsoft Store is found at all.** The
  packaged data directory sits under a package family name, and the two
  Anthropic ships are not spelled alike: `Claude_<hash>` for the MSIX a
  downloaded installer sideloads, `AnthropicPBC.Claude_<hash>` for the
  Store's. Both are named in the app's own build; ccred matched a
  `Claude_` prefix, so a Store install read as "no Claude Desktop here"
  on a machine that plainly has one. The identity before the hash is what
  is matched now, and the `Claude-3p` build's directories are candidates
  too.
- **A Desktop whose data was moved off a network path is found too.**
  Chromium cannot keep a profile on a UNC path, so when `%APPDATA%` is one
  -- a roaming profile on a file share, which is how a managed Windows
  account is often set up -- the app puts its data in
  `%LOCALAPPDATA%\Claude-Data` instead. That is now one of the places
  ccred looks.
- The archived index is written sorted, which is how the app writes it:
  the point of patching that file rather than regenerating it is to leave
  it looking the way it was found.

## 0.3.7

- **Both of the Desktop's session lists are shared, not one.** The app
  draws its sidebar from `claude-code-sessions/` and from
  `local-agent-mode-sessions/`, both per account and both of the same
  shape -- read out of the app's own build, which scans exactly those two.
  Only the first was shared, so half a sidebar stayed behind on a switch,
  and a sign-out inside the app stranded the other half where nothing went
  looking for it. Each list is brought up to the shared one for its own
  kind, so an agent-mode session is never written into the Claude Code
  list.
- A carry of sessions and no groups said "2 sessions and 0 sidebar
  groups", which reads as a tally rather than as news. A count of zero is
  left out.

## 0.3.6

- **A switch that shares nothing says so.** The shared sidebar is a list
  of Claude Code sessions -- the ones started from inside Claude Desktop
  -- and someone who has never started one has an empty list, so the
  switch printed nothing about it at all. That is indistinguishable from
  a feature that did not run, and it was read that way. It now says there
  is nothing to share yet, and where the chats in the sidebar actually
  live: on Anthropic's side, under the account, which nothing local moves
  between accounts.
- **A session entry is known by the `sessionId` in it, not by its name.**
  Only `local_*.json` counted, which is what the build this was written
  against calls them. A Desktop that renames them would have shared
  nothing, silently; the same directory's `scheduled-tasks.json`, which
  has no `sessionId`, is still not an entry and does not travel.
- The README said "one sidebar for every account" without saying which
  list that is. It says now, in the first sentence, that conversations
  with Claude are not in it.
- **The last four lines that ran off the page.** A note under a table row
  -- what `list` prints about a profile it cannot read -- was the one line
  in a table that could be wrapped and was not. `rename` printed its
  warnings without the wrap every other warning goes through. An error's
  `caused by:` line names a file and was left whole. And `save` put the
  reason a half was not saved in a table cell, where a 97-character
  sentence made the row 129 columns wide; a reason that does not fit the
  column hangs under the row instead.

## 0.3.5

- **Sidebar groups that have nowhere to go no longer hide the chats that
  went.** They are written into Claude Desktop's own config, and a
  Desktop that has never been started has not written one -- which is
  exactly the state a switch walks into on the way to a first login. The
  whole pass was reported as having failed, so the chats that did go in
  were not counted anywhere. The groups wait in the shared sidebar, which
  is where they live, and the next switch to that profile puts them in.
- **One account, one Desktop name.** `ccred save <name>` recorded the
  account Claude Desktop is logged in as under whatever name it was
  given, so saving two Claude Code profiles while the Desktop sat on one
  of them left two Desktop rows for one login, both saying they were the
  live one, with nothing to switch between. The Claude Code half is saved
  as always and the Desktop half says which name already has it.
- **A save records the account even when the shared sidebar cannot be
  written.** It refused instead, which lost the identity -- the thing a
  save is for -- over an index that is rebuilt on every switch.
- **Nothing printed runs off the page.** Messages are broken at word
  boundaries to the width the tables already use, with a warning's remedy
  hung under what happened -- the refusal to delete a parked login was one
  240-column line, and `doctor` put a Windows path in the middle of a
  sentence. A path is never broken: half of one looks like another file
  and neither half can be copied.
- **A refused switch no longer deletes a parked Desktop login.** The
  target's metadata is written before the plan is made, so the plan can
  see the Desktop is already on that account, and taken back when the
  plan is refused -- by deleting the directory it sits in. That
  directory is also where the parked login lives, so a profile whose
  metadata had gone missing lost its login to a refusal whose own
  message said nothing had been moved. It takes back the file it wrote,
  and the directory only if that was all it held.

## 0.3.4

Names people choose and names this tool uses, which turned out to share a
directory.

- **The shared sidebar moved out of the profile name space.** It lived in
  `~/.ccred/desktop/sidebar/`, beside the parked logins, whose names are
  whatever anyone saves -- so `ccred save sidebar` put a profile's login and
  every account's chat list in one directory, and `ccred rm sidebar --purge`
  deleted both. It is `.sidebar` now: a profile name has to start with a
  letter or a digit, so nobody can take it. An existing union is moved along
  the first time a switch reads it.
- **A profile called `unclaimed-something` is not a stray directory.**
  `unclaimed-<timestamp>` is what a parking place for an account nobody
  saved is called, and `doctor` reported anything under that prefix as
  belonging to no profile -- advice that ends in "delete when sure", naming
  somebody's parked login. A profile is its metadata file, and that is what
  the check reads now.
- **The uninstall plan counts parked Desktop logins, not directories.**
  It counted every entry in the store, where a profile whose Desktop
  login is the live one leaves a metadata file and the shared sidebar is
  an index -- so `ccred uninstall --purge` said it would delete logins
  that were not there, about the one thing in the plan that no copy can
  replace.
- **The Desktop's index of archived chats is patched, not rewritten.**
  It was read for the one key this tool cares about and written back
  with that key alone, so a `v` field, or anything a later build of the
  Desktop puts in there, was dropped the first time a switch archived
  something. An index that cannot be read at all is now left exactly as
  it was found.
- **A sidebar that cannot be written no longer fails the switch.** Both
  passes raised: one before the move, which refused to switch a login
  because an index could not be written, and one after it, which reported a
  switch that had already happened as a failure. The shared sidebar is built
  out of lists the accounts keep themselves -- it says what went wrong and
  the next switch to that profile does it.

## 0.3.3

The sidebar machinery, which is the part of Desktop support that touches
Claude Desktop's own files.

- **A carry that cannot be put back is kept.** Merging sidebar groups back
  means writing into `claude_desktop_config.json`, and that file can be
  absent -- a fresh install that was never configured -- or mid-write, or in
  a shape this version does not know. Every one of those raised, from a
  point after the directories had already moved: a switch that had happened
  was reported as a failure, and one line further on the carry would have
  been deleted with the groups still in it. The same for the chat lists,
  whose move cannot cross a file system, so a `CCRED_HOME` on another volume
  hit it. Both now stay put with the reason said, and the next switch takes
  them.
- **A restored sidebar counts what actually moved.** It reported everything
  the carry held, while the merge deliberately leaves a chat whose name is
  already taken on the other side -- that copy is the account's own and
  newer. The count was a claim about somebody's chats that was not true.
- Claude Desktop's two files are read the way every other JSON here is: a
  byte-order mark from an editor is not a malformation, and UTF-16 says so.

## 0.3.2

Walking the documented Desktop flow end to end, and reading the new README
paragraph against the code.

- **The uninstall plan says a parked Desktop login would go.** The
  confirmation counted them and `--json` reported them, while the plan a
  person reads before answering listed profiles, backups and the receipt and
  said nothing about the Desktop -- the one thing in there no copy can
  replace, since its token is encrypted and nothing was ever kept aside.
- **Signing out inside the app does not delete anything**, which is what the
  README had started saying. It throws the login away -- the token is gone
  and only a fresh login brings the account back -- and leaves that account's
  chat list in the directory, where the next account inherits it. The chats
  stop appearing, which looks exactly like deletion; `switch` carries them
  out and hands them back, and that is the whole reason it does.
- The two listings are spaced and summed alike, rather than a table followed
  by an afterthought, and the README shows the two-table form.

## 0.3.1

Three things the Desktop support was still missing, found by fuzzing the new
paths and by reading `rename`'s own help against what it does.

- **A rename takes the Desktop login with it.** `work` and `work-desktop`
  are one profile under two roofs, and 0.3.0 said so in the help while
  moving only the Claude Code half -- so a rename broke the pair, leaving
  `work-desktop` and its parked login belonging to a profile that no longer
  existed.
- **The refusal to switch a running Desktop names the lock file.** On
  Windows anything but a clean open counts as held, which is the safe answer
  and sometimes the wrong one; someone whose Desktop is closed had nothing
  to go and look at.
- Two regression tests for moves that would lose a login quietly: parking
  over a place already taken, and a move whose second rename fails, which
  must report a failure and say where the live login was parked.

288 invocations over damaged Desktop state -- an empty, truncated, UTF-16,
binary or missing `config.json`, a lock file held, the directory replaced --
produced no panic and lost no login.

## 0.3.0

**Claude Desktop.** The Desktop app logs in on its own, and a session it
opens runs as whatever account *it* is logged in as, whatever
`.credentials.json` says. Its token is encrypted with the operating system's
keyring, so nothing here can read it, check it, or tell whose it is -- and
every safety rule in this tool depends on doing exactly that before a write.
What can be moved is the whole data directory: one complete logged-in state,
all of it one account's. So a Desktop login is a profile of its own,
`work-desktop`, switched separately, and `ccred save work` records both
halves. The identity comes from the directory's own `lastKnownAccountUuid`,
which is plain text; nothing else in there is read, and nothing is ever
copied, because a token that cannot be checked is not one to duplicate.

Contributed by Yagat0, who tested it on Linux. What the review and a Windows
machine added:

- **Windows keeps it in two different places**, and which one a machine has
  does not depend on the Store: an MSIX package -- what a downloaded
  installer produces as readily as the Store -- runs with filesystem
  redirection, so what the app believes is `%APPDATA%\Claude` lands under
  `%LOCALAPPDATA%\Packages\Claude_<publisher>\LocalCache\Roaming\Claude`,
  and a process outside the package sees only that. Measured on a machine
  running 2.110.1.0: the classic directory did not exist at all. Both are
  checked now, and `doctor` names the one it settled on.
- **A login whose owner cannot be read is not moved.** The identity file was
  read with `.ok()`, so a file being rewritten, truncated by a crash or
  refused by permissions all came back as "nobody's" -- and the plan then
  parked the live directory under a name no command restores.
- **`rm <name>-desktop` refuses to delete a parked login** unless `--purge`
  says to. That directory is the login; its token is encrypted, so nothing
  can be copied aside first and nothing can put it back.
- **A profile saved as `foo-desktop` before the suffix meant anything** is
  listed and renameable again. It had become invisible to every command and
  was still deleted by `uninstall --purge`.
- A half-finished move exits 7 rather than 0, and says where the live login
  was parked. The Desktop save takes the profiles lock. The suffix is read
  whatever its case.
- The output was rebuilt against the rest of the program: callouts instead
  of semicolon-joined lines up to 155 columns, a `Fields` block instead of
  three counts joined by commas, a `Table` instead of hand-laid spacing, and
  a heading over each of the two listings.

Also: packaging no longer starts -- and fails -- on the release workflow's
pull-request run, which is what the repository's first pull request found.

## 0.2.42

- **`ccred restore <name>` puts a removed profile back.** `rm` prints where
  it put a copy of the credentials, because deleting the only stored copy of
  an account should not be the one mistake here that cannot be undone --
  and nothing could use that copy: reading the file back into place by hand
  was the entire recovery. The copy is parsed, validated and checked against
  the tokens other profiles hold before anything is created. What comes back
  is the credentials alone, and it says so: the account blob a switch
  restores is not in the copy, so the profile reads as an unknown account
  until the next save fills it in.
- The commands in `ccred --help` are listed in the order they are used.
  `rename` and `restore` were both numbered six, so where `rename` appeared
  was decided by declaration order rather than intent.

## 0.2.41

**No panic is left in the code that runs unattended.** A panic in a scheduled
refresh is a slot that reports nothing and a profile that quietly goes stale,
with no message anywhere a person looks -- the exact failure this program
exists to prevent.

Two were left. The `claude` handle was unwrapped with "discovered just
above": true today, one rearrangement away from not being, and now a
contained per-profile failure like everything else that can go wrong in that
loop. And a profile name's first character was unwrapped two checks after
emptiness was refused, which is a proof a later edit can quietly break.

`scripts/no-panics.sh` keeps the count at zero and CI runs it. Tests stay
exempt: one that cannot unwrap says less when it fails.

## 0.2.40

- **A rename that only changes case takes the pointer with it.** `rename work
  WORK` is the one way to fix a spelling on a file system that ignores case,
  and it left the pointer saying `work`: the check for "was this the active
  profile" ran after the directory had moved, so the old name resolved to the
  new one and compared unequal to itself. Invisible on Windows, a pointer to
  nothing anywhere that tells the two apart.

## 0.2.39

Both found by looking at a real machine's log and schedule the morning after.

- **A run that decided to do nothing is written down.** The log exists
  because a scheduled run is invisible otherwise -- Task Scheduler discards
  everything a job prints -- but a firing that met the rate limit returned
  before anything was written. So the log could not tell "the timer fired and
  there was nothing to do" from "the timer never fired", which is the
  question it is there to answer. The skip is a line now, marked as the
  scheduler's, and still not recorded as the last *run*: that measures from
  the last one that did something.
- **"Nineteen hours ago" is no longer "today".** `doctor` counted whole days,
  so a run at nine in the evening read as today when the machine was asked at
  eight the next morning.

## 0.2.38

- **`ccred rename <old> <new>`.** There was no way to rename a profile: `rm`
  and `save` again only works for the account that happens to be logged in,
  so a profile named in haste -- and the second one usually is, since that
  save is what registers the schedule -- was named that for good, short of
  moving directories by hand. The rename moves the profile and the copies
  belonging to it, and takes the active pointer with it. No credential is
  written: one directory rename, then the pointer, in that order, because a
  crash between them leaves a pointer naming something absent, which `list`,
  `current` and `doctor` all report and the next `switch` repairs. It refuses
  a name already in use, and refuses to run while an interrupted switch is
  pending.
- `ccred rm --help` listed `<NAME>` with a blank line under it, and so did
  `switch` and `restore`. A test now walks every subcommand's help and
  refuses an argument with nothing beside it.

## 0.2.37

**`ccred rm <name> --purge`.** Ordinary `rm` copies the credentials aside
before deleting the profile, because a mistyped name is the one mistake here
that cannot be taken back. That is the right default, and the wrong answer to
the other request -- "remove this account from this machine" -- which had no
command at all short of uninstalling ccred entirely.

`--purge` deletes the profile and every copy of that account: the rotation
under its name, and the one keyed by account that a switch writes when the
live credentials belong to nobody. It names the directories that went, or
says there were none, and it touches no other account.

## 0.2.36

**A file that is not UTF-8 says so.** PowerShell writes UTF-16 whenever it
redirects output, so a credential file copied between machines with
`Get-Content | Out-File` arrives in it -- and what ccred said about that was
"malformed JSON", which sends someone looking for a syntax error in a file
whose syntax is fine. `save` said "is Claude Code set up in this home?",
which is not what is wrong either.

Encoding is its own error now, so its sentence survives being turned into a
string, which is all a `doctor` finding or a `current` report keeps of one.
A refusal to save names the reason the account could not be read rather than
guessing at it.

## 0.2.35

Two things that made ccred refuse to work on a file, or a machine, that was
perfectly usable.

- **A byte-order mark is no longer a malformed file.** Notepad writes one
  into every UTF-8 file it saves, and so does PowerShell 5.1 redirection --
  so someone who opened `.credentials.json` or `.claude.json` to look at
  something, which is what a person does when they suspect trouble, came back
  to ccred calling their file malformed and refusing to save, switch or
  report. RFC 8259 allows ignoring the mark; it is dropped before anything
  reads the bytes and never written back.
- **A machine with no HOME works when both directories are named.** In a
  container with a scrubbed environment ccred refused everything, and advised
  the very thing that did not help: passing `--ccred-home` and
  `--claude-config-dir` failed identically, because the home was resolved
  before anything looked at what was passed. It is resolved last now, and
  only for a directory left to the default.
- A refused symlink says why it is refused and that the way out is a real
  file.

## 0.2.34

**A wrong clock no longer switches the schedule off.** Three decisions read
moments an earlier run wrote down: a profile's backoff, its next-attempt
time, and the schedule's own rate limit. A machine whose clock was wrong when
that run happened -- a dead CMOS battery, a restored VM, a dual boot -- wrote
a moment that has not arrived, and all three then waited for it. Silently,
and for as long as the mistake was: a year wrong meant a year of a profile
that is never refreshed and quietly expires.

The furthest any of them can legitimately sit ahead of now is one backoff, so
beyond that they are discarded rather than obeyed, and `ccred doctor` says so
outright when the last run is recorded in the future.

## 0.2.33

**A damaged profile is no longer a wall around the profile you are leaving.**
A fuzz run over ccred's own files kept finding the same shape: one file
inside a profile turned unreadable, and `switch` to a *different* profile
then ended with "I/O error".

- Mirroring the outgoing credentials skips on any failure except a lock,
  with the reason shown and the live credentials copied aside first -- the
  machinery that already existed for a mirror refused on identity grounds.
  A lock still means exit 6, because that is the one failure worth waiting
  out.
- Account details that cannot be read are treated as ones that are not
  there. That blob is a cached copy of something Claude Code refetches; the
  credentials, which it cannot, have already moved by the time it is written.

What remains, and is correct, is a refusal to switch *to* a profile whose
own credentials cannot be read.

## 0.2.32

- **`refresh --dry-run` meets the rate limit the real run meets.** A preview
  of `refresh --if-older-than 48` -- the exact command the registered job
  runs -- skipped straight to the per-profile decisions and showed work that
  the real command would have refused, because the limit had already been met
  that day. The gate is one function now, called by the run and the preview
  alike, so they cannot drift apart again.

## 0.2.31

- **`ccred refresh --dry-run`.** `schedule install` and `uninstall` both had
  one; the command that spawns Claude Code against every stored profile did
  not, so "why did the schedule leave that profile alone" could only be
  answered by running it and spending an exchange to find out. The preview
  runs the decision phase and stops: no lock, no spawn, nothing written, not
  even a log entry. It says "would refresh" rather than "refreshed", because
  a preview reporting things in the past tense is worse than none.
- The npm packages carry `LICENSE-MIT` and `LICENSE-APACHE`, and the wrapper
  the README too. A `license` field names the terms; it does not hand anyone
  the text, and for someone installing from npm those packages are the only
  copy of ccred they see. The channel is now verified end to end as well --
  generated from the real artifacts, packed, installed from the tarballs and
  run -- which it had never been.
- A profile written by an older ccred is pinned as a test rather than a
  belief: checked against a real 0.2.24 binary in both directions, then
  written down as a fixture with only the keys that have always existed,
  plus one from no version at all, which must survive a switch untouched.

## 0.2.30

Both found by pointing ccred at the real files on a live machine rather than
at fixtures.

- **A patch of `.claude.json` no longer moves a number it never touched.**
  `lastCost` -- a dollar total accumulated over months -- came back out of a
  switch as `248.0686325000001` where it went in as `248.06863250000006`.
  Not a rounding of the same value: a different double, one unit in the last
  place away, because serde_json's parser is not correctly rounded for some
  inputs unless `float_roundtrip` is on. It is on now. A copy of a real 68 KB
  document -- 72 keys, 17 projects -- now survives a save and a switch with
  nothing changed at all.
- **A Claude Code session that has written its key and not yet its session
  file now counts as running.** A live session leaves both `<pid>.json` and
  `<pid>.<hash>.key`; only the first was read, so between the two writes a
  session was invisible to the guard that refuses to switch under one --
  the single situation where switching corrupts a profile.

2.1.x also keeps a daemon, which writes no session file at all. Why it is
deliberately not counted -- it is permanently resident, so counting it would
mean `--force` on every switch on every machine -- is written down beside the
code, with what would have to change to start counting it.

## 0.2.29

Both of these come from reading a real log and a real `doctor` report and
finding the answer was not in either.

- **`doctor` says whether the schedule is actually running.** A registered
  timer firing into a failure looks, from a terminal, exactly like one that
  works: the job prints into a log nobody opens, and on Windows Task
  Scheduler discards even that. It now reads the last-run record ccred has
  been writing all along -- how long ago, and whether anything needed
  attention. A week without a run is a warning, since the longest gap
  between scheduled ones is four days.
- **`ccred log` says which runs the schedule started.** Every line read the
  same, so "the timer has not fired since Tuesday and you have been
  refreshing by hand" was not something the log could tell anyone. Only the
  registered job passes `--if-older-than`, so that is the tell: a FROM
  column reads `timer` or `you`, and older records say nothing rather than
  guessing.

## 0.2.28

**Linux installs, read this one.** A scheduled refresh inherits the user
manager's environment and reads no shell profile -- so a `claude` installed
by nvm, volta, bun or asdf works when you type it and cannot be found when
the timer fires. That is the failure a Linux install is most likely to hit,
and it is silent: the job exits into a log nobody opens while profiles go
stale.

- Those directories are searched directly now, alongside `~/.local/bin`,
  `/usr/bin`, `/usr/local/bin` and `/opt/homebrew/bin`.
- `ccred schedule install --claude-path /full/path/to/claude` registers a
  path with the job for anywhere else. It is made absolute, since the job
  starts in a directory of the scheduler's choosing, and a path that is not
  there is refused while a person is present to fix it.
- `ccred doctor` says which `claude` it found, or warns that it found none.
  A warning rather than an error: switching profiles never spawns it.
- The README claimed a refresh leaves the deadline "where it was, to the
  millisecond". It does not, and the program knows -- `WINDOW_JITTER_MS`
  exists because one measured exchange moved it 809 ms earlier.

Two things that do not change what ccred does, but change what is known
about it. A CI job now registers a real systemd timer, asks `systemctl
--user` what it made of it, and removes it again; every previous check on
that backend compared text ccred wrote against text ccred expected. And the
property this program exists for -- no sequence of writes leaves a profile
without usable credentials -- is a test rather than an intention, after a
fuzz run over ccred's own state files and 80 commands racing in one
directory failed to break it.

## 0.2.27

A fuzz run over ccred's own state files and 80 commands racing each other in
one directory. No panic and no corrupted profile came out of either; what
follows is what did.

- **A pointer file that has turned to nonsense no longer ends the command.**
  Six bytes in `state/current` name the active profile, and when they stopped
  being readable text, `list`, `current`, `switch` and `rm` all exited with
  "I/O error" -- the commands someone runs *because* something is wrong.
  `list` and `current` now report it and carry on, and `switch` treats the
  pointer as unknown and writes a new one, which is the repair.
- **A switch with no active profile copies the live credentials aside**
  when no profile holds them. The same loss a *wrong* pointer has been
  guarded against since 0.2.19, reached by a pointer that was missing rather
  than wrong.
- **The battery warning could only fire in English.** `schedule status` read
  `DisallowStartIfOnBatteries` out of the localised `schtasks` listing, so on
  a Czech or German Windows the one warning that explains a laptop which
  never refreshes on battery never appeared. It reads the task XML now.
- Exit 6 says what to do about itself: Claude Code takes the credential lock
  whenever it refreshes, so the answer is to try again in a few seconds. A
  malformed JSON file points at `doctor` and `restore`.
- `linger` detection falls back to `LOGNAME`, since a service or a container
  may set only one of the two.
- The `.deb` is installed on a runner and made to answer two commands before
  it is published. It was built, signed and attached without anyone ever
  running what was inside it.

## 0.2.26

Nothing here was found by a user; a smoke run of the released binary turned
all of it up.

- **The credential file is written the way Claude Code writes it**, on one
  line with no trailing newline. ccred pretty-printed it, so a switch left
  the file 20% larger and across thirteen lines. Both shapes parse and
  nothing was lost either way, but a file someone opens should look the way
  they left it. `.claude.json`, which Claude Code does pretty-print, is
  unchanged.
- **`list` says when the active profile no longer exists.** It showed a
  table with no active row and the summary "all healthy", which reads as
  "nothing is active" rather than "the one you were using is gone".
  `current` and `doctor` already reported it. `list --json` is still an
  array of profiles.
- `ccred switch` to the profile that is already active reported `work ->
  work`. It is a resync, and says so.
- An orphaned-credential backup path came out with a lone forward slash
  among the backslashes on Windows -- in a path printed for someone to go
  and read after a switch went sideways.
- Whether a refresh probe rewrote the credential store is now judged from
  the file's bytes. It was the modification time and the length: a token
  swap keeps the length, and the timestamp can be coarser than the write.
  Only a diagnostic line was ever wrong because of it.
- The `.deb` carries a provenance attestation and a `.sha256`, like every
  other artifact. It is the one people install as root, and the README told
  them to hand it to `apt` with nothing to check it against. The 0.2.25
  packages were rebuilt with both.
- The summary line in `list` had two spaces after its comma.

## 0.2.25

- **The installer's PATH advice is one command per line.** The line
  `source $HOME/.local/bin/env (sh, bash, zsh)` names the shells in the
  same colour as the command and only a space away from it. Someone
  installing ccred for the first time copied the whole line, and the shell
  tried to run a file called `(sh,`. Each command now sits on its own line
  under a heading that names the shell. The released installers for 0.2.24
  were rewritten in place, so the link in the README already prints the
  clearer text.
- Build scripts in the dependency tree are held to an allowlist, which is
  what the top of `deny.toml` has claimed all along. Thirteen crates run
  code at build time today; a fourteenth fails CI until someone reads it.

## 0.2.24

An audit of the documentation against the program found seven mismatches.

- **`--json` now works everywhere it is promised.** `restore --json` and
  `schedule uninstall --json` printed nothing, and `schedule install
  --dry-run --json` printed the human block: a script piping any of them
  into a parser got an error. A test runs every documented command with
  `--json` and parses what comes back.
- The refresh summary says "1 needs attention", not "1 need attention".
- The README credited a bare `ccred refresh` with the over-fire rate limit.
  That needs `--if-older-than`, which only the scheduled job passes; a
  refresh you type does the work, as it should.
- The exit-code table said Claude Code running gives 6. It gives 7: a switch
  under a live session is refused, not postponed.
- `log` shows every refresh, not only the scheduled ones.

## 0.2.23

**Upgrade, especially on Windows and macOS.**

### Credentials that could be lost or refused

- **A profile spelled differently was treated as a different profile**, while
  the file system treats it as the same one. `ccred switch WORK` for a profile
  saved as `work` wrote a pointer that matched no directory: `list` showed
  nothing active, and `ccred rm work` -- the profile whose credentials were
  live -- passed the guard that refuses to delete the active one. Saving
  under the other spelling was refused as another profile's credentials.
  Names are now resolved to the stored spelling on Windows and macOS.
- **`save` can repair a profile whose credential file no longer parses**,
  which is what the write gate always promised. Reading the damaged file
  came first, so it refused instead.

### Commands that stopped working

- **A timestamp of `i64::MIN` made `current`, `list` and `doctor` panic**
  with exit 101 -- in exactly the commands someone runs because a file looks
  wrong. Every subtraction on a stored time now saturates.
- **A failed `.claude.json` write no longer wedges `save` and `switch`.** It
  left the switch half-applied and every later command failing on the same
  error, since both replay the journal first. It is a warning now, like the
  read of that file already was, and the mismatch it leaves is one `current`
  and `doctor` report.
- **A profile is no longer latched as "needs login" by text ccred does not
  control.** The phrases were searched for anywhere in the output -- which
  includes the names of the user's own MCP servers and the model's reply.
  They are now read from stderr and from the prompt rung's own error result.

## 0.2.22

- **An expired deadline no longer reads as `0d`.** The day count divided,
  which rounds toward zero, so a refresh window an hour past its end looked
  like one expiring today. It now floors: `-1d`.
- README says what each exit code asks a script or scheduler to do, and what
  to do about the four things `doctor` reports most often -- an expired
  deadline, a login that comes from an environment variable, a pointer that
  disagrees with the live account, and a blocked profile.
- The `.deb` ships `/usr/share/doc/ccred/copyright`, which Debian policy asks
  for by that name.
- The npm layout step refuses a release missing a platform instead of
  publishing a wrapper that has no binary for it.

## 0.2.21

**Upgrade. A refresh running while you switched could sign out the session
you had just switched to.**

### Refresh and switch no longer overlap

- A refresh read which profile was active once, before a loop that can run
  for minutes. Switching during it made a profile live that the refresh
  still treated as idle and then refreshed through its own store, retiring
  the token the live session held. `switch`, `save`, `rm` and `restore` now
  share a lock with `refresh`, which takes it one profile at a time and
  reads the active profile under it. A switch that meets a running refresh
  says so and asks to be retried.
- A profile whose stored refresh token is the live one is never refreshed,
  whatever the active-profile pointer says.

### Situations only a person can settle

- A new `blocked` result replaces `broken` for them -- another account
  logged in, an interrupted switch that could not be read, a profile holding
  the live tokens. It still exits 4, but no longer makes every scheduler
  firing a full run.
- After an unreadable switch journal, nothing copies the live credentials
  into a profile until you run `ccred save` or `ccred switch`; `doctor`
  explains.
- `current` and `doctor` now notice live tokens that are stored as a
  different profile than the active one, which the account name in
  `.claude.json` cannot show.
- `doctor` reports profiles that share a refresh token -- possible before
  0.2.20 -- since refreshing one signs the others out. Such pairs no longer
  report every mirror as failed.

### Also

- A systemd install refuses a binary path with a control character.
- The refresh summary counts a mirror that wrote nothing as unchanged, and a
  relative `HOME` is made absolute.

## 0.2.20

**Upgrade. A token renewal was not saved into its profile whenever its
refresh deadline came back a fraction of a second earlier -- which a real
renewal was measured doing -- and the profile then held a refresh token the
server had already retired.**

### Renewed tokens that were thrown away

- **A renewal whose refresh deadline read slightly earlier was refused.**
  The deadline is fixed at login but recomputed from whole seconds on every
  exchange -- measured: 809 ms earlier after a real one. The write gate
  treated that as a shrinking window, so the scheduled mirror and switching
  away both refused to store the renewed tokens, and the profile kept a
  rotated-away refresh token. Switching back put a dead token live. The gate
  now allows ten minutes of jitter.
- **A refused mirror is no longer reported as "mirrored".** It shows as
  broken, and the run exits 4 so a scheduler flags it.

### One account's tokens in another account's profile

- **A profile never takes a refresh token another profile holds.** The name
  check cannot see live tokens that belong to one account while
  `.claude.json` names another -- what a switch killed half-way leaves -- and
  three paths reached that state. The token itself says whose it is.
- **The scheduled refresh settles an interrupted switch first.** Before, the
  switch's target counted as idle and could be refreshed through its own
  store, retiring the token the live session was using.
- **An unreadable switch journal stops the command that finds it**, after
  moving it aside, instead of carrying on with an identity it cannot trust.

### Schedules

- On systemd, a `$` in the binary's path is no longer doubled (systemd does
  not expand variables there, so the job could never start), and a path
  systemd refuses outright is refused at install.
- `uninstall` finishes on Linux when the user manager does not answer, and on
  macOS no longer reports an agent removed that launchd still has loaded.

### Also

- Claude Code running under a versioned Node binary (`node-22` on Fedora)
  is recognised, so it blocks a switch.
- Tests no longer depend on wall-clock timing, and none runs a real
  `uninstall`.

## 0.2.19

- **`doctor` checks the copies of credentials for loose permissions too.**
  The last-known-good copy of each profile and every backup hold the same
  tokens as the store, and a backup is the file most likely to have been
  copied away and back. Only the stores themselves were checked.
- The token-leak test now runs `restore`, `log`, `refresh` and `uninstall`
  as well, and reads every non-credential file ccred writes, which is what
  SECURITY.md said it did.

## 0.2.18

**An independent review of 0.2.15 to 0.2.17 found a way to store one
account's tokens in another account's profile. Upgrade.**

### Credentials that could end up in the wrong place

- **`save` right after a killed `switch` could store the wrong account.** A
  switch killed after writing the live credentials leaves its lock behind.
  `save` gave up on healing the switch while that lock was fresh, waited for
  it to go stale, and then stored the switch target's tokens under the name
  `.claude.json` still gave -- the other account. It now settles the switch
  under its own lock first. A test reproduces the killed switch.
- **`uninstall --purge` could be pointed at Claude Code's configuration** by
  spelling a path differently: on Windows the safety check was
  case-sensitive, and nowhere did it resolve `..`. Paths are now compared in
  one canonical spelling.

### Schedules that looked fine and never ran

- **A path with `'`, `"`, `\` or `$` broke the systemd unit.** The service
  ended up with no command while the timer still showed a next run. Every
  word is now quoted and escaped the way systemd reads it.
- **Windows read the task definition back in the console's code page**, which
  mangles any character the page lacks. A binary under a profile folder with
  diacritics was reported missing. The definition is now read as UTF-8, in a
  hidden console of its own.
- **A schedule that could not be removed was reported removed**, and
  `uninstall` then deleted the binary the task still pointed at. Removal is
  now checked, and `uninstall` stops if the task is still there.
- A forced refresh that met a signed-out account left a false expiry behind.

### Also

- **An unreadable switch journal no longer wedges `save` and `switch`.** Both
  replay the journal first, so one that failed to parse failed them for good.
  It is now moved aside, with a warning saying where.
- `ccred log` shows the history from before a log rotation.
- **A write no longer fails because a file was briefly held open on
  Windows.** An antivirus scan of a freshly written credential file blocks the
  rename that replaces it; the save or switch failed outright. The rename is
  now retried for up to about two seconds when access is denied.
- **A stale session file no longer blocks switching once its PID is reused.**
  A running Claude Code is now recognised by what the process is -- `claude`,
  a versioned native binary, or `node`/`bun` -- rather than by the PID alone,
  which Windows hands out again quickly. The Windows process list is also
  parsed by its quotes, so a locale that writes `1,024 K` cannot shift the
  columns. A new end-to-end test starts a stand-in session and checks both
  that it blocks a switch and that its leftover file does not.
- **A schedule registered from a Homebrew or Scoop install survives upgrades.**
  On Linux the binary's path came back with links resolved, so the job named
  a versioned directory that a later `brew upgrade` deletes. The stable
  `opt/ccred` (Homebrew) or `apps/ccred/current` (Scoop) path is registered
  instead.
- The npm launcher no longer lets Node 22+ spend seconds collecting network
  details on every run, and no longer suggests `cargo install` before the
  crate exists.

## 0.2.17

- **`doctor` warns when the schedule runs a different copy of ccred.** Found on
  a real machine: the scheduled task still started a copy installed months
  earlier, from before the fixes for losing credentials, while every command
  typed by hand ran the new one.
- **`uninstall` judges a linked binary by where the link leads**, so a Homebrew
  link in `/usr/local/bin` is left to brew.
- **A credential file caught mid-write is no longer reported as damaged.**
  `current`, `list` and `doctor` read without the lock Claude Code writes
  under; a half-written file made `doctor` recommend `ccred restore` over a
  file that was whole a moment later. They now read it again before judging.
- **A Pro plan shows as "Pro"**, not "ai". Such accounts report a generic
  rate-limit tier, so the organization type is used instead.

## 0.2.16

### Fixes

- **A lock taken over while `ccred` was stalled is no longer deleted or kept
  alive.** Past the 15-second stale limit Claude Code may reclaim the lock;
  `ccred` then removed Claude Code's lock on exit and kept refreshing it, so a
  crash would have wedged it. The lock's mtime now says whose it is, as in
  `proper-lockfile`.
- **An interrupted switch is only healed under the switch's own lock.** A
  switch still in progress looks the same from outside, and healing it could
  leave the pointer naming one account while the live store held another.
- **A schedule registered with `CCRED_HOME` or `CLAUDE_CONFIG_DIR` set now
  uses those directories.** The job runs in an environment of its own, so it
  refreshed the defaults -- which hold nothing -- and reported success. The new
  `--ccred-home` and `--claude-config-dir` options carry the choice into the
  job, and the systemd unit makes those directories writable.
- **The spawned `claude` is given the configuration directory `ccred` uses.**
  It was scrubbed, so for anyone with a relocated configuration the refresh
  probe read a different `.claude.json`, and for the default scope a different
  credential file.
- **Orphaned credentials get a rotation per account.** Ten copies of one
  account could push out the only copy of another.
- **The backoff now delays a failing profile** -- one, two, then four days.
  It used to top out at the daily interval, so it never delayed anything.

### Distribution

- **The Scoop bucket was stuck on 0.2.0.** Nothing ran its `autoupdate`. It now
  updates itself within a few hours of a release, after checking the archive's
  checksum. If you installed through Scoop, run `scoop update ccred`.
- Release runs are green again: the crates.io and npm jobs stay off until their
  accounts exist.
- `scripts/tag-release.sh` no longer runs `gh release create` out of its own
  help text.

## 0.2.15

### `ccred uninstall`

Removes the refresh schedule, the installer's receipt and the binary, and with
`--purge` every stored profile, backup and log. The Claude Code login in
`~/.claude` is never touched.

- **`--purge` asks first**, and refuses without a terminal unless `--yes` was
  given. What counts as credentials is judged from the disk, not from the
  profile listing: running it found that a profile too damaged to list, or a
  backup on its own, would have been deleted without the question.
- **`--purge` refuses outright**, before removing anything, when the data
  directory holds files `ccred` did not create, or is or contains the home
  directory or Claude Code's. `CCRED_HOME` can point anywhere.
- **A package manager's binary is left to the package manager**, with the
  command that removes it: Scoop, apt, pacman, npm, Homebrew and `cargo
  install`. Deleting a tracked file corrupts the manager's record.
- **Another copy is not uninstalled by accident.** The installer receipt only
  counts when it describes the running binary, and a schedule that starts a
  different, existing copy is left alone -- so trying out a build cannot switch
  off the installed one's refreshes.
- On Windows, where a running executable cannot be deleted, a detached process
  removes it a moment after `ccred` exits.
- `--dry-run` shows the whole plan and removes nothing.

### The schedule knows which binary it starts

- **`doctor` reports a schedule whose binary has gone** as an error. A package
  upgrade that moves the binary leaves a job the scheduler still calls healthy,
  with a next run, that fails the instant it starts. Nothing reported it.
- `schedule status` shows the registered binary, and marks it when it is
  missing. The command is read back from the task XML, the systemd unit or the
  launchd plist, and each reader is tested against its own renderer.

## 0.2.14

**An independent review of everything since 0.2.0 found credential-loss paths,
including one shipped in 0.2.13. Upgrade.**

### Credentials that could be lost

- **`rm` promised a backup it had never made** (introduced in 0.2.13). The
  backup routine returned success both when it wrote a copy and when there was
  nothing to copy, so `rm` always named a backup directory -- and deleted the
  last-known-good file with the profile. A profile whose live credentials were
  unreadable was destroyed while the command said it was safe. It now falls
  back to the last-known-good copy, reports the file it actually wrote, and
  says so when nothing survived.
- **A read error was treated as "Claude Code cleared the profile".** The guard
  then wrote the pre-refresh credentials over whatever was there, through a
  path that skips the window check -- so a Windows sharing violation during
  Claude Code's own write could replace freshly refreshed tokens with spent
  ones. An unreadable store is now left alone.
- **An explicit `null` made a credential file permanently unreadable.** It
  deserialises to nothing and re-serialises to nothing, so the lossless guard
  counted it as a dropped key on every load, for ever. Absent and null mean the
  same thing to every reader of the file.
- **`Some(0) > None`** made a store that vanished and came back logged out look
  like a successful renewal.

### Things that could wedge or stay stuck

- The lock's stale-reclaim branch spun at full CPU and never honoured its
  timeout when the reclaim could not succeed. A held lock is now exit 6
  (Busy), not 7: Claude Code takes it on every refresh.
- `save` held that lock across scheduler registration, which shells out with
  no timeout of its own.
- A latched `needs_login` could never be cleared -- `restore` did not clear it
  and an unchanged `save` returned before trying.
- The deadline warning cancelled the refresh it was meant to accompany, so a
  profile inside five days could not be renewed even with `--force`.
- Any persistent problem disabled the over-fire rate limit for as long as it
  lasted; only transient trouble lifts it now. And a failed spawn recorded no
  attempt, so there was no backoff at all.
- `doctor` and `current` still failed outright on corrupt metadata for the
  active profile, and `doctor` withheld the `ccred restore` hint from the one
  case it helps most.

### An account that works in Claude Code but reads as expired

Reported from a real Linux install. With `CLAUDE_CODE_OAUTH_TOKEN`,
`ANTHROPIC_API_KEY` or a Bedrock or Vertex switch set, Claude Code
authenticates from the environment and never touches the credential file,
which ages out while everything works. `doctor` now names such a variable --
the name only, never the value. `save` warns at once when it stores
credentials already past their deadline, and an expiry now says how long ago
it was, which tells a real lapse from a misread timestamp.

## 0.2.13

### `rm` keeps a copy

It deleted the profile directory outright, taking the credentials and the
last-known-good copy with them. `rm` is the one command here whose mistake
cannot be taken back -- a mistyped name, and the only stored copy of an
account is gone. Every other write in this tool is guarded, backed up or
recoverable; this was the exception.

The backups live outside the profile directory, so a copy taken first survives
the deletion, and the command says where it went. A backup that fails does not
block the removal that was asked for.

## 0.2.12

### `current` reports an unreadable live store

A credential file that will not parse ended the command with a bare error.
`current` is the first thing anyone runs, and the first thing they run when
something looks wrong, so it now says what it found and points at `doctor` --
which is where an unreadable credential file is a loud failure with an exit
code to match.

That completes the pass begun in 0.2.11: `doctor`, `list` and `current` all
produce a report on the trouble they exist to describe, instead of failing on
contact with it.

### Documentation caught up with the code

- `docs/install.md` opened with "nothing is published yet", which had been
  wrong for several releases. The table now says which channels are live --
  Scoop, the `.deb`, the release installer -- and what each of the rest is
  waiting for.
- `SECURITY.md` promised Windows ACL hardening as future work. It is not
  needed: the inherited DACL was measured and already grants only SYSTEM,
  Administrators and the owner. What the document was missing is that `doctor`
  checks the outcome on both platforms, and that the run log is `0600` and
  carries no rendered error messages.
- The README now states the two rules this work established: that Claude Code,
  pointed at a profile's store, is the one writer our gates do not cover and
  can be wrong; and that a failure belongs to the profile it happened to.

## 0.2.11

### One broken profile no longer ends the run

`refresh_one` propagated every error out of the loop, so a store that would
not load, a spawn that would not start, or a metadata write that failed ended
the whole run and took the other profiles' work with it. An unattended job
that gives up on every account because one is broken is worse than one that
reports the broken account and carries on -- especially when that report is
the only thing anyone will see of it.

### `--force` compares against the right baseline

It backdates the stored access-token expiry so an exchange happens, but was
still measuring success against the expiry from before that write. Whenever
the replaced token outlived its replacement -- an eight-hour renewal over
something longer -- a real exchange would have been recorded as a failure.

The backdating write also aborted the whole run on failure. A forced run that
cannot backdate is just a forced run that finds nothing to do.

### Commands that report on trouble no longer fail on meeting it

`doctor` ended the whole diagnosis with an error when the active-profile
pointer or the profiles directory could not be read. `list` refused the entire
listing when one profile's metadata would not parse. Both are commands someone
reaches for *because* something is already wrong, so producing nothing is the
one response that cannot help: each is a finding or a row now, with the rest
of the report intact around it.

### Answered

Which probe actually causes a refresh, which the design sketch left open and
proposed a whole command to find out. Measured against claude 2.1.236 with an
expired access token: `auth status --json` exchanges nothing, `mcp list`
renews the token, and the prompt rung is never reached. A refresh costs one
cheap invocation and no quota, and the ladder remembers which rung worked.

## 0.2.10

### The schedule warns before the deadline it cannot move

0.2.9 established that the refresh window is fixed at login: no amount of
refreshing postpones it. That makes warning ahead of it the only useful thing
an unattended run can do about it -- and until now `refresh` said nothing
until the token was already dead, which is the one moment a warning is
worthless.

A profile inside five days of its deadline is reported as expiring, ahead of
every gate that would otherwise stay quiet: a backoff, a recent attempt, a
window still above the refresh threshold. It counts as needing attention, so
a scheduled run surfaces it instead of exiting clean. Five days is two
scheduled runs.

It does not displace the two states that outrank it -- the active profile,
which Claude Code refreshes anyway, and one already past the deadline, where
the advice differs.

## 0.2.9

**The refresh path was working the whole time. The test for whether it worked
was not.**

### Success is a renewed access token, not a moved window

Measured against a live account, twice. A probe rotates both tokens -- their
hashes change -- and renews `expiresAt` by eight hours. `refreshTokenExpiresAt`
moves by half a millisecond, and on one reading came back microscopically
*lower* than before.

That deadline is a ceiling fixed when you logged in, and every rotated token
inherits it. Refreshing cannot postpone it. So the success test was watching
the one quantity that cannot change: every real exchange was recorded as "did
not move the refresh window", counted as a failure, and earned a backoff --
which is why this looked broken through four releases while it was in fact
doing exactly what it was asked to.

### The README was promising something impossible

It said `ccred` refreshes profiles on a schedule to prevent an idle account's
refresh token expiring. It cannot: the deadline is fixed at login and only a
new login resets it. What a schedule can influence is whether the token stays
in active use, and that is what it now claims.

## 0.2.8

### `--force` now forces something

It cleared the timing gates and still changed nothing, because none of them
was what stood in the way. Claude Code exchanges tokens only when it needs a
new access token, and only an exchange moves the refresh window -- so a
profile with hours left on its access token ran the entire probe ladder and
came back "left the store untouched". Correct, and useless.

A forced run backdates the stored access-token expiry first, which turns it
into a real exchange. This is also the only way to exercise the refresh path
without waiting for a token to age out, which is why that path went unverified
for as long as it did.

### `doctor` checks the permissions instead of assuming them

The writer asks for 0600, but a mode is only what was requested: a file
restored from a backup, copied with `cp -p`, or synced from another machine
can arrive readable by everyone, and nothing said so. Every credential file
`doctor` can find is now checked, and a loose one fails loudly with the mode
named.

On Windows there is no mode to read. What is there was measured rather than
assumed: a credential file under a default `%USERPROFILE%` inherits exactly
SYSTEM, Administrators and the owner -- no `Users`, no `Everyone` -- which is
what 0600 buys on unix, where root reads it anyway. Writing unsafe ACL code to
reach a state the file is already in would add risk, not remove it, so the
note in `write_atomic` records that measurement instead of promising work.

### Fixed

- The run log is created 0600 like everything else this tool writes, rather
  than 0664.

## 0.2.7

### `ccred log`

A scheduled run left nothing anyone could read. systemd captures stdout in the
journal and launchd can be pointed at a file, but Windows Task Scheduler
discards it entirely -- so on the platform where a job is least visible there
was no record at all. When a refresh quietly emptied a profile, the only
reason it could be explained was that someone happened to be watching at the
time.

Each run now appends a line: what every profile was decided to be, and how its
refresh window moved. Decisions and numbers only, never the detail text --
that is built from rendered error messages, and the rule here is to persist
error *kinds*, because a message can echo its input and that input can be a
token. The file rotates at a megabyte and keeps three generations.

### The sandbox was not a sandbox

Writing that found `log_dir` was derived from `LOCALAPPDATA` or
`XDG_STATE_HOME`, read from the process environment rather than from the
`Paths` it belongs to. Neither a test sandbox nor `CCRED_HOME` relocated it,
which means this project's own test suite had been appending its runs to the
real user's log file. Logs live under the ccred home now, with everything else
this tool owns, and a test pins that nothing is written outside the home it
was given.

## 0.2.6

### A failed probe says which kind of failure it was

"Did not move the refresh window" covered two situations that call for
opposite reactions: the probe was too weak and did nothing, or it did exchange
tokens and the server refused. Only the first is worth trying a harder rung
for; only the second means the account is in trouble. Comparing the store
revision separates them, which is what that method was written for and never
used for.

### Documented

- The README claimed nothing was published anywhere. The Scoop bucket is live
  and verified, and every release carries a `.deb` for amd64 and arm64; both
  now come first, with the registries named as waiting on account setup rather
  than implied to be missing.
- Why the scheduler passes `--if-older-than 48` and not the 72 the design
  sketch suggested: the shortest real gap between runs is 72 hours, so 72
  would sit exactly on the interval and could swallow a run it was meant to
  allow.

### Tested

Every source file now has tests. The last three to get them were the exit-code
mapping, the load contract, and the schedule spec -- respectively the thing
schedulers read, the thing that stopped the shell version destroying a
profile, and the two properties that decide whether a registered job can start
at all.

## 0.2.5

### A profile with a live access token is no longer spawned for

Measured on a live machine: a profile with twenty-one days of refresh window
and six hours left on its access token ran all three probes, moved nothing,
and was recorded as a failure with a backoff.

The refresh window only extends when Claude Code performs a token exchange,
and it only does that when the access token needs renewing. A profile whose
access token is still valid therefore cannot be refreshed at all -- the run
was judging a correct no-op by a yardstick that did not apply. It now waits
instead, which is a short wait: an access token lives about eight hours
against a schedule measured in days.

Without this, every recently saved profile collected spurious failures.

### Fixed

- `trust_dir` is gone rather than wired up. Connecting it would have meant
  marking a directory trusted in the user's own configuration on their behalf,
  to avoid a prompt that does not appear.

### Tested

- The exit-code mapping. A scheduler reads nothing else, so those numbers are
  a published contract, and the distinction this tool cares most about --
  "this machine is not set up" versus "your credentials are in danger" --
  could until now have been swapped without anything noticing.
- The journal recovery arms that decide whether an interrupted switch rolls
  back or is merely litter. Two of the five phases had never executed.
- The switch paths that degrade instead of failing: a profile deleted from
  under the pointer, and one saved before account details were captured.
- The helpers in `src/ui/render.rs`, the file the conventions name as the one
  place to audit for "no output may ever contain a token", which had none.
- The command tree, through clap's own `debug_assert`, which is the
  difference between finding a bad flag here and finding it when a user runs
  that command.

## 0.2.4

### `ccred restore <name>`

A copy of each profile's credentials has been written after every accepted
save since the beginning, and nothing ever read it. That stopped being
academic when a spawned Claude Code signed itself out and emptied a profile on
a live machine: the copy was the only thing that survived, and getting it back
meant editing files over SSH.

`restore` puts it back, and `doctor` now says which unusable profiles have a
copy worth restoring rather than sending everyone to `claude auth login`.

### A timeout that did not time out

The first test ever written for the probe runner found it. Killing the child
does not necessarily close the pipe -- if that child spawned one of its own,
the grandchild still holds the write end and the reader thread stays blocked
on it. Waiting for that thread then undid the deadline that had just fired: a
30-second process survived a 400-millisecond timeout. On a scheduled run that
is a job that never ends.

### Fixed

- A profile whose credentials a probe cleared is reported as needing a login,
  not as broken. They are readable again by then; what is gone is the server's
  willingness to refresh them.
- A missing `claude` is a configuration problem (exit 8), not an unsafe state
  (exit 7), and it no longer aborts the whole run -- the active profile's
  mirror does not use the binary at all. The message names `--claude-path`
  rather than a configuration file nothing reads.

## 0.2.3

**Upgrade from 0.2.1 or 0.2.2.** Both shipped a check that reported healthy
profiles as broken.

### The environment oracle is gone

0.2.1 added a check meant to prove the spawned `claude` had really used the
profile's credential store, by comparing the account it reported against the
profile's own. It reads that account from the cached `oauthAccount` in the
shared `.claude.json` -- which we deliberately leave shared -- so it names the
live account regardless of which store was read. On a real machine it declared
an untouched profile "ignored the credential directory".

No oracle is needed. Measured against claude 2.1.236: pointing
`CLAUDE_SECURESTORAGE_CONFIG_DIR` at empty credentials makes it report
`loggedIn: false`, while without the variable it is logged in. The variable is
honoured, so the existing window-movement test is the whole proof -- had
another store been read, the profile's own file could not have changed.

### A spent refresh token is now reported as needing a login

When a refresh token has already been used, Claude Code prints:

    Failed to authenticate: OAuth session expired and could not be refreshed

None of the phrases this looked for appear in it, so the profile was reported
as broken rather than as needing a login -- which is the one thing the user
can actually act on.

### Added

- `ccred refresh --force` tries every profile now, ignoring the backoff, the
  window threshold and the run-level rate limit. Those are right for a timer
  and wrong for someone who has just fixed what was broken and wants to watch
  it work. It does not clear `needs_login`: a human is genuinely required
  there, and spawning a process that cannot succeed only wastes a rate limit.

## 0.2.2

### A spawned `claude` can no longer empty a profile

This is written from an incident, not a review. A scheduled refresh on a
live machine spawned `claude` against a profile's own credential store; the
binary decided it was signed out and wrote an empty credential blob straight
over it. Every safety gate in this tool guards what *we* write -- that write
was not ours, so nothing stopped it. The profile survived only because a
last-known-good copy happened to be sitting beside it.

`refresh` now takes the store as it stands before spawning anything, and
after each probe checks whether it went from usable to unusable. If it did,
the good copy goes back, `needs_login` is latched and the run says so.
Restoring a token that later turns out to be dead costs nothing, because
`validate` reports it on the next run; not restoring costs the account.

### Fixed

- `save` and the refresh mirror take the credential store lock. Only `switch`
  did, and those two are what a timer runs -- so they are the likeliest to
  collide with a live session refreshing its token, and a read landing
  mid-write copies half of one token pair and half of the next.
- The refresh rate limit was armed by any run that finished, including one
  where every profile came back broken, turning a transient failure into two
  days of silence. Only a clean run suppresses the next one now.
- `publish-npm` upgraded npm in place, leaving the running npm without part
  of its own dependency tree. It takes npm 11 from Node 24 instead.
- The packaging trigger required the whole release run to succeed, but that
  run also carries the registry publish jobs, which fail while the accounts
  are unconfigured. It gates on the release existing instead.

## 0.2.1

### Refreshing could not have worked

`env_pairs_for` set `CLAUDE_CONFIG_DIR` alongside
`CLAUDE_SECURESTORAGE_CONFIG_DIR`. Only the second was wanted: it relocates
the credential store and leaves `.claude.json`, `projects/`, `sessions/` and
MCP config shared. The first moves the whole configuration, so every spawned
probe ran against an empty directory -- no config, no trust state for the
working directory, no MCP. A non-interactive run in that state has nothing to
refresh and may stop on a trust prompt instead.

The oracle that proved the environment took effect went with it.
`projectsDirectory` follows `CLAUDE_CONFIG_DIR`, so it can no longer answer
that question. The account Claude Code reports having authenticated as is
direct evidence instead.

### `switch` could destroy an account outright

The outgoing mirror is deliberately skipped when the live account is not the
one the pointer names -- which is exactly the case where those credentials are
stored in no profile. The next step overwrote them. Someone logged into an
account they had not saved lost it by running the command meant to organise
their accounts. Those credentials are now copied aside first, and the report
says where.

### Fixed

- `scopes` round-trips faithfully. With `skip_serializing_if` on an empty
  `Vec`, an input of `"scopes": []` re-serialised to nothing, the lossless
  guard correctly called that a dropped key, and every command touching such a
  file failed with exit 7.
- The release job's `--target` is gone. Supplying `target_commitish` makes
  GitHub demand `workflows: write` on top of `contents: write` whenever the
  released commit range touches `.github/workflows`, and `GITHUB_TOKEN` cannot
  hold that scope. This is the 403 that made every release be assembled by
  hand.
- `packaging` now chains off the release workflow. It triggered on
  `release: published`, which never fires for a release created with
  `GITHUB_TOKEN`, so no `.deb` or AUR package had ever been built.
- `publish-npm` pinned Node 22.14 and then installed `npm@latest`, which
  requires 22.22.2. It could never reach the registry.
- Both called publish workflows download release assets but were granted no
  `contents` permission.
- The Homebrew job is gated on its token existing, so an unconfigured tap no
  longer fails every release.
- The nfpm download was verified against a filename that was never written to
  disk, and the checksum grep also matched the `.sbom.json` line.
- Every action in `release.yml` is pinned to a commit, including
  `actions/attest`, which mints this tool's provenance.
- The Scoop manifest carried a placeholder hash and an `extract_dir` for a
  directory the archive does not contain.

## 0.2.0

### The refresh schedule registers itself

The save that creates a **second** profile now registers the background
refresh, because that is the moment an idle account starts expiring
unattended and the only moment someone is certainly watching. It never fires
on a re-save, so a schedule that was deliberately removed does not come back,
and `CCRED_NO_AUTO_SCHEDULE=1` opts out for provisioning. Registration can
never fail a save: the credentials are stored by then, and losing that result
because a scheduler refused would trade the operation that matters for one
that does not.

### Three ways scheduling was broken on Windows

**It could not be registered at all without administrator rights.**
`schtasks` answers `Access is denied` for `<LogonType>S4U</LogonType>` and,
independently, for any `<BootTrigger>`. Ordinary task creation needs no
elevation, so the feature was out of reach only because we insisted on the
better definition. Registration now asks for that first and falls back to
`InteractiveToken` without a boot trigger, which any user may create. What is
lost is the signed-out case, not reliability -- `StartWhenAvailable` still
catches up a missed run -- and `schedule status` says so.

**`schedule status` reported "installed, but disabled" on every machine.** The
task state was read from any `/V` row whose *value* was `Disabled`, and a
healthy task has two: `Idle Time` and `Delete Task If Not Rescheduled`. The
state now comes from `<Enabled>` inside `<Settings>` of the registered XML,
which is unambiguous and not localised.

**`doctor` never checked the schedule at all.** Every other finding reports on
a profile as it stands today, so a machine with nothing refreshing its idle
profiles passed cleanly and then lost an account a fortnight later.

### Output is rendered rather than printed

Every command was a column of unaligned `println!`. There is now a
presentation layer: aligned tables, meters for the remaining refresh window,
colour that says how worried to be, and a styled help screen.

Colour is `anstream`'s job. It strips escapes when stdout is not a terminal,
which keeps systemd, launchd and Task Scheduler logs free of them, and it
honours `NO_COLOR` and `CLICOLOR_FORCE`. Unicode drawing characters are
opt-in: a terminal not known to handle UTF-8 gets ASCII, because a tool people
run when something is already broken must not add mojibake to the problem.
`CCRED_UNICODE=1` or `0` overrules the guess.

Reports also carry more than they used to: when a profile was last synced, the
plan it is on, and the access token's remaining life in hours -- it lives about
eight hours, so counting it in days always read as "0 days left". `--json`
keeps the raw API values.

### Fixed

- `packaging.yml` contained a GitHub expression with no right-hand operand.
  The YAML parsed, so nothing local caught it, but GitHub refused to load the
  workflow. No `.deb` or AUR artifact had ever been produced by CI.
- The Windows install command no longer passes `-NoProfile`. It was added on
  the strength of an A/B that does not hold up: `Trojan:Win32/Commando.A!ml`
  is a cloud-backed verdict on the download-cradle command line and is not
  stable over time. See `docs/install.md`.

## 0.1.0

First release. Saves each Claude Code account as a named profile, switches the
active one, and keeps idle profiles from expiring by running the real `claude`
binary against each profile's own credential store. It never contacts an OAuth
endpoint itself.
