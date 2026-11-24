# Changelog

Notable changes to varde and org-varde.

## Unreleased

### Fixed

- A config written for 1.6.0-beta2, whose profiles name `background`
  arguments, loads whole again: the key is read and ignored, the
  sandbox and each agent's grant saying it now.
- `--escalate` moves a loop up when two rounds fail the same way, even
  when each changed the code; it stopped only on the same failure with
  nothing changed, so a model rewriting its way past a reviewer never
  escalated. Two review (or verifier) rejections count as the same
  failure, and past its last profile a loop stays there.

## 1.6.0-beta3 - 2026-11-23

Still a beta. Background sessions now run in a sandbox, a loop carries
on while you are in it, and large work can fan out to several sessions.

### Added

- The sandbox: background sessions (rounds, trail steps, schedules) and
  every check run in the system's own sandbox, bubblewrap on Linux and
  `sandbox-exec` on macOS, around any agent. Inside, the agent asks
  nothing, and it can write only its checkout, its session files,
  temporary files and build caches, and commit (unsigned). It
  cannot read your logins (ssh, gpg, cloud CLIs, gh, git credentials,
  browsers, tokens in the environment), reach what runs things outside it
  (your service manager, tmux, an ssh agent, docker), or touch varde's
  store: its varde hands each command to the varde outside, which runs
  only what a session may (hooks, `note`, `recall`, `new`, reading). `varde sandbox [NAME] [-- CMD]` shows one or runs
  a command in it, and `varde doctor` checks it starts. Built in:
  `default`, `push` (shows git's logins; a `draft-pr` step gets it) and
  `off`. Yours go under `[sandboxes.NAME]` with `show`, `write`, `env`
  `pass` and `deny`. A task picks one with `new --sandbox` or `set --sandbox`,
  and a recipe step with `sandbox`.
- In a sandbox a session's commits go to a quarantine of their own, and
  varde brings them into the repository re-hashed when it is done, as a
  push is received: nothing it writes can change `main`. A repository it
  leaves inside its checkout, however deep, bare or hidden, is set aside
  (`*.varde-held`) and flagged, and a link out of the checkout to where
  a sandbox writes (a cache, temporary files, a git directory) is
  removed. Pipes it leaves for git to wait on are removed, and git varde
  runs stops after two minutes. Hooks kept in the checkout (husky's) are
  read-only to it. `varde doctor` asks for `safe.bareRepository =
  explicit`.
- What a session with nobody at it may do is one grant, which each
  agent's adapter says its own way. With no sandbox, Claude may change
  files in its checkout, commit and run its check. pi cannot be kept
  from a command or asked before a tool, so it runs in the background
  only in a sandbox that denies nothing, or with the sandbox `off`; else
  the loop stops, saying why.
- A check's guard is read-only to background sessions in a sandbox:
  they cannot edit the tests they are checked by, rather than being
  flagged after they did.
- Checks: `--verify "FILE or text"` (and `verify` in a recipe's check)
  has an agent verify the work once the command passes: a fresh session
  on the review profile, in the task's sandbox, with its tools and your
  skills ("run it and use /verify-app"). Its last line, PASS or FAIL,
  decides, and what it touched to look is put back.
- The Claude plugin brings a skill, `varde-loops`: asked to loop work,
  verify it, split it or make it a recipe, a session sets it up with
  varde. pi can link it from `cli/plugin/skills/`.
- `varde follow <task>` (`f` on both boards) watches a task's sessions
  at work, read-only: what each says, its tools, each check, the next
  round.
- Opening a looping task takes the loop over instead of pausing it.
  Each of your turns that changes files is checked as it ends, and a
  failure comes back into your session (`↺ 2/3 with you`). The loop
  goes on in the background when you leave.
- Recipes: `epic`, where a plan you approve becomes a trail of pieces
  built at once, each in its own worktree until green, then merged and
  checked whole; and `change-plan`, which looks at a live system from a
  read-only sandbox and writes `CHECKLIST.md` for you to apply. A step
  whose `output` is `trail` writes `trail.toml`, and approving it makes
  those tasks. `feature` gains a simplify step. `varde new --recipe`
  takes `--after`.
- A step after several others is told to merge their branches.

### Changed

- The store moves to schema 12.
- Scheduled tasks now run in the sandbox too. One that needs a login it
  hides (`gh`, `az`, ssh) needs `varde set <task> --sandbox push`, or a
  sandbox of yours that shows it.
- The heartbeat carries your agent's provider settings
  (`CLAUDE_CODE_USE_VERTEX`, `ANTHROPIC_*`, `AWS_*`, `GOOGLE_*`,
  `CLOUD_ML_*` and the proxies) and a `PATH` without temporary entries.
  It says which secrets it left out, and `doctor` says when it lacks
  what your shell has.
- `doctor` says when a profile's alias may name a model your Vertex or
  Bedrock project does not serve, and how to pin it
  (`ANTHROPIC_DEFAULT_SONNET_MODEL`).
- A profile's `background` arguments are gone: the grant replaces them.
  Remove `background = …` from your `[profiles]` (a profile with it is
  ignored, with a warning), and `["--permission-mode", "auto"]` from
  `[agents.claude] background`, which the grant now overrides. The
  agent's `background` arguments come before the grant, which wins
  where they set the same thing.
- A rubric given as a file is read when you set it and kept as text. One
  set as a file in 1.6.0-beta2 is kept as its path: set it again.
- A recipe whose check command comes out empty is refused, rather than
  run on a review alone.
- `varde after` a task with no check waits on your approval of it, which
  can happen, rather than on its check, which cannot.
- `varde show --json` gives the loop's and the check's words, which
  Emacs shows as they are.

## 1.6.0-beta2 - 2026-11-23

A beta: loops and trails are new, and with the steward they may change
or go before 1.6.0. Nothing else depends on them.

### Added

- Checks: `varde check <task> -- CMD` says how varde knows a task's work
  is good. varde runs it itself, in the task's checkout, after each of
  its sessions and on `--now`; `--review` adds a review of the diff by
  another model, `--rubric` what it judges by, `--again N` passes in a
  row, and `--guard` paths a session must not change. `varde approve`
  is you saying it is good.
- Loops: `varde loop <task>` runs a task in fresh background sessions
  until its check passes, each told its cairn and how the last check
  failed. It stops when the check passes, after `--max` rounds or
  `--minutes`, when two rounds fail the same way with no change (after
  moving up `--escalate`'s profiles), when a round asks you something,
  or when a round touches the guard. Opening the task pauses it.
- Trails: `varde new --after <task> [--approved]` and `varde after` make a
  task wait on others; `varde trail <task> [--start]` draws them and sets
  them going. A step starts in the background once what it waits on is
  verified, or approved.
- Profiles: an agent, a model and an effort, by name (`quick`, `build`,
  `deep`, `review` ship; yours under `[profiles]`), with `--profile` on
  `new`, `start` and `attach`, and `varde set --profile`. `varde
  profiles` lists them.
- Recipes: loops and trails written down once, in TOML. Five are built
  in (`fix-until-green`, `bugfix`, `feature`, `draft-revise`,
  `research`); yours go in `~/.config/varde/recipes/`. `varde recipe
  list|show|check|edit|guide`, and `varde new "…" --recipe NAME --var
  k=v --set step.key=value --start`. `varde recipe guide` is written for
  agents too, and every session is pointed to it.
- Both boards: `L` loop, `c`/`C` set or run the check, `O` its output,
  `A` approve, `t` the trail, `M` the profile, `r` recipes. Rows say
  where a loop or trail stands (`↺ 2/3 ✗ 1 failed`, `✓ verified in 2`,
  `⧗ your approval of abc123`).
- Emacs: a Loop section in the view, a round a line (RET opens it, `O`
  what its check printed); a trail buffer; a recipes buffer; and
  Profile, After, Recipe and Vars in the new-task form.
- The steward has a playbook for loops and trails (`varde steward guide
  loops`).

### Changed

- The store moves to schema 11. Earlier versions refuse it: keep a copy
  of `~/.local/state/varde/varde.db` if you may go back to 1.6.0-beta1.
- `varde tick` also starts loops' rounds and trails' steps, is woken
  when a session, a check or a cairn update ends, and runs at most
  `loops.parallel` (3) background sessions at once. Loops need the
  heartbeat: `varde schedule --install`.
- A profile may carry its own `background` arguments. Claude's `auto`
  permission mode refuses every write on haiku: `varde doctor` says when
  a haiku profile would run under it.
- A task's own session may not `varde approve`.

## 1.6.0-beta1 - 2026-10-31

A beta: the steward is new, opt-in, and may change or go before 1.6.0.

### Added

- The steward (beta, opt-in): `varde steward on` gives you an agent
  session that manages varde with you. It files tasks, reviews what is
  stuck, reports, finds past work, and routes what you forward to it.
  `S` on either board goes to it, and offers to turn it on.
- `varde trust <task>` (or `T` on a board) hands a task to the steward
  for a while: its permission requests and questions wait on the
  steward's answer, and on yours as before. Pushing, merging, closing
  tasks and writes outside the worktree always come to you.
  `--allow 'Bash(cargo test*)'` lets the hook allow what a rule names.
- `varde say <task> TEXT` reaches a task in any state: an open session,
  working or waiting, gets it now; with none, the task carries on in
  the background. Sent from a session, the answer comes back to it as
  a message when the task's turn ends. `varde said` reads what a
  session said last, and `varde stop` ends a task's sessions.

### Changed

- The store moves to schema 10. Earlier versions refuse it: keep a
  copy of `~/.local/state/varde/varde.db` if you may go back to 1.5.
- The Claude plugin gives `PermissionRequest` a 30-minute timeout and
  hooks `AskUserQuestion`. Both do nothing unless a task is handed to
  the steward. The pi extension takes in what `varde say` sends.
- Every session is told how to reach another task with `varde say`.

## 1.5.3 - 2026-10-30

### Changed

- Done holds only tasks closed today. A done task leaves the board the
  next day whether or not its work landed, so notes and experiments no
  longer wait there until you drop them. `varde list --all` still
  shows them.
- A day after `done`, a worktree whose work has not landed is put away
  as `drop` does it: kept under `refs/varde/discarded/`, with the
  worktree and branch removed. Starting the task brings the work back.
  A worktree holding what no ref can keep (a changed `.worktreeinclude`
  file, commits on a detached HEAD) stays.

## 1.5.2 - 2026-10-22

### Fixed

- A done task whose work was cherry-picked, rebased or squashed into
  its base by hand, not through a PR, now counts as landed. Its
  worktree and branch are removed and it leaves Done. Before, such
  tasks stayed in Done for good.

## 1.5.1 - 2026-10-20

### Changed

- `varde start` no longer checks out a new worktree's submodules. Each
  new worktree fetched every submodule again, which held a start up
  for minutes; the session now fetches the ones it needs.

## 1.5.0 - 2026-10-18

### Added

- org-varde opens sessions in ghostel. `org-varde-terminals` says
  which terminals you prefer, in order; the first installed is used,
  and `term`, which can't answer fish 4's startup queries, only when
  none is. ghostel runs through Emacs's PTY, not its native one, so
  org-varde can still read the terminal's tty and whether it is busy.

### Changed

- org-varde prefers ghostel to vterm when both are installed. Set
  `org-varde-terminals` to `'(vterm ghostel)` to keep vterm.

## 1.4.1 - 2026-10-18

### Fixed

- A background run is let off the user's look only if it changed
  nothing: it is told that anything it edited or committed, however
  routine, is for the user to see. A run had judged its own commit not
  worth a look.
- org-varde: going to a vterm shows its latest output, at the cursor; a
  task started in the background had opened at the top its workspace
  was laid out with.
- org-varde: TAB in a task's view, off a section heading, goes to the
  next task that needs you; it had failed with a `wrong-type-argument`.
- org-varde: `org-varde-recall` is autoloaded, as its cookie meant.
- org-varde: in a `term`, any prefix under `C-c` that holds one of
  org-varde's commands is kept from term's escape, not only `C-c v`.
- The board no longer exits when a project picker is opened in a
  terminal one or two rows tall.

## 1.4.0 - 2026-10-17

- One project picker on both boards, for every question about a
  project: known projects first, the likeliest picked, and typed
  directories completed. In org-varde it's a `completing-read`.
- The edit form (`e`) has a `Project:` field; changing it moves the
  task, as `m` does.
- The board's new-task form matches org-varde's: it opens in the
  context and has `Title:`, `Project:` and `When:` fields.
- `varde projects --json` lists the projects varde knows.
- The new-task form no longer binds C-j and C-k; TAB and S-TAB go
  between its fields. On the board, C-j is Enter.

## 1.3.2 - 2026-10-17

- The new-task form opens in the context, not the Title.
- Fixes to the form under Doom/evil, and a task is no longer started
  twice when its key is pressed again.

## 1.3.1 - 2026-10-16

- Fixes to the new-task form under evil-org.

## 1.3.0 - 2026-10-16

### Added

- `agents.<agent>.background` in the config: extra arguments for
  scheduled sessions, e.g. `["--permission-mode", "auto"]`.
- `varde schedule <task> --now` runs the next start at once.
- A repeating task's row says how many runs wait unread (`↻ 3 unread`)
  and when a start is held up (`↻ due`, `↻ late`).
- The view lists every session a schedule ran.

### Changed

- `done` on a repeating task ends the run, not the series; end the
  series with `varde schedule --off`.
- Every unopened background run stays unread until opened.
- Recall leaves out lines you removed or rewrote.
- The project cairn is yours: closing a task no longer promotes lines
  into it.
- org-varde: the new-task form's context is written in org-mode.

## 1.2.0 - 2026-10-15

### Added

- Scheduled and repeating tasks: `varde schedule <task> RULE`, `varde
  new --when`, or `When:` in the forms. `varde schedule --install`
  sets up the timer that runs `varde tick`.
- `org-varde-new-here` copies a heading's `SCHEDULED:` into `When:`.

### Changed

- org-varde writes a task in a form with `Title:` and `Project:`
  fields above the context. `C-u C-c C-c` no longer asks for another
  project: change the Project field.
- The board's form: Tab and Shift-Tab go between all fields.

### Fixed

- On macOS the Emacs board no longer misses updates while a session
  runs.

## 1.1.1 - 2026-10-13

- Fixes: two starts of one task at once, and org-varde on Emacs 29/30
  loads `org-element` itself.

## 1.1.0 - 2026-10-13

### Added

- Install with Homebrew: `brew install smebbs/tap/varde`.
- Cairns shown and edited a line at a time: the board's view (`v`),
  `varde show`, the Emacs view, and `varde edit --add/--set/--rm/--restore`.
- `varde --popup` for the board in a tmux popup, and `varde next` to
  go to the next task that needs you.
- Model-written titles for tasks varde titled itself (`[memory] titles
  = false` turns them off).
- The board keeps to the project it's opened in; `.` shows every one.
- `[keys]` in the config rebinds the terminal board's keys.
- `u` reopens the tasks last closed on a board.
- org-varde: `varde:` links (`org-varde-insert-link`), which show their
  task's status; `org-varde-new-here` makes a task from a heading or
  region; DONE on a heading offers to close its tasks; a `#+BEGIN:
  varde` block lists tasks for a review; `org-varde-next-needs-you`.

### Changed

- **Keys that moved.**

  | What | 1.0 | 1.1 |
  |---|---|---|
  | Park (terminal) | `p` | `P`, as in Emacs |
  | Project cairn (terminal) | `V` | `v`, then `]`: the view's Project tab |
  | Edit a cairn in `$EDITOR` (terminal) | `e`, `E` for the project's | `E`: the task's, or the project's on the Project tab |
  | Edit title and context (terminal) | `e`, on a task without a cairn | `e`, always |
  | Write a new task in `$EDITOR` (terminal) | C-g in the form | C-x C-e or Alt-e; C-g puts the form away |
  | Refresh (terminal) | `g` | C-l |
  | First task (terminal) | Home | `gg`, and Home |
  | Fold Parked (terminal) | Tab | `z`, on any group |
  | Tab (both boards) | fold | the next task that needs you |
  | RET (Emacs board) | start in the background | go to the session; `s` starts in the background |
  | View a task (Emacs) | `s` | `v`, or `K` under evil |
  | Project cairn (Emacs) | `S` | `]` in the task view: its Project tab |
  | Edit the whole cairn (Emacs) | `e` in the view, `E` for the project's | `E`; `e` edits the line at point |
  | Fold a group (Emacs) | TAB | `z` (`za` under evil); TAB on a group's heading |
  | First / last task (Emacs, evil) | none | `gg` / `G` |

- Every group is always shown, in the same order, on every board.
- "Drop" is the word for throwing a task away everywhere.
- `X` in the terminal view drops the task (`x` there removes a line).
- `varde new` and `varde set` take `--context`; `--body` still works.
- org-varde: `N` runs the configured agent, not always claude.

### Removed

- **org-varde no longer makes org headings into tasks.** Gone: the
  `VARDE_TASK` property, keyword changes closing tasks, archiving,
  `org-varde-done`, the board's `f`, `l` and `o`, the view's `h`, and
  the options `org-varde-directories`, `org-varde-start-goes`,
  `org-varde-park-keywords`, `org-varde-drop-keywords`,
  `org-varde-settle-delay` and `org-varde-context-words`.
  `org-varde-start` and `org-varde-link` are obsolete aliases.
  **Migrating:** `M-x org-varde-convert-links` (`C-u` for every org
  file) puts a link to each heading's task in its text; save the files
  after.

## 1.0.3 - 2026-10-12

- Tab completes directories in the board's project prompts.
- `e` on a task without a cairn edits it in the new-task form.

## 1.0.2 - 2026-10-12

- C-g in the board's new-task form writes the task in `$EDITOR`.
- `n` on the org-varde board writes the task in a buffer of its own.
- org-varde starts a task without blocking Emacs.

## 1.0.1 - 2026-10-12

- `s` in the board's recall pane starts the picked task.

## 1.0.0 - 2026-10-12

The first public release.

- **Tasks** over Claude Code and pi sessions: start, resume, fork,
  park, close and throw away, with live status from the agents' hooks.
- **The board** (`varde`): a terminal UI that groups tasks by what they
  need from you, with a window per session inside tmux.
- **Projects:** a task belongs to a repository, and can move to
  another (`varde set --project`, or `m` on either board).
- **Worktrees** per task that are removed once their work has landed,
  and kept under `refs/varde/discarded/` when thrown away.
- **Memory:** a cairn per task and a project cairn, updated in the
  background, handed to every session, searchable with `varde recall`,
  and measured with `varde eval`.
- **org-varde**, the Emacs client: org headings as varde tasks, and a
  board where RET on a region starts every task in it.
