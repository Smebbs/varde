# org-varde

org-varde (the `.el` files at the repo root) is varde's Emacs client.
Tasks live on the board, notes in org, and `varde:` links connect them.
org-varde keeps no state of its own and only writes the links you ask
for into org.

## Install

You need varde and its hooks ([the guide](guide.md#install)), and
Emacs 29.1 or later. Sessions open in the first installed terminal in
`org-varde-terminals`: by default
[ghostel](https://github.com/dakra/ghostel), then
[vterm](https://github.com/akermu/emacs-libvterm), else `term`. With
Doom's workspaces, each task gets one.

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
`M-: (package-vc-install '(org-varde :url "https://github.com/Smebbs/varde"))`
and drop the `:vc` line.

On Doom:

```elisp
;; packages.el
(package! org-varde :recipe (:host github :repo "Smebbs/varde"))

;; config.el
(use-package! org-varde
  :after org
  :config (org-varde-mode 1))

;; SPC v under evil. Without evil Doom's leader is C-c, where v is
;; version control: pick a free letter.
(map! :leader :prefix ("v" . "varde")
      :desc "Board"           "b" #'org-varde-board
      :desc "Link a task"     "i" #'org-varde-insert-link
      :desc "New task here"   "t" #'org-varde-new-here
      :desc "Next needing me" "n" #'org-varde-next-needs-you
      :desc "Recall"          "r" #'org-varde-recall
      :desc "View task"       "v" #'org-varde-show)
```

`org-varde-use-worktree` (`ask`, `always` or `never`) decides whether
a task's first run gets a worktree. `M-x org-varde-doctor` checks the
setup.

## In org

**Links.** `org-varde-insert-link` picks a task by title and inserts
`[[varde:k2f0g3][Fix the login bug]]`; a project's cairn is
`varde:project:~/src/app`. While `org-varde-mode` is on, a link shows
its task's state live after itself (`● working`, `○ open`, `✓ done`).
Following it opens the task's view. On the board or in a view, `y`
copies a link to the task.

**A task from a note.** `org-varde-new-here` on a heading, or a region,
opens the new-task form, filled in from it:

```
Title:   Fix the SSO redirect
Project: ~/src/app
When:
── context ──────────────────────────────
It loops after the session expires.

From: [[file:~/notes/auth.org::*Fix the SSO redirect][auth.org › Auth › Fix the SSO redirect]]
```

`TAB` and `S-TAB` move between fields, and `RET` (or `M-TAB`) on the
Project opens the picker. The context is plain org with org's keys
(`TAB` folds, `M-RET` adds an item), and the agent gets it as written.
`C-c C-c` creates the task and puts a link to it in the heading's text
(the file is left unsaved); `C-c C-k` puts the draft away. The Project
comes from a `VARDE_PROJECT` property, own or inherited, else the
file's repo. On a heading that already links an open task, it offers
to go to that instead.

A heading's `SCHEDULED:` goes into `When:`, so varde starts the task
then; clear it to start it yourself. It's copied once: later, change
the schedule with `e` on the board, not the org timestamp.

**Picking a project** works as on the
[terminal board](guide.md#the-project-picker), in the minibuffer:
projects as `name  ~/dir`, the likeliest first so RET takes it.
Your completion UI narrows them (vertico, ivy and the like keep
varde's order), a typed directory (`~`, `/` or `.` first) completes to
the directories in it, and `another directory…` uses
`read-directory-name`.

**Marking a heading DONE** offers to close the open tasks it links
(`org-varde-close-with-heading`: `ask`, `always` or `never`). Closing
a task never touches the heading.

**A review.** A dynamic block lists tasks with their cairn's Status;
`C-c C-c` on its first line fills it:

```
#+BEGIN: varde :project ~/src/app :state all :closed 7d
|   | Task                                           | Project | Status         |     Closed |
|---+------------------------------------------------+---------+----------------+------------|
| ○ | [[varde:kvdcpq][Write onboarding docs]]        | app     | Outline agreed |            |
| ✓ | [[varde:hxj30t][Remove the old feature flags]] | app     | Flags removed  | 2026-10-13 |
#+END:
```

`:state` takes `open`, `done` or `all`, and `:closed` `24h`, `7d` or
`2w`.

**From an older org-varde.** `M-x org-varde-convert-links` turns
`VARDE_TASK` properties on headings into links (`C-u`: in every agenda
and `org-directory` file).

## The board

`M-x org-varde-board` opens it, kept to the current buffer's project
(`.` shows every project). It works like the terminal board; `?` lists
every key.

| Key | Does |
|---|---|
| `RET` | Go to the task's session, starting or resuming it. `R` fresh, `F` fork. |
| `s` | Start it in the background and stay on the board. |
| `TAB` / `S-TAB` | The next / previous task that needs you. |
| `v` | View the task and its cairn. |
| `n` / `e` | New task / edit it, in the same form as above. |
| `d` / `P` / `x` | Done / park / drop. `u` reopens. |
| `a` | Accept an Inbox task. |
| `m` | Move it to another project. |
| `y` | Copy a link to the task. |
| `/` | Recall (`C-u /`: every project). |
| `=` | Its changes, in magit or `vc-dir`. |
| `E` | Edit the whole cairn as text. |
| `N` | A session of your own. |
| `z` | Fold the group. |
| `g` / `q` | Refresh / quit. |
| `L` | Loop the task: asks what to loop until, how many rounds, and which profiles to move up to when stuck. On a loop at work, pause or end it; paused, resume it; stopped, run it again. |
| `c` / `C` | Set the task's check (command, a review by another model, paths it must not change) / run it now. |
| `O` | What its last check printed. |
| `A` | Approve its work: what waits on your approval can start. |
| `t` | Its trail, drawn as a tree: `RET` shows a task, `S` sets the trail going, and `L` `c` `C` `A` `M` work on the line's task. |
| `M` | The profile (agent, model, effort) it runs on. |
| `r` | The recipes: `RET` shows one, `n` makes tasks from it, `e` edits your copy, `?` how to write one. |

In the form, an empty Title has a model name the task, `When:` takes
a [schedule rule](guide.md#scheduled-and-repeating-tasks) (empty:
never), and changing the Project in `e` moves the task, as `m` does.

`j`/`k` move between tasks, `{`/`}` between groups. `d`, `P`, `x`, `a`
and `s` act on every task in the region. The keys live in
`org-varde-board-mode-map` and `org-varde-show-mode-map`.

Where Emacs already has a key, it differs from the terminal:

| Terminal | Emacs | evil |
|---|---|---|
| `gg` / `G` | `M-<` / `M->` | `gg` / `G` |
| `C-d` / `C-u` | `C-v` / `M-v` | `C-d` / `C-u` |
| `C-l` refresh | `g` | `gr` |
| `z` fold | `z`, `TAB` on a heading | `za` |
| `Esc` | `q` | `q` |
| space marks | the region | `V` |
| `v` view | `v` | `K` |
| `J` / `K` in a view | `J` / `K` | `C-j` / `C-k` |

The buffers you write in start in evil's insert state; `:w` and `ZZ`
save, `ZQ` cancels.

## The view

`v` on the board, or following a `varde:` link, opens the view. The
**Task** tab shows the task's state, cairn, context, sessions and git
state; the **Project** tab shows the project's cairn. `›` marks a line
you wrote (updates keep it); a yellow `?` one about code that's gone.

| Key | Does |
|---|---|
| `n`/`p`, `j`/`k` | The next / previous line. |
| `[` / `]` | The other tab. |
| `e` | Edit the line at point. |
| `o` / `a` | Add a line to this section / to one picked by key. |
| `x` / `u` | Remove the line / bring it back. |
| `E` | The whole cairn as text. |
| `J` / `K` | The next / previous task. |
| `RET` | Go to the session. |
| `d`, `P`, `X`, `s`, `/` | As on the board (`X` drops). |
| `L` `c` `C` `O` `A` `t` `M` | Loops, checks and trails, as on the board. |
| `q` | Close. |

Lines are typed in the minibuffer. `C-g` keeps a draft, and the same
key on the same line picks it up again.

## Loops, checks and trails

A task with a check, a loop or a trail says where it stands on its row:
`↺ 2/3 ✗ 1 failed` while a loop works, `✓ verified in 2` once its check
passes, `⧗ abc123` while it waits on another, `⚑ touched tests/a.rs`
when a round changed what its check guards. The view has a **Loop**
section: its profile and recipe, its check and the last result, then one
line per round with the profile it ran on, how long it took and what its
check found. `RET` on a round opens that session, `O` what its check
printed, and `RET` on a task before or after it shows that one.

The new-task form (`n`) has four more fields:

- `Profile:` what its sessions run on (`RET` picks, with each profile's model).
- `After:` the tasks it waits on (`RET` adds one).
- `Recipe:` make a trail of tasks from a recipe (`RET` picks; its
  variables then fill `Vars:` with their defaults).
- `Vars:` the recipe's variables: `check="cargo test" tests=tests/*`.

With a recipe, `C-c C-c` makes one task per step and asks whether to
set the trail going.

## Moving between sessions

Each task's session runs in its own terminal, reused every time you
go to it, and in its own Doom workspace if you use workspaces
(`org-varde-workspaces` nil keeps one).

- `RET` on the board or in a view goes to the session.
- `org-varde-next-needs-you` goes from anywhere to the next task that
  needs you. Bind it.
- `org-varde-board` comes back to the board, in its own workspace.
- In `term`, `C-c` is term's escape; the keys under `C-c` you bound
  org-varde's commands to still work.
- Under evil, a session's terminal wants insert state, as
  evil-collection sets it.

Closing a task closes its terminal and its workspace.

A `claude` session you start outside Emacs, or with `N`, becomes an
Inbox task once you ask it something, and stays there until you accept
(`a`), start or close it.

**Scheduled tasks** sit in the Scheduled group with their start time
(`↻ Mon 09:00`). varde runs them in the background; one that found
something waits in Needs you, and `RET` carries it on in its terminal.
`RET` while it's still working asks whether to take it over. The
[guide](guide.md#scheduled-and-repeating-tasks) has the details.
