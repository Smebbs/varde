# org-varde for Emacs

**org-varde** (the `.el` files at the repository's root) is varde's Emacs client. It turns
org headings into varde tasks.

- **Starting.** Start a session from its heading. The heading's title,
  body and outline go along as the task.
- **The board.** Watch every session in one place.
- **Finishing.** Mark the heading DONE to close its task. Nothing is
  asked about its worktree, which goes once its work has landed.
- **Memory.** Read and edit a task's cairn, and search earlier work.
- **What it writes to org.** It writes a `VARDE_TASK` property on
  headings you start, link or file, and the heading `f` files. It also
  writes todo keywords:
  - the board's done, park and throw-away set them;
  - starting a closed heading sets it back to TODO;
  - a task closed, parked or reopened elsewhere has its heading follow,
    say when an agent ran `varde done`.

  A change org-varde makes is saved only when it is the buffer's only
  one.

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
  :bind (("C-c v s" . org-varde-start)
         ("C-c v b" . org-varde-board)
         ("C-c v d" . org-varde-done)
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
      :desc "Start"     "a" #'org-varde-start
      :desc "Board"     "b" #'org-varde-board
      :desc "Done"      "d" #'org-varde-done
      :desc "Recall"    "r" #'org-varde-recall
      :desc "Show task" "s" #'org-varde-show)
```

**Where tasks run.** Set the project directory once, high up in the
file or on a parent heading, as `#+PROPERTY: VARDE_PROJECT ~/src/app`.
When no heading above has one, `org-varde-start` asks. Each start
sends the heading's VARDE_PROJECT with its title and body, so moving a
heading to a file or subtree with another VARDE_PROJECT moves its task
there on its next start. varde refuses while the task has an active
worktree or a live session; the message says why, and the task stays
in its old project. A VARDE_PROJECT that is not a directory on this
machine, as in a file synced from another, moves nothing.
`org-varde-use-worktree` (`ask`, `always` or `never`) says whether a
task's first run gets a worktree of its own.

**Where headings are found.** In the agenda files and their archives,
and in every `.org` and `*_archive` file under `org-directory`. If you
keep tasks elsewhere, add those directories to `org-varde-directories`.

`M-x org-varde-doctor` checks the installation.

## Use

**`org-varde-start` on a heading:**

- **The first time,** it creates the task and starts it in the
  background.
- **Afterwards,** it resumes the task where it ran, also in the
  background, or goes to the terminal it is running in. Set
  `org-varde-start-goes` to go to the terminal every time.
- `C-u` starts a fresh conversation, and `C-u C-u` forks the latest
  one.

**Marking the heading DONE** closes the task. Nothing is asked about
its worktree, which goes once its work has landed. A session still
working is stopped only after asking; on the board, `d` and `P` ask as
you press them.

- **Marked DONE by mistake?** Set it back to TODO (or SOMEDAY) with
  `C-c C-t`, and the task reopens (or parks).
- org-varde tells varde a second after the keyword stops changing. So
  stepping through keywords acts once, on where you stop, and a
  repeating heading leaves its task open.
- Undo does not count, since it fires no state change.

**Marking it SOMEDAY** parks the task. That is for work you may come
back to, or may not. Its session stops and its worktree stays. It moves
to the board's collapsed **Parked** group. Set it back to TODO, or
start it, to pick it up again. `org-varde-park-keywords` sets the
keywords.

**Closed elsewhere.** A task closed, parked or reopened outside Emacs,
by an agent or in a terminal, has its heading follow. When the heading
and the task disagree, the more recent change wins. A busy session is
never stopped to make them agree. A file a sync changed is reread
first; one with unsaved edits waits until you save or revert it.

**A copied heading** keeps its `VARDE_TASK` link. Starting the copy
asks whether to give it a task of its own. Until one of them is
unlinked, the board cannot go to either heading, the task's changes
reach neither, and a keyword change on either is set back.

**On the board,** the header shows what you need most:

| Key | Does |
|---|---|
| `RET` | Start or resume in the background, or go to a running task. `C-u` starts fresh, `C-u C-u` forks. |
| `n` | New task: asks for a title, context and project, offering the project of the task at point. Nothing starts until you press `RET` on it. |
| `d` | Done |
| `f` | File the task into org: a new heading beside its origin, or under one you pick |
| `x` | Throw the task away, after asking. Any work in its worktree is kept under `refs/varde/discarded/`. |
| `?` | Everything else, in a menu |

The menu also has these keys, which work on the board directly too:

- `P` parks the task, and `a` accepts it.
- `l` links it to a heading, and `o` goes to its heading.
- `s` shows it. In that buffer, `e` edits the cairn: `C-c C-c` saves and
  `C-c C-k` cancels.
- `S` shows the cairn of the task's project, and `E` edits it.
- `/` recalls: RET on a hit shows its task, and `o` goes to its heading.
- `N` starts a session of your own.
- `m` moves the task to another project, asking for the directory with
  its project filled in. varde's refusal (an active worktree, a live
  session) shows on the message line. A linked heading that names a
  VARDE_PROJECT, its own or inherited, gets the new one on itself, so
  its next start does not move the task back.

To move, use `C-n`/`C-p` between tasks and `M-n`/`M-p` between groups. TAB
folds a group, `g` refreshes and `q` quits. `d`, `P` and `a` act on
every task in the region, so a full Inbox is cleared in one go. `RET`
on a region starts each open task in it in the background, and goes to
no terminal; done, parked and dropped tasks are skipped. The worktree question is asked once for them all; each
project question names its task and offers your last answer. Under
evil, `gr` refreshes, `gj`/`gk` move between groups and `C-j`/`C-k`
between tasks; `V` selects lines for RET, `d`, `P` and `a`. `v` and
`V` are left to evil.

`M-x org-varde-link` links the heading at point to a task.

**Sessions you start outside org.** A `claude` session started in a
plain terminal, or with `N`, becomes an Inbox task once you ask it
something. While it runs it shows under Needs you or Working, as every
live session does, marked `◇` as any task without a heading is, and
once it ends it sits in the **Inbox** group. It stays an Inbox task
until you decide what it is:

- **`f` files it into org.** A linked heading is written beside the
  heading it was deferred from. Otherwise it goes under a heading you
  pick among your refile targets, or among the agenda files' headings
  if you have none.
- **`l`, or `org-varde-link` on a heading, links it to a heading you
  already have.** Done tasks and tasks whose heading is gone can be
  linked too. The heading's title and body replace the task's. The
  task keeps its project: a heading that inherits another VARDE_PROJECT
  gets the task's on itself, as does one `f` files.
- **`a` keeps it without a heading,** and so does starting it with RET.
  Org is optional for every task.
- **`d` closes it, `P` parks it, and `x` throws it away.**

Nothing leaves the Inbox by itself.
