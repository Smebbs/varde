# org-varde for Emacs

**org-varde** (the `.el` files at the repository's root) is varde's Emacs client. Tasks
live on the board; your notes live in org; the two meet through
`varde:` links, and nothing is kept in step behind your back.

- **The board.** Watch every session in one place, go to them, start
  and close tasks.
- **Links in your notes.** `org-varde-insert-link` puts in a link to
  a task you pick by its title. `[[varde:k2f0g3][Fix login]]` opens the
  task's view, and shows after itself where the task stands
  (`● working`, `○ your turn`, `✓ done`).
- **A task from a note.** `org-varde-new-here` makes a task from the
  heading at point, or the region, and links it there.
- **Finishing.** Marking a heading DONE offers to close the tasks it
  links.
- **Reviewing.** A `#+BEGIN: varde` block lists tasks with their
  cairn's Status.
- **Memory.** Read a task's cairn, what its sessions learned, edit it
  a line at a time, and search earlier work.
- **What it writes to org.** Only the links you ask for: the one
  `org-varde-new-here` puts in, and those `org-varde-convert-links`
  makes. It never saves a file for you.

## Install

1. **varde and its hooks.** See [Install](guide.md#install) in the
   guide.
2. **org-varde.** It needs Emacs 29.1 or later. It opens sessions in
   [vterm](https://github.com/akermu/emacs-libvterm) when that is
   installed, and in the built-in `term` otherwise. With Doom's
   workspaces it gives each task a workspace.

On Emacs 30 or later:

```elisp
(use-package org-varde
  :vc (:url "https://github.com/Smebbs/varde" :rev :newest)
  :demand t    ; the mode must be on before you mark a heading DONE
  :after org
  :config (org-varde-mode 1)
  :bind (("C-c v b" . org-varde-board)
         ("C-c v i" . org-varde-insert-link)
         ("C-c v t" . org-varde-new-here)
         ("C-c v n" . org-varde-next-needs-you)
         ("C-c v r" . org-varde-recall)
         ("C-c v v" . org-varde-show)))
```

On Emacs 29, install it once with
`M-: (package-vc-install '(org-varde :url "https://github.com/Smebbs/varde"))`,
and use the same form without the `:vc` line.

On Doom:

```elisp
;; packages.el
(package! org-varde :recipe (:host github :repo "Smebbs/varde"))

;; config.el
(use-package! org-varde
  :after org
  :config (org-varde-mode 1))

(map! :leader :prefix ("l" . "agent")
      :desc "Board"           "b" #'org-varde-board
      :desc "Link a task"     "i" #'org-varde-insert-link
      :desc "New task here"   "t" #'org-varde-new-here
      :desc "Next needing me" "n" #'org-varde-next-needs-you
      :desc "Recall"          "r" #'org-varde-recall
      :desc "View task"       "v" #'org-varde-show)
```

`org-varde-use-worktree` (`ask`, `always` or `never`) says whether a
task's first run gets a worktree of its own; `ask` names the task and
its project (`Start "Fix login…" in a worktree of its own (app)? `);
`C-g` cancels the start.

`M-x org-varde-doctor` checks the installation.

## In org

The board is where tasks are made, started and closed. Org is where
you write about them: a plan, meeting notes, a weekly review. A
`varde:` link is the only thing between the two.

**Links.** `org-varde-insert-link` (`C-c v i` above) asks for a task by
its title, with completion (vertico, ivy or Emacs's own; any part of
the title matches, in any case, and so does the task's short id; `SPC`
types a space, as in the title), and puts in
`[[varde:k2f0g3][Fix the login bug]]`: open tasks first, then closed
ones, then the projects' cairns, each with where it stands, its project
and how long ago. A project's cairn is linked as
`[[varde:project:~/src/app][app · project cairn]]`. In a Markdown
buffer it puts in `[Fix the login bug](varde:k2f0g3)`, elsewhere
`varde:k2f0g3`. It offers what was last fetched, and fetches every
task again in the background for next time; it waits on varde only when
nothing was fetched yet. Following a link (`C-c C-o`, or `RET` with
`org-return-follows-link`) opens the task's view, or the project's
cairn, and `M-x org-varde-show` on a link does too. From the board,
or a task's view, `y` copies a link to the task at point, for `C-y`
(`p` under evil), and `C-c l` (`org-store-link`) there, or in a task's
terminal, stores one for `C-c C-l`; on the view's Project tab both link
the project's cairn. Exported (HTML, LaTeX, plain text), a link is its
description, or the task's short id, as plain text.

**Where a task stands.** A `varde:` link shows its task's status after
itself, with the board's glyph and colour: `● working`, `● your turn`
(`○` once you have looked), `● needs input` (red), `○ open`, `✓ done`,
`‖ parked` or `✕ dropped`;
nothing for a task varde does not know, a project's cairn, or a link in
a table (a review's has a column for it). It is drawn from the rows the
board last fetched, so drawing runs nothing; a link to a task the board
no longer shows (one closed before today, or dropped) has every task's
row fetched once, in the background, and is drawn again. While
`org-varde-mode` is on, a link follows its task within a second, in
every buffer showing one. A doc with 200 links to a board of 1 000
tasks draws in about a millisecond more than without them.

**A task from a note.** `org-varde-new-here` (`C-c v t` above) on a
heading opens the board's new-task form with the heading's title as its
Title and, as its context, the heading's text (without its drawers,
planning, clocks and logged state changes), then a line saying where
it came from:

```
Title:   Fix the SSO redirect
Project: ~/src/app
── context ──────────────────────────────
It loops after the session expires.

From: [[file:/home/me/notes/auth.org::*Fix the SSO redirect][auth.org › Auth › Fix the SSO redirect]]
```

With a region, its first line is the title, the rest the context, and
the From line names the file and line. The title leaves out a list
item's bullet, number or checkbox (`- [ ] Fix the loop` is `Fix the
loop`), and a region that starts on a heading takes its title as org
shows it, without stars, keyword, priority or tags. A buffer with no file has no
From line. On a heading whose text already links an open task, it asks
first: `This heading links "Fix the SSO redirect": go to it? (n makes a
new task)`; `y` goes to its session, as `RET` on the board does. Edit it as you like; `C-c C-c`
(`:w` under evil) creates the task, and `C-c C-k` puts it away as `n`
on the board does; left as it was filled in, it is dropped, and a draft
you changed comes back with `n` as the board's own, linked nowhere. Its
Project starts as the project the heading's
`VARDE_PROJECT` names, its own or inherited (`#+PROPERTY:
VARDE_PROJECT ~/src/app` high up in the file sets it for all), else
as the repository the file is in, once that has tasks; otherwise it is
empty, and asked for on create. Once made, a link to it goes into the heading's text, as its
first line below the drawers, or on the line below the region: below a
table, block, drawer or list the region ends in, never inside one, and
in the heading's text when the region ends on a heading or its
planning. The file is left unsaved. In a buffer that is not org, the
link goes to the kill ring instead, as `org-varde-insert-link` would
write it there (Markdown's form in a Markdown buffer). Nothing starts until you press
`RET` on the board.

In the task's view, the From line shows as `From  auth.org › Auth ›
Fix the SSO redirect`; `RET` on it goes there. Agents read the same
line in the task's context, and can open the file.

**Marking a heading DONE** offers to close the open tasks its own text
links (the heading and the text under it, not its children's):
`Close "Fix the SSO redirect" (and 1 more) as done? ` (a link to a task
not fetched yet has every task fetched first, once). `y` closes them
as the board's `d` does, asking about a busy session, `n` leaves them.
`org-varde-close-with-heading` set to `always` closes them without
asking, and `never` never asks. A repeating heading asks nothing. It
goes one way: closing or reopening a task never changes the heading.

**A review.** A dynamic block lists tasks, each with where it stands,
a link to it, its project, its cairn's Status (cut to 50 columns, with
`…`) and the day it closed:

```
#+BEGIN: varde :project ~/src/app :closed 7d
|   | Task                         | Project | Status         |     Closed |
|---+------------------------------+---------+----------------+------------|
| ○ | [[varde:kvdcpq][Write onboarding docs]]        | app     | Outline agreed |            |
| ✓ | [[varde:hxj30t][Remove the old feature flags]] | app     | Flags removed  | 2026-10-13 |
#+END:
```

`C-c C-c` on the `#+BEGIN:` line fills it, asking varde then. `:project
DIR` keeps to one project (default `all`), `:state` takes `open`,
`done` or `all` (the default), and `:closed 7d` leaves out tasks closed
more than 7 days ago (`24h`, `2w`; a bare number is days). A value it
cannot read is refused, naming the ones it takes, and a project with no
task says `No tasks in ~/src/app` in place of a table.

**From headings to links.** An earlier org-varde made each task an org
heading, with a `VARDE_TASK` property, and kept the two in step. That
is gone (the design's Q31 says why). `M-x org-varde-convert-links`
turns each such heading in the buffer (`C-u`: in the agenda files and
every org file under `org-directory`) into a heading with a link: the
property goes, and `[[varde:k2f0g3][Fix the login bug]]` becomes the
first line of its text. varde forgets the heading (`varde unlink`), so
the task is the board's again. It says how many it changed, and saves
nothing; a file it opened and left unchanged is closed again. Until then, `org-varde-show` on such a heading still shows its
task.

## The board

**On the board,** the header shows the keys you need most
(`RET go  TAB needs you  v view  n new  d done  ? help`, `K view` under
evil), then the project it keeps to (said shorter in a narrow window,
so `? help` is never cut). `?` opens a menu of everything, which fits
80x24, headed by what each group holds and what `d` and `P` ask of a
busy session; under evil it names evil's keys (`K`, `za`, `gr`, `gg`,
`G`). The board looks as the terminal's does: every group is always
there, in the same order, with no blank lines between them; an empty
one is a quiet line (`Working  0`), so nothing jumps as tasks move.
A row is its glyph, its title, and at the right what else there is
to know, its short id last and quiet; in Needs you the group says it
is your turn, so its rows do not. Parked starts folded. While the
board is in sight its ages move on each minute.

| Key | Does |
|---|---|
| `RET` | Go to the task's session, starting or resuming it first if needed. A done, parked or dropped task asks first, as starting it reopens it. On a group's heading, fold or unfold it. `R` (or `C-u RET`) starts a fresh conversation, `F` (or `C-u C-u RET`) forks. |
| `s` | Start or resume it in the background and stay on the board; `RET` then goes to it. The `?` menu lists it; the header leaves it out. |
| `TAB`, `S-TAB` | Move to the next (previous) task that needs you, round to the first, in the order varde gives them; a folded Needs you opens. On a group's heading `TAB` folds it, as `z` does on any line of a group. |
| `v` | View the task: its cairn first, and its project's cairn on the view's other tab (below). The view opens below the board, most of the frame's height; `display-buffer-alist` can put it elsewhere. |
| `=` | The task's changes, as Emacs shows them: `magit-status` where magit is installed, else `vc-dir`, in its worktree while it has one, else in its project. |
| `E` | Edit the task's whole cairn as text (below). |
| `n` | New task, in a form of its own, as a mail is written in `message-mode`: `Title:`, `Project:` and, below the line, the context. `RET` goes from a field to the next, and `TAB` on the Project picks its directory; the names and the line cannot be changed, and evil's `cc`, `dd` and `^` keep to a field. Leave the Title empty, as it says in grey, and varde cuts a title from the context, and a model names the task in the background; an older varde that wants a title says so, and the draft stays. The Project starts as the project the board keeps to; left empty, `C-c C-c` asks for it, offering that of the task at point. `C-c C-c` (or `C-x C-s`, `:w` under evil) creates it. `C-c C-k` (`later`, the header says) puts it away, and the next `n` brings it back, however often; to drop it, clear it first. Either way the windows are as they were, and once created the board is on the new task. Nothing starts until you press `RET` on it. |
| `e` | Edit the task's title and context in the same form, without its Project (`m` moves a task; `C-c C-c` saves). |
| `d`, `P`, `x` | Done, park, drop, each said once it is (`Done "…"; u reopens`, `Parked "…"; u reopens`). `x` asks first ("Drop …?", or once for a region: "Drop 3 tasks?"), any work in its worktree kept under `refs/varde/discarded/`; `d` and `P` ask only about a busy session, as above, and `P` on a done or dropped task asks first, as `RET` does. Point stays on the line, which then holds the next task, as on the terminal board. |
| `u` | Reopen the task last closed here with `d`, `P` or `x` (all of them, for a region), back in Open, and put point on it; nothing starts. In a view, `u` first undoes a line removed or a Status handed back there. |
| `a` | Accept an Inbox task (`Accepted "…"`). On a task not in the Inbox it says so; on a region it accepts the Inbox tasks in it and says how many it left. |
| `m` | Move it to another project, asking for the directory with its project filled in. varde's refusal (an active worktree, a live session) shows on the message line. |
| `y` | Copy a link to the task, `[[varde:k2f0g3][Fix the login bug]]`, for `C-y` (and `C-c C-l`) to put in a note. |
| `/` | Recall, in the board's project first (`C-u /`: every project). Its first line says what was asked and where it looked; `.` asks again in every project, and again here, as the terminal's `.` does. `j`/`k` (`n`/`p`, `C-n`/`C-p`) go between hits; a project cairn's hit is named `app · project cairn`, and `(+2 more in this task)` follows a hit with more like it; `RET` on one views its task, or opens the view on the project cairn it is from, in the hits' window, where `q` brings the hits back; `s` starts its task, staying there. A snippet that wraps hangs under itself, and a yellow `?` ends one about code that is gone. |
| `.` | This project, or every project. |
| `N` | A session of your own, running the agent varde is set to start (`org-varde-agent-command` overrides it). |
| `g` | Refresh. `q` quits. |

To move: `j`/`k`, `C-n`/`C-p` or the arrows between tasks, `{`/`}` or
`M-p`/`M-n` between groups, `M-<`/`M->` to the first and last task. A
folded group's heading is a stop too; an empty group's line is not,
and has nothing to fold. A group you folded that empties opens again
as varde shows it, so tasks that come back to it are not hidden.
`d`, `P`, `x`, `a` and `s` act on
every task in the region, so a full Inbox is cleared in one go. `RET`
on a region starts each open task in it in the background, as `s`
does; done, parked and dropped tasks are skipped. The worktree
question is asked once for them all; each project question names its
task and offers your last answer.

The keys are the terminal board's wherever Emacs allows, so moving
between the two takes no relearning. They are in
`org-varde-board-mode-map` and `org-varde-show-mode-map`; rebind them
there. **Under evil** those keys win in normal state too. Where the
host has the key already, it differs:

| Terminal | Emacs | Emacs with evil | Why |
|---|---|---|---|
| `gg` / `G` | `M-<` / `M->` | `gg` / `G` | `g` refreshes in Emacs's special buffers. |
| `C-d` / `C-u` | `C-v` / `M-v` | `C-d` / `C-u` (`C-u` with `evil-want-C-u-scroll`, as Doom sets it) | `C-u` is Emacs's prefix argument. |
| `C-l` | `g` | `gr` | `C-l` recenters in Emacs. |
| `z` | `z`, or `TAB` on a heading | `za`, or `TAB` on a heading | `z` is evil's fold prefix; `TAB` folds as in org. |
| `Esc` | `q` | `q` | `Esc` is Emacs's meta prefix. |
| `space` marks | the region | `V`, then `j`/`k` | Emacs selects with the region. |
| `v` views | `v` | `K` | `v` and `V` are evil's selections; `K` is its "look up the thing at point". |
| `v` or `Esc` closes the view | `v` or `q` | `K` or `q` | The key that opens the view closes it. |
| `J` / `K` in a view: next / previous task | `J` / `K` | `C-j` / `C-k` | Under evil `K` closes the view; `C-j` / `C-k` step through items as in evil-collection's lists. |
| `j` / `k` in a view: next / previous line | `n` / `p`, and `j` / `k` | `j` / `k` | `n` / `p` move by line in Emacs's other special buffers; on the board `n` is still a new task. |
| (none) | `y` copies a link to the task | `y`, `p` puts it in | A `varde:` link is for org; the terminal has no use for one. |
| `s` starts in the background inside tmux only | `s` | `s` | Emacs opens every terminal itself, so it can always start one without going to it. |
| (no Git section) | the view ends with Git | the same | Emacs reads it from varde a moment after the view opens; the terminal's board row has its label. |

`V j RET` starts the selected tasks, as do `s`, `d`, `P`, `x` and `a`
on them. The buffers you write in (a new task, a task's title and context,
a whole cairn) start in insert state, as `org-capture` does; `:w`,
`:wq` and `ZZ` save, `:q` and `ZQ` cancel, as their header lines say.

The board keeps to the project of the buffer you open it from, once
that project has a task on the board. The header line names it and
says how many tasks are in other projects, and how many of those need
you. `.` switches between this project and every project. A task you
make or move into another project is shown all the same: the board
then shows every project. Opened from
a buffer outside a repository, it shows every project. Opened from an
org buffer, it keeps to the project the entry at point's
`VARDE_PROJECT` names, its own or inherited, as `org-varde-new-here`
does; without one, to the repository the org file is in, if any: a
notes repository has no tasks, so the board shows every project and
says so. Set `org-varde-board-scope` to `all` to open it on every
project instead. Come back to from a task's terminal or view, the board
is as you left it, kept to a project or not, with point on that task.

## The view and its cairns

A task's **cairn** is what its sessions learned, handed to the next
one: a Status line, Findings, Decisions and Open questions, a few
short lines each. varde keeps it up to date as sessions end and hands
it to every new session. A **project cairn** is what holds across a
project's tasks (Conventions, Gotchas, How-to), handed to each of its
sessions; tasks closed as done add to it.

`v` on the board (or `M-x org-varde-show` on a `varde:` link) opens
the view. Its header line holds two tabs, ` Task ` and
` Project `, the one on view highlighted, and the keys used most:

```
 Task   Project    RET go  e edit  a add  x remove  [ ] tabs  ? help
Fix the login redirect loop after session expiry  sqzz92
your turn · 2 agents · +12 −3 · app · worktree varde/fix-login
updated 20m ago · handed to 3 sessions    cairn: what its sessions learned

Status › Narrowed it to the session cookie path.
Findings 6/7
  - The redirect loop starts when the refresh token has expired. ?
  ...
Decisions
Open
```

- **Task** is the task: its title and short id, then its facts on one
  quiet line, as the terminal's view has them (its board row's status,
  what its session left running, git and project, then whether it is in
  the Inbox, its worktree's branch and what it is linked to), how its cairn is used (when
  it last changed, how many sessions it was handed to, and how many of
  its lines are stale, when any is), then the cairn, the notes waiting
  for its next update, its context, its sessions, and where its git
  work stands, which follows a moment later.
- **Project** is its project's cairn.
- The facts line says what a cairn is, quietly at its right
  ("cairn: what its sessions learned, handed to the next one", or a
  shorter form), or else the line below it, where the window has room.
- It opens on the cairn's first line, else on its Status, as the
  terminal's view does. An empty cairn is a hint ("No cairn yet: its
  first session's update writes one; a adds a line.") and its sections'
  headings, with no Status until one is set.
- `]` and `[` switch between them; `v` again (`K` under evil), or `q`,
  closes the view.
  `M-x org-varde-show-project` opens the view on its Project tab; on a
  buffer of no task, it shows a project's cairn alone.

A section says how full it is only when it is full or nearly
(`Findings 6/7`); an empty one is its heading, dimmed. A `›` in
place of the `-` (in `org-varde-mine`, a keyword's colour) marks a line you wrote (on the Status line, after
`Status`), which updates keep, and a yellow `?` at its end one about
code that is no longer there; the view's `?` menu says so too. While the view is
in sight it follows varde, as the board does.

Edit a cairn there, a line at a time. What you write is yours. The
keys are the terminal view's:

| Key | Does |
|---|---|
| `n`/`p`, `j`/`k` | The next / previous line or heading (`M-<`/`M->`, or `gg`/`G` under evil: the first / last). |
| `J`/`K` | The next / previous task, in the board's order, on the tab the view is on (`C-j`/`C-k` under evil, where `K` closes the view). A folded group on the board opens for a task in it. |
| `TAB`/`S-TAB` | The next / previous task that needs you, the view kept open. On a section's heading `TAB` folds it, as in org. |
| `]`, `[` | The other tab. |
| `RET` | Go to the task's session, as on the board. |
| `e` | Edit the line at point (`Edit line #12: `), starting from its text; on the Status line, the Status (`Status: `). |
| `o` | Add a line to the section at point (`Findings · new line: `), as vi's `o` opens one; elsewhere, as `a`. |
| `a` | Add a line to a section you pick, one key each: `f`indings, `d`ecisions, `o`pen, `s`tatus, or on the Project tab `c`onventions, `g`otchas, `h`ow-to; `RET` picks the section at point, else Findings (Conventions). |
| `x` | Remove the line at point, asking nothing. Recall still finds it, and `u` brings it back where it was. On a Status you set, hand it back (`Status handed back: the next update may change it; u undoes`); `u` or `e` makes it yours again. |
| `u` | Bring back the line last removed here, or make a Status handed back yours again (`The Status is yours again`). With nothing of that to undo, reopen the task in view when `d`, `P` or `X` here closed it (so `Done "…"; u reopens` holds in the view too). |
| `E` | Edit the whole cairn of the tab as text, with its context above it, for changes to many lines at once. It opens at the end of the Status line; varde's `#:` lines cannot be changed. `C-c C-c` (or `C-x C-s`) saves, and `C-c C-k` cancels, asking first when you changed something: say no and it stays. The windows are then as they were. If the cairn was updated while you edited it, nothing is saved: the buffer shows it as it is now, and what you wrote is in the kill ring. |
| `z` | Fold or unfold the section at point (`za` under evil). |
| `=` | The task's changes in `magit-status` or `vc-dir`, as on the board. |
| `d`, `P`, `X`, `s`, `R`, `F`, `m`, `/`, `.` | As on the board, for the task shown (`X` drops it: `x` removes a line). `/` searches the task's project first. |
| `y` | Copy a link to the task, or on the Project tab to its project's cairn, for `C-y`. |
| `g`, `q`, `?` | Refresh (`gr` under evil); close; every key, and what the marks mean. |

Every line is typed in the minibuffer, its words counted at the right
(`5/40`). Saved, it says so, as the terminal does (`Added to Findings,
as yours`, `Changed line #12, as yours`, `Status set, yours now:
updates won't change it; x hands it back`); `x` says `Removed: u
brings it back, and recall still finds it`. The view's `u` brings back
a line, as the board's reopens a task: each undoes what was done in
its own buffer, as vi's undo does. A task closed in the view is
reopened by the view's `u`, and the close forgets a line removed
before it. A line varde refuses (a line too long, a section full) stays
open with what you wrote, and why beside it. A full section is said
at once, before you write. The caps are the ones varde says.

**Nothing typed is lost.** `C-g` keeps what you wrote as a draft of
that line, saying how to take it up again, as the terminal does
(`Draft kept for "…": e on the line continues it`, or `a f continues
it` for a new line in Findings, `e on the Status continues it`): the next
`e`, `a` or `o` on it starts from it, and it is in the kill ring too,
for `C-y`. To drop a draft, clear the line and `C-g`. The draft
of a line you remove becomes one to add to its section.

A task made with `org-varde-new-here` has a From line in its context,
shown as `From  auth.org › Auth › SSO`: `RET` on it goes to where it
came from.

## Moving between sessions

Each task's session runs in a terminal of its own (vterm, or term),
reused every time you go to the task. With Doom's workspaces, each task
gets a workspace too, named by its short id and title.

- `RET` on the board, or in a task's view, goes to the session.
- `M-x org-varde-next-needs-you` goes, from anywhere, to the session of
  the next task that needs you: the first you have not seen, other
  than the one you are in, else the one after it. The one you are in
  is the task of the terminal or view you are in; a board's line or a
  link at point is not. Pressed again at
  once, it goes on from the last one it went to. A task running in a
  terminal outside Emacs is named once, not gone to, and passed over
  until it moves on. It is the key to bind:

  ```elisp
  (global-set-key (kbd "C-c v n") #'org-varde-next-needs-you)
  ;; On Doom, SPC l n in the map! above.
  ```

- `org-varde-board` comes back to the board, in its own workspace: the
  one it was opened in.
- In a `term` (without vterm), `C-c` is term's own escape, where
  `C-c v` would be `C-x v`. The keys you bound under `C-c v`, as
  above, work there all the same; other `C-c` keys of yours do not.
- Under evil, a session wants its terminal in insert state, so what
  you type goes to the agent. evil-collection's vterm setup does that
  (Doom's `:term vterm` has it); org-varde leaves evil's states as
  you set them.
- Going to a task whose terminal is already in a window selects that
  window; the terminal is never shown twice.

Closing a task closes its workspace and its terminal, but not the
buffers you opened there. `org-varde-workspaces` set to nil keeps
everything in one workspace; without Doom it changes nothing.

## The Inbox

**Sessions you start outside Emacs.** A `claude` session started in a
plain terminal, or with `N`, becomes an Inbox task once you ask it
something. While it runs it shows under Needs you or Working, as every
live session does, and once it ends it sits in the **Inbox** group. It
stays there until you decide what it is: `a` keeps it, and so does
starting it with `RET`; `d` closes it, `P` parks it, and `x` drops it.
Nothing leaves the Inbox by itself.
