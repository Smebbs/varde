;;; org-varde.el --- Org-mode client for varde -*- lexical-binding: t; -*-

;; Copyright (C) 2026 Morten Smebye

;; Author: Morten Smebye <mortensmeb@gmail.com>
;; URL: https://github.com/Smebbs/varde
;; Version: 1.1.1
;; Package-Requires: ((emacs "29.1") (org "9.6") (transient "0.4"))
;; Keywords: tools, outlines
;; SPDX-License-Identifier: MIT

;; This file is not part of GNU Emacs.  It is free software under the
;; MIT License; see the LICENSE file that comes with it.

;;; Commentary:

;; varde runs AI coding-agent sessions (Claude Code or pi) as tasks and
;; keeps what they learned.  It is a separate command-line program: see
;; the README at the URL above to install it.  This package drives varde
;; from Emacs: watch the board (`org-varde-board'), start and go to
;; sessions in terminals, close tasks, read and edit their cairns, and
;; search earlier work.
;;
;; Tasks live on the board, and meet your org notes through links:
;; [[varde:SHORT]] opens a task and shows where it stands,
;; `org-varde-new-here' makes a task from a heading or a region and
;; links it there, and a heading marked DONE offers to close the tasks
;; it links.  varde owns everything durable: org-varde writes nothing to
;; org but the links you ask for.

;;; Code:

(require 'filenotify)
(require 'json)
(require 'org)
(require 'org-element)
(require 'seq)
(require 'subr-x)
(require 'text-property-search)
(require 'transient)

(declare-function vterm "vterm" (&optional arg))
(declare-function vterm-send-string "vterm" (string &optional paste-p))
(declare-function vterm-send-return "vterm" ())
(declare-function term-char-mode "term" ())
(declare-function +workspace-list-names "ext:workspaces" ())
(declare-function +workspace-current-name "ext:workspaces" ())
(declare-function +workspace-switch "ext:workspaces" (name &optional auto-create-p))
(declare-function +workspace-kill "ext:workspaces" (name &optional inhibit-kill-p))
(declare-function evil-set-initial-state "evil-core" (mode state))
(declare-function hl-line-highlight "hl-line" ())
(declare-function +workspace-get "ext:workspaces" (name &optional noerror))
(declare-function +workspace-contains-buffer-p "ext:workspaces" (buffer &optional persp frame))
(declare-function evil-define-key* "evil-core" (state keymap key def &rest bindings))

(defgroup org-varde nil
  "Org-mode client for varde."
  :group 'tools
  :prefix "org-varde-")

(defcustom org-varde-varde-program "varde"
  "The varde executable."
  :type 'string)

(defcustom org-varde-agent-command nil
  "What `org-varde-new-session' runs for a session of your own.
Nil runs the agent varde starts a new conversation with, as its
settings say."
  :type '(choice (const :tag "varde's agent" nil) string))

(defcustom org-varde-use-worktree 'ask
  "Whether a task's first run gets its own git worktree.
`ask' asks, the first time a task starts, when varde needs to know."
  :type '(choice (const ask) (const always) (const never)))

(defcustom org-varde-workspaces t
  "Open a Doom workspace per task, when Doom's workspaces are loaded."
  :type 'boolean)

(defcustom org-varde-terminal-function #'org-varde--open-shell
  "Function of (DIRECTORY NAME) that opens a terminal and returns its buffer.
The default opens a vterm when vterm is installed, and a term otherwise."
  :type 'function)

(defcustom org-varde-send-function #'org-varde--send
  "Function of (COMMAND) that types COMMAND into the current terminal."
  :type 'function)

(defconst org-varde-property "VARDE_TASK"
  "The property an earlier org-varde linked a heading to its task with.
`org-varde-convert-links' turns it into a link.")

(define-error 'org-varde-varde-error "varde")

;; Bound by org around `org-after-todo-state-change-hook'.
(defvar org-state)
(defvar org-last-state)

(defvar org-varde-mode)
(defvar org-varde--watch nil "The watch on varde's state directory.")
(defvar org-varde-board-data)
(defvar org-varde-board--project)
(defvar org-varde-board--closed)
(defvar org-varde--tab)
(defvar org-varde--project-dir)

(defconst org-varde-board-buffer "*varde*"
  "The board's buffer name.")

(defvar org-varde--kept nil
  "The project the board keeps to as last drawn, or nil when it shows all.")

;;; --- varde -----------------------------------------------------------------

(defconst org-varde--safe-directory (expand-file-name "~/")
  "Where varde runs from.
Every call names its paths, and the current buffer's directory may be
gone: a task's terminal outlives the worktree `done' removed.")

(defun org-varde--call (input args)
  "Run varde with ARGS, INPUT on stdin when non-nil.  Return its stdout.
Signals `org-varde-varde-error' with varde's message on failure.  What
varde says on stderr when it succeeds (a worktree restored, a setup
that failed) is shown in the echo area."
  (let ((err (make-temp-file "varde-err"))
        (default-directory org-varde--safe-directory))
    (unwind-protect
        (with-temp-buffer
          (let ((code (if input
                          ;; Without its properties: read-only text could not be sent.
                          (progn (insert (substring-no-properties input))
                                 (apply #'call-process-region (point-min) (point-max)
                                        org-varde-varde-program t (list t err) nil args))
                        (apply #'call-process org-varde-varde-program nil (list t err) nil args))))
            (let ((said (string-trim (with-temp-buffer (insert-file-contents err)
                                                      (buffer-string)))))
              (unless (eq code 0)
                (signal 'org-varde-varde-error (list (string-remove-prefix "varde: " said))))
              (unless (string-empty-p said)
                (message "%s" said)))
            (buffer-string)))
      (delete-file err))))

(defun org-varde--varde (&rest args)
  "Run varde with ARGS and return its stdout."
  (org-varde--call nil args))

(defun org-varde--json (string)
  "Parse varde's JSON output STRING into plists and lists."
  (if (json-available-p)
      (json-parse-string string :object-type 'plist :array-type 'list
                         :null-object nil :false-object nil)
    ;; Emacs 29 built without libjansson.
    (let ((json-object-type 'plist) (json-array-type 'list) (json-false nil))
      (json-read-from-string string))))

(defun org-varde--varde-json (&rest args)
  "Run varde with ARGS and parse its JSON output."
  (org-varde--json (org-varde--call nil args)))

(defun org-varde--varde-async (args callback &optional on-error input)
  "Run varde with ARGS without waiting, and return its process.
INPUT, when non-nil, goes to its stdin.  CALLBACK gets its stdout when
it succeeds.  When it fails, varde's message is shown and ON-ERROR, if
given, is called with no arguments."
  (let* ((default-directory org-varde--safe-directory)
         (out (generate-new-buffer " *varde out*"))
         (err (generate-new-buffer " *varde err*"))
         (stderr (make-pipe-process :name "varde-stderr" :buffer err :noquery t :sentinel #'ignore))
         (called nil)
         ;; Once only: a failed start and the sentinel would both call it.
         (done (lambda (ok text)
                 (unless called
                   (setq called t)
                   (when (process-live-p stderr) (delete-process stderr))
                   (kill-buffer out)
                   (kill-buffer err)
                   (condition-case e
                       (if ok
                           (funcall callback text)
                         (message "varde: %s" (string-remove-prefix "varde: " (string-trim text)))
                         (when on-error (funcall on-error)))
                     (error (message "org-varde: %s" (error-message-string e))))))))
    (condition-case e
        (let ((proc (make-process
                     :name "varde" :buffer out :stderr stderr :noquery t :connection-type 'pipe
                     :command (cons org-varde-varde-program args)
                     :sentinel (lambda (proc _)
                                 (unless (process-live-p proc)
                                   (let ((ok (eq (process-exit-status proc) 0)))
                                     (funcall done ok (with-current-buffer (if ok out err)
                                                        (buffer-string)))))))))
          ;; A varde that exits before reading says why in its sentinel.
          (ignore-errors
            (when input (process-send-string proc input))
            (process-send-eof proc))
          proc)
      (error (funcall done nil (error-message-string e)) nil))))

(defun org-varde--short (task)
  "The six characters of TASK's id people see and type."
  (downcase (substring task (max 0 (- (length task) 6)))))

(defun org-varde--age (ms)
  "MS milliseconds as a short age."
  (let ((s (/ (max 0 ms) 1000)))
    (cond ((< s 60) (format "%ds" s))
          ((< s 3600) (format "%dm" (/ s 60)))
          ((< s 86400) (format "%dh" (/ s 3600)))
          (t (format "%dd" (/ s 86400))))))

(defun org-varde--named (task &optional title)
  "TASK as a message names it: by TITLE, as the board shows it, when known."
  (setq title (or title (plist-get (org-varde--row task) :title)))
  (if (and title (not (string-blank-p title)))
      (format "\"%s\"" (truncate-string-to-width title 40 nil nil "…"))
    (org-varde--short task)))

(defun org-varde--pick (prompt choices &optional default)
  "Ask with PROMPT for one of CHOICES, kept in the order given, offering DEFAULT."
  (completing-read (format-prompt prompt default)
                   (lambda (string pred action)
                     (if (eq action 'metadata)
                         '(metadata (display-sort-function . identity) (cycle-sort-function . identity))
                       (complete-with-action action choices string pred)))
                   nil t nil nil default))

(defun org-varde--details (task)
  "TASK as `varde show --json --no-git' gives it."
  (org-varde--varde-json "show" task "--json" "--no-git"))

;;; --- Org -----------------------------------------------------------------------

(defun org-varde--heading-p ()
  "Non-nil when point is on or under an org heading."
  (and (derived-mode-p 'org-mode) (not (org-before-first-heading-p))))

(defun org-varde--title ()
  "The heading's title, without keyword, priority, tags or fontification,
and with links as their descriptions and no statistics cookies, as org
shows it."
  (string-trim
   (replace-regexp-in-string
    "[ \t]*\\[[0-9]*\\(?:%\\|/[0-9]*\\)\\][ \t]*" " "
    (org-link-display-format (substring-no-properties (org-get-heading t t t t))))))

(defun org-varde--body ()
  "The heading's own text: below its metadata, above its first child.
Drawers are left out; they are org's bookkeeping, not the task."
  (save-excursion
    (org-back-to-heading t)
    (let ((end (save-excursion (outline-next-heading) (point))))
      (org-end-of-meta-data t)
      ;; Org's own lines come first: its log of state changes and notes,
      ;; when not kept in a drawer, and clock lines.
      (let ((log (concat "^[ \t]*\\(?:CLOCK:\\|- \\(?:State \"\\|Note taken on \\|CLOSING NOTE \\|"
                         "Rescheduled from \\|Not scheduled\\|New deadline \\|Removed deadline\\|Refiled on \\)\\)")))
        (while (and (< (point) end)
                    (or (looking-at-p "^[ \t]*$") (looking-at-p log)))
          ;; A note's lines below its first are indented under it.
          (let ((indent (current-indentation)))
            (forward-line 1)
            (while (and (< (point) end) (not (looking-at-p "^[ \t]*$"))
                        (> (current-indentation) indent) (not (looking-at-p log)))
              (forward-line 1)))))
      (let ((text (buffer-substring-no-properties (min (point) end) end)))
        (string-trim
         (replace-regexp-in-string
          "^[ \t]*:[A-Za-z_-]+:[ \t]*\n\\(?:.*\n\\)*?[ \t]*:END:[ \t]*\n?" "" text))))))

;;; --- Terminals and workspaces ----------------------------------------------

(defvar-local org-varde--task nil
  "The varde task this terminal buffer belongs to.")

(defun org-varde--open-shell (directory name)
  "Open a terminal named NAME in DIRECTORY running your shell; return it.
A vterm when vterm is installed, else a term."
  (let ((default-directory (file-name-as-directory directory)))
    (if (require 'vterm nil t)
        (save-window-excursion (vterm name))
      (let ((buffer (make-term (string-trim name "\\*" "\\*")
                               (or (bound-and-true-p explicit-shell-file-name)
                                   (getenv "ESHELL") shell-file-name))))
        (with-current-buffer buffer
          (term-char-mode)
          ;; There C-c is term's escape, where C-c v is C-x v: your C-c v
          ;; keys, as the README binds org-varde's, are kept.
          (let ((keys (keymap-lookup global-map "C-c v")))
            (when (keymapp keys)
              (let ((map (make-sparse-keymap)))
                (set-keymap-parent map (current-local-map))
                (keymap-set map "C-c v" keys)
                (use-local-map map)))))
        buffer))))

(defun org-varde--send (command)
  "Type COMMAND into the current terminal and run it."
  (if (derived-mode-p 'vterm-mode)
      (progn (vterm-send-string command) (vterm-send-return))
    (process-send-string nil (concat command "\n"))))

(defun org-varde--terminal (task)
  "TASK's terminal buffer, if it is still open."
  (seq-find (lambda (b) (and (equal (buffer-local-value 'org-varde--task b) task)
                             (get-buffer-process b)))
            (buffer-list)))

(defun org-varde--tty-terminal (tty)
  "The buffer whose process runs on terminal TTY, if any.
That is a terminal a session was typed into, not one org-varde opened."
  (and tty
       (seq-find (lambda (b)
                   (let ((p (get-buffer-process b)))
                     (and p (equal (process-tty-name p) tty))))
                 (buffer-list))))

(defun org-varde--workspaces-p ()
  "Whether to use Doom's workspaces: wanted, and loaded."
  (and org-varde-workspaces (fboundp '+workspace-switch)))

(defun org-varde--go-to-buffer (buffer)
  "Show BUFFER where it already is.
In the current workspace if it holds it, else in the first workspace
that does.  Makes no workspace."
  (when (and (org-varde--workspaces-p) (not (+workspace-contains-buffer-p buffer)))
    (let ((name (seq-find (lambda (n) (+workspace-contains-buffer-p buffer (+workspace-get n t)))
                          (+workspace-list-names))))
      (when name (+workspace-switch name))))
  (let ((window (get-buffer-window buffer)))
    (if window (select-window window) (pop-to-buffer-same-window buffer))))

(defun org-varde--workspace (task)
  "The name of TASK's open workspace, if it has one.
Found by the task's id, never by its title, which can change."
  (and (org-varde--workspaces-p)
       (seq-find (lambda (n) (string-prefix-p (org-varde--short task) n))
                 (+workspace-list-names))))

(defun org-varde--workspace-try (what fn name &rest args)
  "Call FN with workspace NAME and ARGS; on failure, say it could not WHAT it."
  (condition-case err
      (apply fn name args)
    (error (message "org-varde: could not %s workspace %s: %s" what name (error-message-string err)))))

(defun org-varde--workspace-name (task title)
  "The workspace for TASK: found by its id, named with TITLE for display."
  (or (org-varde--workspace task)
      (format "%s %s" (org-varde--short task) (truncate-string-to-width (or title "") 24 nil nil "…"))))

(defun org-varde--workspace-open (task title)
  "Switch to TASK's workspace, named with TITLE, making it if needed."
  (when (org-varde--workspaces-p)
    (org-varde--workspace-try "open" #'+workspace-switch (org-varde--workspace-name task title) t)))

(defun org-varde--workspace-current ()
  "The name of the current workspace, when org-varde uses them."
  (and (org-varde--workspaces-p) (+workspace-current-name)))

(defun org-varde--workspace-return (name)
  "Switch back to workspace NAME."
  (org-varde--workspace-try "return to" #'+workspace-switch name))

(defun org-varde--workspace-close (task)
  "Close TASK's workspace, if it has one.
Its buffers stay: the terminal is closed apart, and what you opened
there is yours."
  (let ((name (org-varde--workspace task)))
    (when name (org-varde--workspace-try "close" #'+workspace-kill name t))))

(defvar org-varde--background nil
  "Non-nil while starting tasks without going to any terminal.
The board's s binds it, and its RET on a region.")

(defun org-varde--open-terminal (task title cwd command)
  "Run COMMAND for TASK in its terminal, opening one in CWD if needed.
TITLE names a workspace made for it."
  (let ((from (org-varde--workspace-current))
        (goes (not org-varde--background)))
    (org-varde--workspace-open task title)
    (let ((buffer (or (org-varde--terminal task)
                      (funcall org-varde-terminal-function cwd
                               (format "*varde %s*" (org-varde--short task))))))
      ;; Something already running there, such as the session a start a
      ;; moment ago launched, would take the command as its input.
      (if (org-varde--terminal-busy-p buffer)
          (progn (unless org-varde--background (org-varde--go-to-buffer buffer))
                 (message "%s is already starting in its terminal" (org-varde--named task title)))
        (with-current-buffer buffer
          (setq org-varde--task task)
          (funcall org-varde-send-function command))
        (cond (goes (org-varde--go-to-buffer buffer))
              ((not (equal from (org-varde--workspace-current)))
               ;; Lay the task's workspace out with its terminal, then come back.
               (org-varde--go-to-buffer buffer)
               (org-varde--workspace-return from)))
        (unless goes
          (message "Started %s in the background; RET goes to it" (org-varde--named task title))))
      buffer)))

(defun org-varde--terminal-busy-p (buffer)
  "Whether BUFFER's terminal is running something, rather than at its shell."
  (and buffer (get-buffer-process buffer)
       (process-running-child-p (get-buffer-process buffer))))

;;; --- Starting ----------------------------------------------------------------

(defun org-varde--plan (task want title then fail)
  "Ask varde to start TASK, without waiting.
WANT is nil, \"--fresh\" or \"--fork\".  TITLE names the task in a
question.  THEN and FAIL are as for `org-varde--plan-answering'."
  (org-varde--plan-answering (append (list "start" task "--json")
                                     (and want (list want))
                                     (pcase org-varde-use-worktree
                                       ('always '("--worktree" "always"))
                                       ('never '("--worktree" "never"))))
                             title then fail))

(defvar org-varde--answers nil
  "Answers given while the board starts the tasks in a region, as a plist.
:region is t while it does.  The worktree answer holds for every task in
it.  The project answer is only the next one's default: each task may
belong to another project.")

(defun org-varde--plan-answering (args title then fail)
  "Run varde with ARGS without waiting, answering what it asks.
It asks about a task's first run, with an `ask' plan; each question is
asked at most once.
TITLE names the task in a question.  THEN gets the plan.  FAIL gets
varde's message, or `quit' when a question was quit."
  ;; Creating a worktree can take minutes: a repository's submodules are
  ;; cloned into it.  The callbacks run after the caller's bindings are
  ;; gone, so they are bound again for the questions and for THEN.
  (let ((background org-varde--background)
        (answers org-varde--answers))
    (org-varde--varde-async
     args
     (lambda (out)
       (let ((org-varde--background background)
             (org-varde--answers answers)
             plan next)
         (when (condition-case e
                   (progn (setq plan (org-varde--json out)
                                next (org-varde--answer plan args title))
                          t)
                 (quit (funcall fail 'quit) nil)
                 (error (message "org-varde: %s" (error-message-string e))
                        (funcall fail (error-message-string e))
                        nil))
           (if next
               (org-varde--plan-answering next title then fail)
             (funcall then plan)))))
     (lambda ()
       (let ((org-varde--background background)
             (org-varde--answers answers))
         (funcall fail "varde failed; see *Messages*"))))))

(defun org-varde--worktree-question (plan args title)
  "The question whether the task ARGS start, titled TITLE, gets a worktree
of its own, naming it and its project, as PLAN or the board knows it."
  (let* ((task (nth 1 args))
         (project (or (plist-get plan :project)
                      (cadr (member "--project" args))
                      (plist-get (org-varde--row task) :project))))
    (format "Start %s in a worktree of its own%s? " (org-varde--named task title)
            (if project (format " (%s)" (org-varde--project-name project)) ""))))

(defun org-varde--answer (plan args title)
  "ARGS with the answer to what PLAN asks added, or nil if it asks nothing.
TITLE names the task in the question."
  (pcase (and (equal (plist-get plan :action) "ask") (plist-get plan :ask))
    ((and "project" (guard (not (member "--project" args))))
     (let* ((choices (append (plist-get plan :choices) nil))
            (default (or (plist-get org-varde--answers :project) (car choices)))
            (project (expand-file-name
                      (read-directory-name (format "Project directory for %s: " (or title "this task"))
                                           default default)))
            (region (plist-get org-varde--answers :region)))
       (when region
         (setq org-varde--answers (plist-put org-varde--answers :project project)))
       (append args (list "--project" project))))
    ((and "worktree" (guard (not (member "--worktree" args))))
     (let* ((region (plist-get org-varde--answers :region))
            (worktree (or (plist-get org-varde--answers :worktree)
                          (if (y-or-n-p (if region
                                            "Start each task in the region in a worktree of its own? "
                                          (org-varde--worktree-question plan args title)))
                              "always" "never"))))
       (when region
         (setq org-varde--answers (plist-put org-varde--answers :worktree worktree)))
       (append args (list "--worktree" worktree))))
    ('nil nil)
    (other (error "Varde asked again about the %s" other))))

(defun org-varde--start (task title want &optional finished)
  "Start, resume or go to TASK's session, titled TITLE, without waiting.
WANT is as for `org-varde--plan'.  FINISHED, if given, is called with
nil once the session is started or gone to, or with why not: a message,
or `quit'."
  ;; A task has one terminal here: a second session would have nowhere
  ;; to run but inside the first.
  (when (and want (org-varde--terminal-busy-p (org-varde--terminal task)))
    (user-error "%s is running in its terminal; end that session to start another"
                (org-varde--named task title)))
  (let ((finished (or finished #'ignore)))
    (org-varde--plan
     task want title
     (lambda (plan)
       (funcall finished
                (condition-case e
                    (progn
                      (org-varde--follow-plan task title plan)
                      (org-varde-refresh)
                      nil)
                  (error (message "org-varde: %s" (error-message-string e))
                         (error-message-string e)))))
     (lambda (why)
       (when (eq why 'quit) (message "Quit"))
       (funcall finished why)))))

(defvar org-varde--outside nil
  "Tasks found running in a terminal outside Emacs, as (TASK . LAST), LAST
the time of their row then.  `org-varde-next-needs-you' passes them
over until they move on.")

(defun org-varde--follow-plan (task title plan)
  "Do what varde's PLAN says for TASK, titled TITLE."
  (pcase (plist-get plan :action)
    ("focus"
     (let ((ours (org-varde--terminal task))
           (typed (org-varde--tty-terminal (plist-get plan :tty))))
       (cond (org-varde--background
              (message "%s is already running" (org-varde--named task title)))
             (ours
              (org-varde--workspace-open task title)
              (org-varde--go-to-buffer ours))
             (typed (org-varde--go-to-buffer typed))
             (t (push (cons task (plist-get (org-varde--row task) :last)) org-varde--outside)
                (message "%s is running in a terminal outside Emacs: go there, or F on the board forks it"
                         (org-varde--named task title))))))
    ("run" (org-varde--open-terminal task title (plist-get plan :cwd) (plist-get plan :command)))
    (other (error "Unexpected plan from varde: %S" other))))

(defun org-varde--want (arg)
  "The start flag for prefix ARG."
  (pcase arg ('(4) "--fresh") ('(16) "--fork")))

(defun org-varde-start-task (task title &optional arg finished)
  "Start TASK, titled TITLE, with prefix ARG: \\[universal-argument] a fresh
conversation, two a fork.  It goes to its terminal, unless
`org-varde--background' says.  FINISHED is as for `org-varde--start'."
  (org-varde--start task title (org-varde--want arg) finished))

(defun org-varde-go-task (task &optional title arg)
  "Go to TASK's session, titled TITLE, starting or resuming it first if
needed.  Prefix ARG is as for `org-varde-start-task'."
  (let ((org-varde--background nil))
    (org-varde-start-task task title arg)))

(defun org-varde--confirm-reopen (task title state &optional doing)
  "Ask before TASK, titled TITLE, is started when STATE says it is closed.
Starting reopens it: one key is too easily pressed for that.  DOING,
when given, says what is done instead of starting it."
  (when (member state '("done" "parked" "dropped"))
    (unless (y-or-n-p (format "%s is %s: %s? " (org-varde--named task title) state
                              (or doing "reopen and start it")))
      (user-error (if doing "Nothing changed" "Nothing started")))))

(defun org-varde--needs-you ()
  "The rows of tasks that need you, in every project, as varde orders them."
  (unless org-varde-board-data
    ;; Asked for before the first refresh came back.
    (let ((data (org-varde--varde-json "list" "--json")))
      (when (org-varde--readable data) (setq org-varde-board-data data))))
  (plist-get (seq-find (lambda (g) (equal (plist-get g :key) "needs_you"))
                       (plist-get org-varde-board-data :groups))
             :rows))

(defun org-varde--needs-you-here ()
  "The rows of tasks that need you, in the project the board keeps to."
  (seq-filter (lambda (r) (or (null org-varde--kept) (equal (plist-get r :project) org-varde--kept)))
              (org-varde--needs-you)))

(defvar org-varde--last-needed nil
  "The task `org-varde-next-needs-you' last went to.
Pressed again at once, it goes on from there, wherever it landed.")

;;;###autoload
(defun org-varde-next-needs-you ()
  "Go to the session of the next task that needs you, from anywhere.
The first not yet seen, other than the one here; else the one after
here.  Here is the terminal or view you are in, never a board's line
or a link at point.  Pressed again at once, it goes on from the last
one it went to, round to the first.  A task running in a terminal
outside Emacs is said once, then passed over.  Under Doom's workspaces
it switches to the task's workspace."
  (interactive)
  (unless org-varde-mode (org-varde-mode 1))
  (let* ((all (or (org-varde--needs-you) (user-error "Nothing needs you")))
         (again (eq last-command 'org-varde-next-needs-you))
         (here org-varde--task)
         (from (if again (or org-varde--last-needed here) here))
         (rows (or (seq-remove (lambda (r) (or (equal (plist-get r :task) here)
                                               (member (cons (plist-get r :task) (plist-get r :last))
                                                       org-varde--outside)))
                               all)
                   (user-error "Nothing else needs you%s"
                               (if (seq-some (lambda (r) (not (equal (plist-get r :task) here))) all)
                                   " that Emacs can go to: the rest run outside it"
                                 ""))))
         (i (seq-position all from (lambda (r task) (equal (plist-get r :task) task))))
         (after (if i (append (seq-drop all (1+ i)) (seq-take all (1+ i))) all))
         (next (or (and (not again) (seq-find (lambda (r) (plist-get r :unread)) rows))
                   (seq-find (lambda (r) (memq r rows)) after))))
    (setq org-varde--last-needed (plist-get next :task))
    (org-varde-go-task (plist-get next :task) (plist-get next :title))))

;;;###autoload
(defun org-varde-new-session (&optional directory)
  "Start a session of your own in DIRECTORY, else `default-directory'.
varde puts it in the Inbox once you ask it something.  It runs
`org-varde-agent-command', or else the agent varde is set to start."
  (interactive)
  (pcase-let* ((directory (file-name-as-directory (expand-file-name (or directory default-directory))))
               (`(,name . ,command) (org-varde--agent))
               (buffer (funcall org-varde-terminal-function directory
                                (format "*%s %s*" name
                                        (file-name-nondirectory (directory-file-name directory))))))
    (with-current-buffer buffer (funcall org-varde-send-function command))
    (pop-to-buffer-same-window buffer)))

(defvar org-varde--agent nil
  "The agent varde starts a new conversation with, once asked: (NAME . COMMAND).")

(defun org-varde--agent ()
  "The agent of a session of your own, as (NAME . COMMAND).
`org-varde-agent-command' when set, else what `varde agent' says, asked
once; claude when varde cannot say."
  (cond (org-varde-agent-command
         (cons (file-name-nondirectory (car (split-string-and-unquote org-varde-agent-command)))
               org-varde-agent-command))
        (org-varde--agent)
        (t (let ((said (ignore-errors (org-varde--varde-json "agent" "--json"))))
             (if (and (consp said) (plist-get said :command))
                 (setq org-varde--agent
                       (cons (plist-get said :agent)
                             (mapconcat #'shell-quote-argument (plist-get said :command) " ")))
               '("claude" . "claude"))))))

;;; --- Finishing ---------------------------------------------------------------

(defun org-varde--ask-stop (task title verb)
  "What to do with TASK's session, titled TITLE, as it is closed or parked.
VERB, \"Done\" or \"Park\", names the action in the question.
`stop' when it is not busy; else you are asked: y stops it, n leaves it
running, and the task is closed or parked either way.  Esc or a quit
cancels."
  (if (not (org-varde--busy-p task))
      'stop
    ;; Worded as the terminal board asks it.  Not `read-multiple-choice':
    ;; it marks a choice's key inside its name, as the n of "running".
    (pcase (read-char-choice
            (let ((rest (format "? It %s. y stop, n leave running, Esc cancel "
                                (org-varde--busy-words (org-varde--row task)))))
              ;; On one line: the title gives way.
              (concat verb " "
                      (org-varde--named
                       task (when-let* ((title (or title (plist-get (org-varde--row task) :title))))
                              (truncate-string-to-width
                               title (max 12 (- (frame-width) (string-width rest) (length verb) 4)) nil nil "…")))
                      rest))
            '(?y ?n ?\e))
      (?y 'stop)
      (?n 'keep)
      (_ (signal 'quit nil)))))

(defun org-varde--busy-words (row)
  "How the session of ROW is busy, for a question: \"is working\", \"is
waiting for your answer\", or what its last turn left running, as \"has
3 agents and a shell running\"."
  (let ((count (lambda (key one many)
                 (let ((n (or (plist-get row key) 0)))
                   (cond ((= n 1) one) ((> n 1) (format "%d %s" n many)))))))
    (pcase (plist-get row :status)
      ("blocked" "is waiting for your answer")
      ((or "working" "delegated") "is working")
      (_ (let ((left (delq nil (list (funcall count :agents "an agent" "agents")
                                     (funcall count :shells "a shell" "shells")))))
           (if left (format "has %s running" (string-join left " and ")) "is working"))))))

(defvar org-varde--on-board nil
  "Non-nil while the board or a view closes a task, where u reopens it.")

(defun org-varde--finish (task title)
  "Close TASK, titled TITLE, in varde, and put its windows away.
varde stops the session; one that is busy is stopped only if you say,
as you press the key: cancelling changes nothing.  The worktree is removed once
its work has landed, now or later; nothing is asked.  A task varde
already has done or dropped is only put away: any session kept running
was settled when it was closed."
  (if (member (plist-get (org-varde--details task) :state) '("done" "dropped"))
      (progn (org-varde--put-away-idle task)
             (org-varde-refresh))
    (let ((keep (eq (org-varde--ask-stop task title "Done") 'keep))
          (board org-varde--on-board))
      ;; Removing a worktree can take seconds: Emacs need not wait.
      (org-varde--varde-async
       (append (list "done" task "--json") (and keep '("--keep-running")))
       (lambda (out)
         (let ((wt (plist-get (org-varde--json out) :worktree)))
           (message "Done %s%s%s" (org-varde--named task title)
                    (if board "; u reopens" "")
                    (if (equal (plist-get wt :action) "kept")
                        (format "; %s is kept until its work lands" (plist-get wt :branch))
                      "")))
         (if keep (org-varde--put-away-idle task) (org-varde--put-away task))
         (org-varde-refresh))))))

(define-obsolete-function-alias 'org-varde-throw-away-task #'org-varde-drop-task "1.0.4")

(defun org-varde--has-work-p (task)
  "Whether TASK has a worktree, whose work a drop keeps aside."
  (equal (plist-get (plist-get (org-varde--details task) :worktree) :state) "active"))

(defun org-varde-drop-task (task title &optional asked)
  "Drop TASK, titled TITLE, once you confirm, unless ASKED says you did.
Work in its worktree is kept under refs/varde/discarded/, so it can be
recovered."
  (let ((named (org-varde--named task title))
        (board org-varde--on-board))
    (when (or asked
              (if (org-varde--has-work-p task)
                  (yes-or-no-p (format "Drop %s and its work (kept under refs/varde/discarded/)? " named))
                (y-or-n-p (format "Drop %s? " named))))
      ;; Keeping the work and removing the worktree can take a while, and
      ;; a session first has seconds to stop: Emacs does not wait.
      (message "Dropping %s…" named)
      (org-varde--varde-async
       (list "drop" task "--json")
       (lambda (out)
         (let ((saved (plist-get (plist-get (org-varde--json out) :worktree) :saved)))
           (message "Dropped %s%s%s" named
                    (if (stringp saved) (format "; its work is kept at %s" saved) "")
                    (if board "; u reopens" "")))
         (org-varde--put-away-idle task)
         (org-varde-refresh))))))

(defun org-varde--row (task)
  "TASK's row on the board as it was last fetched, if it has one."
  (seq-some (lambda (g) (seq-find (lambda (r) (equal (plist-get r :task) task)) (plist-get g :rows)))
            (plist-get org-varde-board-data :groups)))

(defconst org-varde--busy '("working" "delegated" "blocked")
  "Statuses of a session that is never stopped without asking.
A session whose sub-agents are still working counts as working.")

(defun org-varde--row-busy-p (row)
  "Whether a session of ROW's task is busy: mid-turn, as varde says.
From the status of its latest session, for a varde that does not say."
  (if (plist-member row :busy)
      (plist-get row :busy)
    (member (plist-get row :status) org-varde--busy)))

(defun org-varde--busy-p (task)
  "Whether TASK's session is busy, as the board last saw it."
  (org-varde--row-busy-p (org-varde--row task)))

(defun org-varde--mark-seen (&rest _)
  "Tell varde you have seen the task whose terminal you are looking at:
the one in the selected window, while Emacs has focus."
  (let* ((buffer (window-buffer (selected-window)))
         (task (buffer-local-value 'org-varde--task buffer)))
    ;; The show and cairn buffers carry the task too, but are not its terminal.
    (when (and task (get-buffer-process buffer) (frame-focus-state) (plist-get (org-varde--row task) :unread))
      (org-varde--varde-async (list "seen" task) #'ignore))))

(defun org-varde--put-away (task)
  "Close TASK's terminal and workspace."
  (let ((terminal (org-varde--terminal task)))
    (when terminal
      (let ((kill-buffer-query-functions nil))
        (kill-buffer terminal))))
  (org-varde--workspace-close task))

(defun org-varde--put-away-done ()
  "Put away the terminals and workspaces of tasks done today.
However they were closed.  A terminal still running something, a
session kept running with --keep-running, is left open."
  (dolist (g (plist-get org-varde-board-data :groups))
    (when (equal (plist-get g :key) "done")
      (dolist (r (plist-get g :rows))
        (org-varde--put-away-idle (plist-get r :task))))))

(defun org-varde--put-away-idle (task)
  "Put TASK away, unless its terminal is still running something."
  (let ((terminal (org-varde--terminal task)))
    (unless (and terminal (process-running-child-p (get-buffer-process terminal)))
      (org-varde--put-away task))))

(defun org-varde--park (task title)
  "Park TASK, titled TITLE, in varde and put its windows away.
Its worktree stays.  varde stops the session; one that is busy is
stopped only if you agree, as you press the key."
  (if (equal (plist-get (org-varde--details task) :state) "parked")
      (org-varde--put-away-idle task)
    (let ((keep (eq (org-varde--ask-stop task title "Park") 'keep))
          (board org-varde--on-board))
      (org-varde--varde-async (append (list "park" task "--json") (and keep '("--keep-running")))
                              (lambda (_)
                                (message "Parked %s%s" (org-varde--named task title)
                                         (if board "; u reopens" ""))
                                (unless keep (org-varde--put-away task))
                                (org-varde-refresh))))))

;;; --- Links -------------------------------------------------------------------

(org-link-set-parameters "varde"
                         :follow #'org-varde--follow-link
                         :store #'org-varde--store-link
                         :complete #'org-varde--complete-link
                         :activate-func #'org-varde--link-activate
                         :export #'org-varde--export-link)

(defun org-varde--follow-link (path &optional _arg)
  "Open the view on what varde:PATH names: a task, by its id or short
id, or with project:DIR a project's cairn."
  (if (string-prefix-p "project:" path)
      (org-varde-show-project (expand-file-name (substring path 8)))
    (org-varde-show path)))

(defun org-varde--export-link (path description &rest _)
  "A varde: link to PATH, exported: its DESCRIPTION, else the task's short
id or the project's directory, as plain text, since no reader can follow it."
  (or description
      (if (string-prefix-p "project:" path) (substring path 8) (org-varde--short path))))

(defun org-varde--project-link (dir)
  "A link to the cairn of the project DIR, as (LINK . DESCRIPTION)."
  (cons (concat "varde:project:" (abbreviate-file-name dir))
        (concat (org-varde--project-name dir) " · project cairn")))

(defun org-varde--link-here ()
  "A link to the task of the board's line, of the view, or of the terminal
it runs in, as (LINK . DESCRIPTION); on the view's Project tab, to its
project's cairn.  Else nil."
  (let ((row (cond ((derived-mode-p 'org-varde-board-mode) (get-text-property (point) 'org-varde-row))
                   ((derived-mode-p 'org-varde-show-mode)
                    (unless (eq org-varde--tab 'project) (org-varde--view-row)))
                   ((and org-varde--task (get-buffer-process (current-buffer)))
                    (or (org-varde--row org-varde--task) (org-varde--details org-varde--task))))))
    (cond (row (cons (concat "varde:" (plist-get row :short)) (plist-get row :title)))
          ((and (derived-mode-p 'org-varde-show-mode) org-varde--project-dir)
           (org-varde--project-link org-varde--project-dir)))))

(defun org-varde--store-link ()
  "Store a link to the task of the board's line, of the view, or of the
terminal it runs in, as varde:SHORT described by its title; on the
view's Project tab, to its project's cairn."
  (when-let* ((link (org-varde--link-here)))
    (org-link-store-props :type "varde" :link (car link) :description (cdr link))
    t))

(defun org-varde--complete-link (&optional _arg)
  "A varde: link to a task or a project cairn you pick."
  (car (org-varde--read-link "Link to")))

(defun org-varde--link-at-point ()
  "The task of the varde: link at point, as the link names it, or nil."
  (when (derived-mode-p 'org-mode)
    (let ((link (org-element-context)))
      (and (eq (org-element-type link) 'link)
           (equal (org-element-property :type link) "varde")
           (not (string-prefix-p "project:" (org-element-property :path link)))
           (org-element-property :path link)))))

(defun org-varde--by-short (data)
  "The rows of DATA, a board from `varde list --json', by short id."
  (let ((table (make-hash-table :test #'equal)))
    (dolist (g (plist-get data :groups) table)
      (dolist (r (plist-get g :rows))
        (puthash (plist-get r :short) r table)))))

(defvar org-varde--rows nil
  "The board's rows by short id, as (DATA . TABLE), DATA the board they
are from.")

(defvar org-varde--all nil
  "Every task's row by short id, from the last `varde list --all', for
links to tasks the board no longer shows; nil until it was fetched.")

(defvar org-varde--all-due nil
  "The idle timer that fetches every task's row, while one is due.")

(defvar org-varde--all-listing nil
  "The running `varde list --all', if any.")

(defun org-varde--board-rows ()
  "The board's rows by short id, from `org-varde-board-data'."
  (unless (and org-varde--rows (eq (car org-varde--rows) org-varde-board-data))
    (setq org-varde--rows (cons org-varde-board-data (org-varde--by-short org-varde-board-data))))
  (cdr org-varde--rows))

(defun org-varde--link-row (short)
  "The row of the task SHORT names, as last fetched, or nil.  Runs nothing."
  (or (gethash short (org-varde--board-rows))
      (and org-varde--all (gethash short org-varde--all))))

(defun org-varde--status-mark (row &optional word)
  "Where ROW's task stands, as the board shows it: its glyph, and with
WORD the word after it, as a link and a review say it."
  (pcase-let ((`(,glyph ,said ,face)
               (pcase (plist-get row :status)
                 ((or "working" "delegated") '("●" "working" org-varde-board-working))
                 ("blocked" '("●" "needs input" org-varde-board-blocked))
                 ;; Hollow once you have looked and left it.
                 ("waiting" (list (if (plist-get row :unread) "●" "○") "your turn" 'org-varde-board-waiting))
                 ("done" '("✓" "done" org-varde-quiet))
                 ("parked" '("‖" "parked" org-varde-quiet))
                 ("dropped" '("✕" "dropped" org-varde-quiet))
                 (_ '("○" "open" org-varde-quiet)))))
    (propertize (if word (concat glyph " " said) glyph) 'face face)))

(defvar-local org-varde--link-shown nil
  "What this buffer's varde: links say of their tasks: short id to the
status shown, nil for none.")

(defun org-varde--link-activate (start end path _bracket)
  "Show after the varde: link from START to END, to the task PATH names,
where that task stands, from the rows last fetched.  Font-lock calls it,
so it runs nothing: when no row of every task was fetched yet, it asks
for them from an idle timer.  A project's link shows nothing, nor does a
link in a table, as a review's, whose columns it would push out."
  (dolist (o (overlays-in start end))
    (when (overlay-get o 'org-varde-link) (delete-overlay o)))
  (unless (or (string-prefix-p "project:" path)
              (save-excursion (goto-char start) (beginning-of-line) (looking-at-p "[ \t]*|")))
    (let* ((short (org-varde--short path))
           (row (org-varde--link-row short))
           (status (and row (org-varde--status-mark row t))))
      (unless org-varde--link-shown
        (setq org-varde--link-shown (make-hash-table :test #'equal)))
      (puthash short status org-varde--link-shown)
      (when status
        (let ((o (make-overlay start end nil t)))
          (overlay-put o 'org-varde-link t)
          (overlay-put o 'evaporate t)
          ;; Edited, it goes; font-lock draws it again while it is a link.
          (overlay-put o 'modification-hooks (list #'org-varde--link-edited))
          (overlay-put o 'after-string (concat " " status))))
      (unless (or row org-varde--all org-varde--all-due)
        (setq org-varde--all-due (run-with-idle-timer 0.5 nil #'org-varde--list-all))))))

(defun org-varde--link-edited (overlay &rest _)
  "Take the status of a link being edited, OVERLAY, away."
  (delete-overlay overlay))

(defun org-varde--list-all ()
  "Fetch every task's row without waiting, for the links to tasks the
board does not show, and draw those links again."
  (setq org-varde--all-due nil)
  (unless (process-live-p org-varde--all-listing)
    (setq org-varde--all-listing
          (org-varde--varde-async
           '("list" "--json" "--all")
           (lambda (out)
             (let ((data (org-varde--json out)))
               (when (org-varde--readable data)
                 (setq org-varde--all (org-varde--by-short data))
                 (org-varde--links-refresh))))
           ;; Not asked again by every link drawn: the next board does.
           (lambda () (setq org-varde--all (or org-varde--all (make-hash-table :test #'equal))))))))

(defun org-varde--links-refresh (&optional board)
  "Draw again the varde: links whose task's status changed, in the
buffers that show any.  BOARD, after a new board: when a task left it,
every task's row is fetched again, as a link may name it."
  (let* ((before (cdr org-varde--rows))
         (now (org-varde--board-rows))
         (buffers (seq-filter (lambda (b)
                                (let ((shown (buffer-local-value 'org-varde--link-shown b)))
                                  (and shown (> (hash-table-count shown) 0))))
                              (buffer-list))))
    (when (and board buffers before (not (eq before now))
               (catch 'left
                 (maphash (lambda (short _) (unless (gethash short now) (throw 'left t))) before)
                 nil))
      (org-varde--list-all))
    (dolist (b buffers)
      (let ((shown (buffer-local-value 'org-varde--link-shown b)))
        (when (catch 'changed
                (maphash (lambda (short was)
                           (let ((row (org-varde--link-row short)))
                             (unless (equal (and row (org-varde--status-mark row t)) was)
                               (throw 'changed t))))
                         shown)
                nil)
          (with-current-buffer b
            (clrhash shown)
            (font-lock-flush)))))))

(defun org-varde--links-refreshed ()
  "Redraw the statuses after links once the cached rows change."
  (org-varde--links-refresh t))

(add-hook 'org-varde-after-refresh-hook #'org-varde--links-refreshed)

(defun org-varde--known-rows ()
  "Every task's row as last fetched, the board's first, in its order.
With nothing fetched yet, every task's are fetched now; else they are
fetched again in the background, for next time."
  (if (or org-varde-board-data org-varde--all)
      (org-varde--list-all)
    (let ((data (org-varde--varde-json "list" "--json" "--all")))
      (when (org-varde--readable data) (setq org-varde--all (org-varde--by-short data)))))
  (let ((board (org-varde--board-rows))
        rows)
    (dolist (g (plist-get org-varde-board-data :groups))
      (dolist (r (plist-get g :rows)) (push r rows)))
    (when org-varde--all
      (maphash (lambda (short r) (unless (gethash short board) (push r rows))) org-varde--all))
    (nreverse rows)))

(add-to-list 'completion-category-defaults '(varde (styles substring basic)))

(defun org-varde--read-link (prompt)
  "Ask with PROMPT for a task or a project cairn, by its title, and return
a link to it as (LINK . DESCRIPTION).  Open tasks come first, then
closed ones, then the projects' cairns, each said where it stands."
  (let* ((now (truncate (* 1000 (float-time))))
         (rows (org-varde--known-rows))
         (projects (delete-dups (delq nil (mapcar (lambda (r) (plist-get r :project)) rows))))
         (table (make-hash-table :test #'equal))
         (cands nil))
    (dolist (r rows)
      (let ((cand (concat (plist-get r :title) "  " (org-varde--quiet (plist-get r :short)))))
        (puthash cand (list (if (equal (plist-get r :state) "open") "Open tasks" "Closed tasks")
                            (cons (concat "varde:" (plist-get r :short)) (plist-get r :title))
                            (org-varde--status-mark r)
                            (concat (substring (org-varde--status-mark r t) 2)
                                    " · " (or (org-varde--project-name (plist-get r :project)) "")
                                    " · " (org-varde--age (- now (or (plist-get r :last) now)))))
                 table)
        (push cand cands)))
    (dolist (dir projects)
      (let ((cand (concat (org-varde--project-name dir) " · project cairn  " (org-varde--quiet (abbreviate-file-name dir)))))
        (puthash cand (list "Project cairns" (org-varde--project-link dir) " " "") table)
        (push cand cands)))
    (unless cands (user-error "varde has no tasks yet"))
    (setq cands (sort (nreverse cands)
                      ;; Stable: open first, then closed, then cairns.
                      (lambda (a b) (< (seq-position '("Open tasks" "Closed tasks" "Project cairns") (car (gethash a table)))
                                       (seq-position '("Open tasks" "Closed tasks" "Project cairns") (car (gethash b table)))))))
    (let* ((suffix (lambda (c) (let ((said (nth 3 (gethash c table))))
                                 (if (string-empty-p said) ""
                                   (concat (propertize " " 'display `(space :align-to (- right ,(+ 2 (string-width said)))))
                                           (org-varde--quiet said))))))
           ;; Titles are typed as they read: any case, and SPC a space,
           ;; not a completion that rewrites what was typed.
           (completion-ignore-case t)
           (pick (minibuffer-with-setup-hook
                     (lambda ()
                       (use-local-map (make-composed-keymap (define-keymap "SPC" #'self-insert-command)
                                                            (current-local-map))))
                   (completing-read
                    (format-prompt prompt nil)
                    (lambda (string pred action)
                      (if (eq action 'metadata)
                          `(metadata (category . varde)
                                     (display-sort-function . identity)
                                     (cycle-sort-function . identity)
                                     (group-function . ,(lambda (c transform) (if transform c (car (gethash c table)))))
                                     (annotation-function . ,suffix)
                                     (affixation-function
                                      . ,(lambda (cs) (mapcar (lambda (c) (list c (concat (nth 2 (gethash c table)) " ")
                                                                                (funcall suffix c)))
                                                              cs))))
                        (complete-with-action action cands string pred)))
                    nil t))))
      (cadr (or (gethash pick table) (user-error "No such task"))))))

;;;###autoload
(defun org-varde-insert-link ()
  "Insert a link to a task, or to a project's cairn, picked by its title.
In org, [[varde:SHORT][Title]], which opens the task's view and shows
where it stands; in Markdown, [Title](varde:SHORT); elsewhere varde:SHORT."
  (interactive)
  (pcase-let ((`(,link . ,title) (org-varde--read-link "Link to")))
    (insert (org-varde--link-text link title))))

(defun org-varde--link-text (link title)
  "LINK, described by TITLE, as this buffer writes a link: org's
[[LINK][TITLE]], Markdown's [TITLE](LINK), else LINK alone."
  (let ((title (and title (not (string-blank-p title)) title)))
    (cond ((derived-mode-p 'org-mode) (org-link-make-string link title))
          ((and title (derived-mode-p 'markdown-mode)) (format "[%s](%s)" title link))
          (t link))))

(defun org-varde-copy-link ()
  "Copy a link to the task at point, on the board or in its view, for
\\[yank] to put in; on the view's Project tab, to its project's cairn.
\\[org-insert-link] offers it too."
  (interactive)
  (let ((link (or (org-varde--link-here) (user-error "No task on this line"))))
    (kill-new (org-link-make-string (car link) (cdr link)))
    (setq org-stored-links (cons (list (car link) (cdr link))
                                 (seq-remove (lambda (l) (equal (car l) (car link))) org-stored-links)))
    (message "Copied a link to \"%s\"; %s puts it in" (cdr link)
             (if (org-varde--evil-p) "p" (substitute-command-keys "\\[yank]")))))

(defun org-varde--insert-link (short title)
  "Insert a varde: link to SHORT, described by TITLE, as the first line
of the text of the heading at point, below its planning and drawers."
  (save-excursion
    (org-back-to-heading t)
    (org-end-of-meta-data t)
    (unless (bolp) (insert "\n"))
    (insert (org-link-make-string (concat "varde:" short)
                                  (and title (not (string-blank-p title)) title))
            "\n")))

(defcustom org-varde-close-with-heading 'ask
  "Whether marking a heading done closes the open tasks its text links.
Its own text, not its children's.  `ask' asks once for them all."
  :type '(choice (const ask) (const always) (const never)))

(defun org-varde--entry-links ()
  "The tasks the varde: links of the heading at point name, by short id.
In the heading and its own text, not its children."
  (save-excursion
    (org-back-to-heading t)
    (let ((end (save-excursion (outline-next-heading) (point)))
          found)
      (while (re-search-forward "varde:\\([[:alnum:]]\\{4,\\}\\)" end t)
        (let ((short (org-varde--short (match-string-no-properties 1))))
          ;; Not a project's cairn, varde:project:DIR.
          (unless (or (eq (char-after) ?:) (member short found)) (push short found))))
      (nreverse found))))

(defun org-varde--entry-rows ()
  "The rows of the open tasks the heading at point links, as
`org-varde--entry-links' finds them.  For a user's action: a task with
no row fetched yet has every task's fetched now, once."
  (let ((links (org-varde--entry-links)))
    (when (and links (not (seq-every-p #'org-varde--link-row links)))
      (let ((data (org-varde--varde-json "list" "--json" "--all")))
        (when (org-varde--readable data) (setq org-varde--all (org-varde--by-short data)))))
    (seq-filter (lambda (row) (equal (plist-get row :state) "open"))
                (delq nil (mapcar #'org-varde--link-row links)))))

(defun org-varde--on-done ()
  "Offer to close the open tasks a heading just marked done links to, as
`org-varde-close-with-heading' says.  Nothing is written to org: a task
closed or reopened elsewhere leaves the heading as it is."
  (when (and (not (eq org-varde-close-with-heading 'never))
             (member org-state org-done-keywords)
             (not (member org-last-state org-done-keywords))
             ;; A repeat comes round again: its tasks are not done.
             (not (org-get-repeat)))
    (let ((rows (org-varde--entry-rows)))
      (when (and rows
                 (or (eq org-varde-close-with-heading 'always)
                     (y-or-n-p (format "Close %s%s as done? "
                                       (org-varde--named (plist-get (car rows) :task) (plist-get (car rows) :title))
                                       (if (cdr rows) (format " (and %d more)" (length (cdr rows))) "")))))
        ;; Esc on a busy session's question leaves it, and those after it, open.
        (condition-case nil
            (dolist (row rows)
              (org-varde--finish (plist-get row :task) (plist-get row :title)))
          (quit (message "Quit: the heading is done, the rest of its tasks open")))))))

;;;###autoload
(defun org-dblock-write:varde (params)
  "A table of varde's tasks for a review, in a #+BEGIN: varde block,
filled by \\[org-ctrl-c-ctrl-c] on it.  PARAMS: :project DIR, or all (the
default); :state open, done or all (the default); :closed 7d, closed
tasks only from the last 7 days (h hours, d days, w weeks; d unless
said).  Each row says where its task stands, links it, and gives its
project, its cairn's Status, cut to 50 columns, and the day it was
closed."
  (let* ((word (lambda (key default) (format "%s" (or (plist-get params key) default))))
         (true (lambda (d) (directory-file-name (file-truename (expand-file-name d)))))
         (project (funcall word :project "all"))
         (dir (unless (equal project "all") (funcall true project)))
         (state (funcall word :state "all"))
         (closed-in (funcall word :closed "0"))
         (since (if (string-match "\\`\\([0-9]+\\)\\([hdw]?\\)\\'" closed-in)
                    (let ((n (string-to-number (match-string 1 closed-in))))
                      (and (> n 0)
                           (- (* 1000 (float-time))
                              (* n 1000 (pcase (match-string 2 closed-in) ("h" 3600) ("w" 604800) (_ 86400))))))
                  (user-error "varde block: :closed takes a number of hours, days or weeks, as 24h, 7d or 2w")))
         (data (if (member state '("open" "done" "all"))
                   (org-varde--varde-json "list" "--json" "--all")
                 (user-error "varde block: :state takes open, done or all")))
         (projects (make-hash-table :test #'equal))
         (cell (lambda (text) (replace-regexp-in-string "|" "\\\\vert{}" (or text ""))))
         (any (null dir))
         lines)
    (unless (org-varde--readable data) (user-error "Update org-varde to read this varde's tasks"))
    (dolist (g (plist-get data :groups))
      (dolist (r (plist-get g :rows))
        (let ((closed (plist-get r :closed))
              (p (plist-get r :project)))
          (when (or (null dir)
                    (and p (equal dir (with-memoization (gethash p projects) (funcall true p)))
                         (setq any t)))
            (when (and (pcase state
                         ("open" (equal (plist-get r :state) "open"))
                         ("done" (equal (plist-get r :state) "done"))
                         (_ t))
                       (or (null since) (null closed) (>= closed since)))
              (push (format "\n| %s | %s | %s | %s | %s |"
                            (org-varde--status-mark r)
                            (org-link-make-string (concat "varde:" (plist-get r :short))
                                                  (funcall cell (plist-get r :title)))
                            (or (org-varde--project-name p) "")
                            (funcall cell (truncate-string-to-width (or (plist-get r :cairn_status) "")
                                                                    50 nil nil "…"))
                            (if closed (format-time-string "%F" (/ closed 1000)) ""))
                    lines))))))
    (if (not any)
        ;; Not an empty table: a project named wrong says so.
        (insert "No tasks in " (abbreviate-file-name dir))
      (insert "| | Task | Project | Status | Closed |\n|-")
      (apply #'insert (nreverse lines))
      (org-table-align))))

(defun org-varde--convert-buffer ()
  "`org-varde-convert-links' in this buffer: how many headings it changed."
  (let ((n 0)
        (re (format "^[ \t]*:%s:[ \t]+\\(\\S-+\\)" org-varde-property)))
    (org-with-wide-buffer
     (goto-char (point-max))
     (while (re-search-backward re nil t)
       (let ((task (match-string-no-properties 1)))
         (unless (org-before-first-heading-p)
           (org-back-to-heading t)
           (when (equal (org-entry-get nil org-varde-property) task)
             (org-entry-delete nil org-varde-property)
             (org-varde--insert-link (org-varde--short task) (org-varde--title))
             ;; The task is the board's again: a model may name it.
             (ignore-error org-varde-varde-error (org-varde--varde "unlink" task))
             (setq n (1+ n)))))))
    n))

;;;###autoload
(defun org-varde-convert-links (&optional everywhere)
  "Turn the VARDE_TASK properties an earlier org-varde wrote into links.
Each heading with one gets a varde: link to its task, described by its
title, as the first line of its text, and loses the property; varde
forgets the link (`varde unlink').  In this buffer, or with a prefix
EVERYWHERE in the agenda files and the org files under `org-directory'.
Nothing is saved for you; a file opened for it and left unchanged is
closed again."
  (interactive "P")
  (let ((buffers (if everywhere
                     (delete-dups
                      (mapcar #'file-truename
                              (append (org-agenda-files t)
                                      (and org-directory (file-directory-p org-directory)
                                           (directory-files-recursively
                                            org-directory "\\.org\\(?:_archive\\)?\\'" nil
                                            (lambda (d) (not (string-prefix-p "." (file-name-nondirectory d)))))))))
                   (list (current-buffer))))
        (headings 0)
        (files 0))
    (dolist (b buffers)
      (let* ((opened (and (stringp b) (not (find-buffer-visiting b))))
             (b (if (stringp b) (find-file-noselect b) b))
             (n (with-current-buffer b
                  (if (derived-mode-p 'org-mode) (org-varde--convert-buffer) 0))))
        (if (> n 0)
            (setq headings (+ headings n) files (1+ files))
          (when opened (kill-buffer b)))))
    (if (zerop headings)
        (message "No heading has a VARDE_TASK to convert")
      (message "Converted %d heading%s in %d file%s; nothing is saved: C-x s saves them"
               headings (if (= headings 1) "" "s") files (if (= files 1) "" "s")))))

;;; --- Memory ----------------------------------------------------------------

(defun org-varde--task-here (&optional none)
  "The varde task here: the board row, the buffer's task, the varde: link
at point, or a heading's VARDE_TASK, from before links.  NONE is what to
say when there is none."
  (or (get-text-property (point) 'org-varde-task)
      org-varde--task
      (org-varde--link-at-point)
      (and (org-varde--heading-p) (org-entry-get nil org-varde-property))
      (user-error (or none "No varde task here"))))

(defvar-local org-varde--project-dir nil
  "The project directory whose cairn the view's Project tab shows.")

(defvar-local org-varde--tab 'task
  "The view's tab: `task', the task and its cairn, or `project', the
cairn of its project.")

(defvar-local org-varde--task-data nil
  "The task in view, as `varde show --json' last gave it.")

(defvar-local org-varde--editing nil
  "The cairn being edited, as `varde edit' is told it.
A task, or \"--project\" and its directory.")

(defvar-local org-varde--shown nil
  "What the view's tab shows, as `varde show --json' gave it.")

(defvar-local org-varde--shown-out nil
  "The text of the last `varde show --json' a refresh drew this view from.")

(defvar-local org-varde--folded nil
  "The sections folded in this view.")

(defvar-local org-varde--removed nil
  "The line last removed here, for bringing back: (EDITING . ID), or
\(EDITING status . TEXT) once the Status TEXT was handed back.")

(defvar-local org-varde--hints nil
  "The keys the view's header line names.")

(defconst org-varde-show-buffer "*varde show*"
  "The view's buffer name.")

;;;###autoload
(defun org-varde-show (&optional task)
  "Show TASK in the view: its cairn first, then its context, sessions and git.
Its git status, which can take a second, follows when varde has it.
The cairn is edited there, a line at a time.  The view has two tabs:
the task, and its project's cairn.  An org link [[varde:TASK]], TASK
being its id or short id, shows it too."
  (interactive)
  (org-varde--show-fill (or task (org-varde--task-here "No varde task here; org-varde-new-here makes one"))
                        'task)
  (org-varde--pop-view))

(defun org-varde--pop-view ()
  "Show the view: in its window, else in one below the selected window,
most of the frame's height.  `display-buffer-alist' can say otherwise."
  (pop-to-buffer org-varde-show-buffer
                 '((display-buffer-reuse-window display-buffer-below-selected)
                   (window-height . 0.6))))

(defun org-varde--show-fill (task &optional tab)
  "Fill the view with TASK at once, on TAB, and its git status when varde
has it.  TAB nil keeps the tab the view is on.  TASK may be its short
id: the view keeps the one varde gives."
  (let* ((data (org-varde--details task))
         (task (or (plist-get data :id) task)))
    (org-varde--view task data tab)
    (org-varde--varde-async (list "show" task "--json")
                            (lambda (out)
                              (let ((b (get-buffer org-varde-show-buffer)))
                                (when (and b (equal (buffer-local-value 'org-varde--task b) task))
                                  (with-current-buffer b
                                    ;; Only git: the cairn may have been edited meanwhile.
                                    (setq org-varde--task-data
                                          (plist-put org-varde--task-data :git (plist-get (org-varde--json out) :git)))
                                    (when (eq org-varde--tab 'task)
                                      (setq org-varde--shown org-varde--task-data)
                                      (org-varde--view-render)))))))))

(defun org-varde--view (task data tab &optional dir)
  "Show TASK, as DATA gives it, in the view, on TAB: nil keeps the tab it
is on.  With no TASK, DATA is the cairn of the project DIR, shown alone."
  (with-current-buffer (get-buffer-create org-varde-show-buffer)
    (unless (derived-mode-p 'org-varde-show-mode) (org-varde-show-mode))
    (let ((dir (if task (plist-get data :project) dir)))
      (unless (and (equal task org-varde--task) (equal dir org-varde--project-dir))
        (setq org-varde--folded nil
              org-varde--removed nil)
        ;; Another task's view opens on its cairn's first line.
        (let ((inhibit-read-only t)) (erase-buffer)))
      (setq org-varde--task task
            org-varde--project-dir dir
            org-varde--task-data (and task data)
            org-varde--tab (cond ((not task) 'project) (tab) (t org-varde--tab))
            org-varde--hints (org-varde--view-hints))
      (org-varde--view-tab (unless task data)))))

(defun org-varde--view-tab (&optional project)
  "Draw the view's tab.  PROJECT, what `varde show --project --json' says
of its project, saves asking varde."
  (if (eq org-varde--tab 'task)
      (setq org-varde--shown org-varde--task-data
            org-varde--editing (list org-varde--task))
    (setq org-varde--shown (or project
                               (and org-varde--project-dir
                                    (org-varde--varde-json "show" "--project" org-varde--project-dir "--json")))
          org-varde--editing (list "--project" org-varde--project-dir)))
  (setq org-varde--shown-out nil)
  (org-varde--view-render))

(defun org-varde--view-hints ()
  "The keys the view's header line names: what is used most."
  (substitute-command-keys
   (concat "\\<org-varde-show-mode-map>"
           (if org-varde--task "\\[org-varde-show-go] go  " "")
           "\\[org-varde-cairn-edit] edit  \\[org-varde-cairn-add] add  \\[org-varde-cairn-remove] remove  "
           (if org-varde--task
               "\\[org-varde-show-previous-tab] \\[org-varde-show-next-tab] tabs  "
             "\\[org-varde-cairn-edit-all] all as text  ")
           "\\[org-varde-show-menu] help")))

(defface org-varde-tab '((t :inherit bold :inverse-video t))
  "The tab on view in a view's header line."
  :group 'org-varde)

(defun org-varde--view-header ()
  "The view's header line: its tabs, the one on view highlighted, then
its keys."
  (let ((tab (lambda (name key)
               (propertize (concat " " name " ") 'face
                           (if (eq org-varde--tab key) 'org-varde-tab 'org-varde-quiet)))))
    (let ((tabs (concat (if org-varde--task (concat (funcall tab "Task" 'task) " ") "")
                        (funcall tab "Project" 'project) "   ")))
      ;; In a narrow window, the key that lists them all.
      (concat tabs (if (<= (string-width (concat tabs org-varde--hints)) (window-width))
                       org-varde--hints
                     (substitute-command-keys "\\<org-varde-show-mode-map>\\[org-varde-show-menu] help"))))))

(defun org-varde-show-next-tab ()
  "Show the view's other tab: the task, or its project's cairn."
  (interactive)
  (unless org-varde--task
    (user-error "This view shows a project cairn alone; v on a task shows its Task tab too"))
  (setq org-varde--tab (if (eq org-varde--tab 'task) 'project 'task))
  ;; Each tab opens on its cairn's first line.
  (let ((inhibit-read-only t)) (erase-buffer))
  (org-varde--view-tab))

(defun org-varde-show-previous-tab ()
  "Show the view's other tab; with two, as \\[org-varde-show-next-tab] does."
  (interactive)
  (org-varde-show-next-tab))

(defun org-varde--refresh-views ()
  "Bring the view, when in sight, up to date with varde, without waiting.
Its git is left as it was: looking again takes a while."
  (let ((b (get-buffer org-varde-show-buffer)))
    (when (and b (get-buffer-window b t))
      (let* ((task (buffer-local-value 'org-varde--task b))
             (dir (buffer-local-value 'org-varde--project-dir b))
             (tab (buffer-local-value 'org-varde--tab b))
             (args (if (eq tab 'task) (list "show" task "--json" "--no-git") (list "show" "--project" dir "--json"))))
        (when (or (eq tab 'task) dir)
          (org-varde--varde-async
           args
           (lambda (out)
             (when (buffer-live-p b)
               (with-current-buffer b
                 ;; Still on what it was asked for, and changed.
                 (when (and (equal task org-varde--task) (equal dir org-varde--project-dir)
                            (eq tab org-varde--tab) (not (equal out org-varde--shown-out)))
                   (let ((data (org-varde--json out)))
                     (when (eq tab 'task)
                       (when (plist-member org-varde--task-data :git)
                         (setq data (plist-put data :git (plist-get org-varde--task-data :git))))
                       (setq org-varde--task-data data))
                     (setq org-varde--shown data
                           org-varde--shown-out out)
                     (org-varde--view-render))))))))))))

(add-hook 'org-varde-after-refresh-hook #'org-varde--refresh-views)

(defun org-varde--view-reload ()
  "Read the view's cairn from varde again, and show it."
  (let ((data (if (eq org-varde--tab 'task)
                  (org-varde--details org-varde--task)
                (org-varde--varde-json "show" "--project" org-varde--project-dir "--json"))))
    (setq org-varde--shown (plist-put org-varde--shown :cairn (plist-get data :cairn)))
    (org-varde--view-render)))

(defface org-varde-heading '((t :inherit bold))
  "Section headings in a view."
  :group 'org-varde)

(defface org-varde-quiet '((t :inherit shadow))
  "What a view says in passing."
  :group 'org-varde)

(defface org-varde-board-blocked '((t :inherit error))
  "A session waiting on a permission decision."
  :group 'org-varde)

(defface org-varde-board-waiting '((t :inherit warning))
  "A session waiting for you."
  :group 'org-varde)

(defface org-varde-board-working '((t :inherit success))
  "A session at work."
  :group 'org-varde)

(defconst org-varde--sections
  '(("findings" "Findings" 7) ("decisions" "Decisions" 5) ("open" "Open" 5)
    ("conventions" "Conventions" 5) ("gotchas" "Gotchas" 7) ("howto" "How-to" 5))
  "A cairn's sections, as (KEY HEADING CAP): KEY is what `varde edit' takes.
The first three are a task's, the others a project's.  CAP is its most
lines, for a varde that does not say its limits.")

(defun org-varde--limit (what)
  "This view's cairn's limit WHAT, as varde says it.
WHAT is `status' or `line', the most words of either, or a section's
key, the most lines it holds."
  (let* ((limits (plist-get org-varde--shown :limits))
         (said (if (memq what '(status line))
                   (plist-get limits (intern (format ":%s" what)))
                 (plist-get (plist-get limits :sections) (intern (concat ":" what))))))
    (or said (pcase what ('status 30) ('line 40) (_ (nth 2 (assoc what org-varde--sections)))))))

(defun org-varde--view-sections ()
  "The sections of the cairn this view's tab shows."
  (if (eq org-varde--tab 'task) (seq-take org-varde--sections 3) (seq-drop org-varde--sections 3)))

(defun org-varde--quiet (text)
  "TEXT in the quiet face."
  (propertize text 'face 'org-varde-quiet))

(defun org-varde--view-heading (key title &optional said gap quiet)
  "Insert the heading of section KEY, TITLE, followed by SAID.
GAP puts a blank line above it, and QUIET, for an empty section, dims
it.  Return whether its section is open."
  (let ((open (not (member key org-varde--folded))))
    (when gap (insert "\n"))
    (insert (propertize (concat (propertize title 'face (if quiet 'org-varde-quiet 'org-varde-heading))
                                (if said (org-varde--quiet (concat " " said)) "")
                                (if open "" (org-varde--quiet " ▸"))
                                "\n")
                        'org-varde-section key))
    open))

(defun org-varde--view-line (text &optional item)
  "Insert TEXT as a line of the section above, wrapped under itself.
ITEM, (SECTION ID TEXT), makes it a line of the cairn to edit."
  (insert (propertize (concat "  " text "\n") 'wrap-prefix "    " 'org-varde-item item)))

(defface org-varde-mine '((t :inherit font-lock-keyword-face))
  "The › that marks a line of a cairn you wrote."
  :group 'org-varde)

(defun org-varde--bullet (mine)
  "What a line of the cairn starts with: a › when MINE, a line you
wrote, else a dash."
  (if mine (propertize "›" 'face 'org-varde-mine) "-"))

(defun org-varde--view-cairn (cairn)
  "Insert CAIRN's sections, each line ready to edit.
A section says how full it is only when it is full, or nearly."
  (dolist (section (org-varde--view-sections))
    (pcase-let* ((`(,key ,title ,_) section)
                 (lines (plist-get cairn (intern (concat ":" key))))
                 (cap (org-varde--limit key)))
      (when (org-varde--view-heading key title
                                     (and (>= (length lines) (1- cap)) (format "%d/%d" (length lines) cap))
                                     nil (null lines))
        (dolist (b lines)
          (let ((text (plist-get b :text)))
            (org-varde--view-line
             (concat (org-varde--bullet (plist-get b :mine)) " " text
                     ;; Its code is gone.
                     (if (plist-get b :stale) (propertize " ?" 'face 'warning) ""))
             (list key (plist-get b :id) text))))))))

(defun org-varde--view-render ()
  "Draw the view's tab from `org-varde--shown', keeping point on its line."
  (let* ((window (get-buffer-window (current-buffer) t))
         ;; Where the view is looked at, though another window is selected.
         (_ (when window (goto-char (window-point window))))
         (data org-varde--shown)
         (cairn (plist-get data :cairn))
         (item (get-text-property (point) 'org-varde-item))
         (section (get-text-property (point) 'org-varde-section))
         (line (line-number-at-pos))
         (inhibit-read-only t))
    (erase-buffer)
    (let ((about (org-varde--view-top)))
      (if (eq org-varde--tab 'task)
          (org-varde--view-task data cairn about)
        (org-varde--view-project data cairn)))
    (goto-char (point-min))
    (let ((match (or (and item (text-property-search-forward
                                'org-varde-item item
                                ;; The same line, by its id; the Status by its section.
                                (lambda (want have) (and have (equal (seq-take want 2) (seq-take have 2))))))
                     (and section (text-property-search-forward 'org-varde-section section t)))))
      (cond (match (goto-char (prop-match-beginning match)))
            ;; Drawn afresh, as the terminal: on the cairn's first
            ;; bullet, else its first line.
            ((= line 1)
             (let ((bullet (text-property-search-forward 'org-varde-item nil (lambda (_ have) (nth 1 have)))))
               (goto-char (if bullet (prop-match-beginning bullet)
                            (or (next-single-property-change (point-min) 'org-varde-item) (point-min))))))
            (t (forward-line (1- line)))))
    (when window (set-window-point window (point)))
    (org-varde--highlight)))

(defun org-varde--highlight ()
  "Put `hl-line-mode''s highlight back on point's line, after a redraw.
Erasing the buffer took it away, and it comes back only after a command."
  (when (bound-and-true-p hl-line-mode)
    (let ((window (get-buffer-window (current-buffer) t)))
      (save-excursion
        (when window (goto-char (window-point window)))
        (hl-line-highlight)))))

(defun org-varde--status-face (status)
  "The face of a session's STATUS, as the board colours it."
  (pcase status
    ((or "working" "delegated") 'org-varde-board-working)
    ("waiting" 'org-varde-board-waiting)
    ("blocked" 'org-varde-board-blocked)
    (_ 'org-varde-quiet)))

(defun org-varde--project-name (dir)
  "The project DIR by its name, as the board says it."
  (and dir (file-name-nondirectory (directory-file-name dir))))

(defconst org-varde--about-cairn
  '("cairn: what its sessions learned, handed to the next one" "cairn: what its sessions learned")
  "What a cairn is, said at the right of the Task tab where it fits: the
long way, else the short.")

(defun org-varde--with-about (line about)
  "LINE with the first of ABOUT that fits beside it at the view's right
edge, quiet; nil when none does."
  (let* ((window (get-buffer-window (current-buffer) t))
         (width (window-width window))
         (fit (seq-find (lambda (a) (<= (+ (string-width line) 4 (string-width a)) width)) about)))
    (and fit (concat line (propertize " " 'display `(space :align-to (- right ,(1+ (string-width fit)))))
                     (org-varde--quiet fit)))))

(defun org-varde--view-top ()
  "Insert what the view is of, and its facts on one quiet line.
The task's title, where it stands, its project and worktree, and what a
cairn is where it fits; on the Project tab, what a project cairn is.
Return what a cairn is when it did not fit, for the line below."
  (let ((task org-varde--task-data)
        (name (org-varde--project-name org-varde--project-dir)))
    ;; The Project tab is headed by its project, as the terminal's is.
    (if (and task (eq org-varde--tab 'task))
        (insert (propertize (or (plist-get task :title) "") 'face 'org-varde-heading)
                (if (plist-get task :short) (org-varde--quiet (concat "  " (plist-get task :short))) "")
                "\n")
      (insert (propertize (or name "") 'face 'org-varde-heading) "\n"))
    (if (eq org-varde--tab 'task)
        (let* ((facts (org-varde--view-facts task))
               (line (org-varde--with-about facts org-varde--about-cairn)))
          (insert (or line facts) "\n")
          (unless line org-varde--about-cairn))
      (insert (org-varde--quiet (format "project cairn: what %s's tasks learned, handed to each new session"
                                        (or name "its project")))
              "\n")
      nil)))

(defun org-varde--view-facts (data)
  "Where the task DATA shows stands, on one line, as the terminal's view
has it: its row's meta on the board (a live session's status, else the
task's state; what it left running; where it came from; git; the
project), then the Inbox, its worktree's branch and its link."
  (let* ((row (org-varde--row (plist-get data :id)))
         (status (plist-get row :status))
         (git (plist-get row :git))
         (label (plist-get git :label))
         (worktree (plist-get data :worktree))
         (count (lambda (k what)
                  (let ((n (or (plist-get row k) 0)))
                    (and (> n 0) (format "%d %s%s" n what (if (= n 1) "" "s"))))))
         (parts
          (delq nil
                (list (if (member status '("working" "delegated" "waiting" "blocked"))
                          (propertize (org-varde--status-word status) 'face (org-varde--status-face status))
                        (or (plist-get row :state) (plist-get data :state)))
                      (funcall count :agents "agent")
                      (funcall count :shells "shell")
                      (let ((origin (plist-get row :origin)))
                        (and origin (concat "← " (plist-get origin :short))))
                      (and label (not (string-empty-p label))
                           (propertize label 'face
                                       (cond ((equal (plist-get git :checks) "failing") 'org-varde-board-blocked)
                                             ((equal (plist-get git :stage) "edited") 'org-varde-board-waiting)
                                             ((equal (plist-get git :stage) "landed") 'org-varde-board-working)
                                             (t 'org-varde-quiet))))
                      (org-varde--project-name (or (plist-get row :project) (plist-get data :project)))
                      (and (plist-get row :inbox) "in the Inbox")
                      (and (equal (plist-get worktree :state) "active") (plist-get worktree :branch)
                           (concat "worktree " (plist-get worktree :branch)))
                      (let ((link (plist-get row :external)))
                        (and link (concat "linked to " link)))))))
    (mapconcat (lambda (p) (if (get-text-property 0 'face p) p (org-varde--quiet p)))
               parts (org-varde--quiet " · "))))

(defun org-varde--view-usage (usage &optional about)
  "Insert how a cairn is used, as USAGE says, on one quiet line.
When it last changed, how many sessions it was handed to, and how many
of its lines are stale, when any is; ABOUT, what a cairn is, at its
right where it fits."
  (let* ((updated (plist-get usage :updated))
         (n (or (plist-get usage :sessions) 0))
         (stale (or (plist-get usage :stale) 0))
         (parts (and updated
                     (list (format "updated %s ago" (org-varde--age (- (truncate (* 1000 (float-time))) updated)))
                           (if (> n 0)
                               (format "handed to %d session%s" n (if (= n 1) "" "s"))
                             "not handed to a session yet")))))
    (when (or parts (> stale 0))
      (let ((line (concat (org-varde--quiet (string-join parts " · "))
                          (if (> stale 0)
                              (concat (if parts (org-varde--quiet " · ") "")
                                      (propertize (format "%d stale" stale) 'face 'warning))
                            ""))))
        (insert (or (and about (org-varde--with-about line about)) line) "\n")))))

(defun org-varde--cairn-empty-p (cairn)
  "Whether CAIRN, of the view's tab, has no Status and no line."
  (and (string-empty-p (or (plist-get cairn :status) ""))
       (not (seq-some (lambda (s) (plist-get cairn (intern (concat ":" (car s)))))
                      (org-varde--view-sections)))))

(defun org-varde--view-task (data cairn &optional about)
  "Insert the task DATA shows, CAIRN first, and ABOUT, what a cairn is,
where it fits."
  (org-varde--view-usage (plist-get data :usage) about)
  ;; An empty cairn is its hint and its sections' headings, as the
  ;; terminal has it: no Status yet.
  (if (org-varde--cairn-empty-p cairn)
      (insert "\n" (org-varde--quiet "No cairn yet: its first session's update writes one; a adds a line.") "\n")
    (let ((status (plist-get cairn :status)))
      (insert "\n"
              (propertize (concat (propertize "Status" 'face 'org-varde-heading)
                                  (if (plist-get cairn :status_mine) (concat " " (org-varde--bullet t) " ") "  ")
                                  (if (string-empty-p (or status "")) (org-varde--quiet "none yet") status)
                                  "\n")
                          'wrap-prefix (if (plist-get cairn :status_mine) "         " "        ")
                          'org-varde-item (list "status" nil (or status ""))))))
  (org-varde--view-cairn cairn)
  (let ((notes (plist-get data :notes)))
    (when (and notes (org-varde--view-heading "notes" "Notes for the next update" nil t))
      (dolist (n notes)
        (org-varde--view-line (org-varde--quiet (concat "- " (if (stringp n) n (plist-get n :text))))))))
  (let ((body (plist-get data :body)))
    (when (and body (not (string-blank-p body))
               (org-varde--view-heading "context" "Context" nil t))
      (dolist (l (split-string (string-trim-right body) "\n"))
        ;; Where `org-varde-new-here' made it: RET goes there.
        (if (string-match (concat "\\`From: " org-link-bracket-re) l)
            (let ((start (point)))
              (org-varde--view-line (concat (propertize "From" 'face 'org-varde-heading) "  "
                                            (propertize (or (match-string 2 l) (match-string 1 l)) 'face 'org-link)))
              (put-text-property start (point) 'org-varde-from (substring l 6)))
          (org-varde--view-line l)))))
  (let ((runs (plist-get data :runs)))
    (when (and runs (org-varde--view-heading "sessions" "Sessions" nil t))
      (dolist (r (seq-take (reverse runs) 3))
        (org-varde--view-line
         (format "%s · %s %s" (org-varde--status-word (plist-get r :status)) (plist-get r :relation)
                 (org-varde--quiet (format-time-string "%b %e %H:%M" (/ (plist-get r :started) 1000))))))))
  (org-varde--view-git data))

(defun org-varde--status-word (status)
  "STATUS as the board says it."
  (pcase status
    ((or "working" "delegated") "working")
    ("waiting" "your turn")
    ("blocked" "needs input")
    ("exited" "ended")
    (_ status)))

(defun org-varde--view-git (data)
  "Insert where DATA's git work stands, once varde has said."
  (let ((git (plist-get data :git)))
    (when (or git (not (plist-member data :git)))
      (when (org-varde--view-heading "git" "Git" nil t)
        (org-varde--view-line
         (if (not git)
             (org-varde--quiet "looking…")
           (string-join
            (delq nil (list (plist-get git :stage)
                            (concat "on " (or (plist-get git :branch) "?")
                                    (if (plist-get git :shared) ", shared with other tasks" ""))
                            (let ((a (or (plist-get git :ahead) 0)) (b (or (plist-get git :behind) 0)))
                              (and (plist-get git :target) (> (+ a b) 0)
                                   (format "%d ahead, %d behind %s" a b (plist-get git :target))))
                            (let ((pr (plist-get git :pr)))
                              (and pr (format "PR #%s %s%s" (plist-get pr :number) (plist-get pr :state)
                                              (if (plist-get pr :checks) (concat ", checks " (plist-get pr :checks)) ""))))))
            " · ")))))))

(defun org-varde--view-project (data cairn)
  "Insert the project cairn DATA shows, CAIRN."
  (if (not data)
      (insert "\n" (org-varde--quiet "This task has no project yet: its first start picks one.\n"))
    (org-varde--view-usage (plist-get data :usage))
    (when (org-varde--cairn-empty-p cairn)
      (insert (org-varde--quiet "No project cairn yet: tasks closed as done add to it; a adds a line.\n")))
    (insert "\n")
    (org-varde--view-cairn cairn)))

(defun org-varde-show-toggle ()
  "Fold or unfold the section at point."
  (interactive)
  (let ((key (save-excursion
               (end-of-line)
               (let ((match (text-property-search-backward 'org-varde-section nil nil)))
                 (if match (prop-match-value match) (user-error "Not in a section"))))))
    (setq org-varde--folded (if (member key org-varde--folded)
                                (delete key org-varde--folded)
                              (cons key org-varde--folded)))
    (goto-char (or (text-property-any (point-min) (point-max) 'org-varde-section key) (point)))
    (org-varde--view-render)))

(defun org-varde--view-stop-p ()
  "Whether this line is one the view's moves stop at: a heading, a line,
or where the task came from."
  (or (get-text-property (point) 'org-varde-section)
      (get-text-property (point) 'org-varde-item)
      (get-text-property (point) 'org-varde-from)))

(defun org-varde-show-next (&optional n)
  "Move to the Nth next heading or line of the cairn."
  (interactive "p")
  (let ((step (if (< (or n 1) 0) -1 1)) (left (abs (or n 1))) found)
    (save-excursion
      (beginning-of-line)
      (while (and (> left 0) (zerop (forward-line step)) (not (eobp)))
        (when (org-varde--view-stop-p)
          (setq found (point) left (1- left)))))
    (if found (goto-char found) (user-error "No more"))))

(defun org-varde-show-previous (&optional n)
  "Move to the Nth previous heading or line of the cairn."
  (interactive "p")
  (org-varde-show-next (- (or n 1))))

(defun org-varde-show-first ()
  "Move to the first heading or line."
  (interactive)
  (goto-char (point-min))
  (unless (org-varde--view-stop-p) (org-varde-show-next)))

(defun org-varde-show-last ()
  "Move to the last heading or line."
  (interactive)
  (goto-char (point-max))
  (org-varde-show-previous))

(defun org-varde--board-tasks ()
  "The tasks in the order the board lists them, in the project it keeps
to; those in a folded group too."
  (seq-mapcat (lambda (g)
                (seq-keep (lambda (r) (and (or (null org-varde--kept) (equal (plist-get r :project) org-varde--kept))
                                           (plist-get r :task)))
                          (plist-get g :rows)))
              (plist-get org-varde-board-data :groups)))

(declare-function org-varde-board--unfold "org-varde-board" (key))
(declare-function org-varde-board-reopen "org-varde-board" ())

(defun org-varde--board-select (task)
  "Put the board on TASK's row, in every window showing it.
A folded group it is in opens, as TAB opens Needs you."
  (when-let* ((board (get-buffer org-varde-board-buffer)))
    (with-current-buffer board
      (let ((find (lambda () (save-excursion (goto-char (point-min))
                                             (text-property-search-forward 'org-varde-task task t)))))
        (unless (funcall find)
          (when-let* ((group (seq-find (lambda (g) (seq-find (lambda (r) (equal (plist-get r :task) task))
                                                             (plist-get g :rows)))
                                       (plist-get org-varde-board-data :groups))))
            (org-varde-board--unfold (plist-get group :key))))
        (when-let* ((m (funcall find)))
          (goto-char (prop-match-beginning m))
          (dolist (w (get-buffer-window-list board nil t))
            (set-window-point w (point))))))))

(defun org-varde--view-step (task)
  "Show TASK in this view, on the tab it is on, and put the board on it."
  (org-varde--show-fill task)
  (org-varde--board-select task))

(defun org-varde--view-task-only ()
  "Refuse in a view of a project cairn alone, which shows no task."
  (unless org-varde--task
    (user-error "This is a project cairn: %s and TAB step through tasks in a task's view"
                (if (org-varde--evil-p) "C-j, C-k" "J, K"))))

(defun org-varde-show-next-task (&optional n)
  "Show the Nth next task, as the board lists them, keeping the view open."
  (interactive "p")
  (org-varde--view-task-only)
  (let* ((tasks (org-varde--board-tasks))
         (i (seq-position tasks org-varde--task))
         (to (cond (i (+ i n)) ((> n 0) 0) (t (1- (length tasks))))))
    (unless (and (<= 0 to) (< to (length tasks)))
      (user-error "No more tasks"))
    (org-varde--view-step (nth to tasks))))

(defun org-varde-show-previous-task (&optional n)
  "Show the Nth previous task, as the board lists them, keeping the view open."
  (interactive "p")
  (org-varde-show-next-task (- n)))

(defun org-varde--nothing-needs-you ()
  "What to say when nothing in the board's project needs you."
  (let ((elsewhere (- (length (org-varde--needs-you)) (length (org-varde--needs-you-here)))))
    (if (> elsewhere 0)
        (format "Nothing here needs you; %d in other projects: . shows them" elsewhere)
      "Nothing needs you")))

(defun org-varde-show-next-needs-you (&optional n)
  "Show the Nth next task that needs you, round to the first, keeping the
view open.  Negative N goes back, round to the last."
  (interactive "p")
  (org-varde--view-task-only)
  (let* ((tasks (mapcar (lambda (r) (plist-get r :task)) (org-varde--needs-you-here)))
         (len (length tasks))
         (i (seq-position tasks org-varde--task)))
    (if (zerop len)
        (message "%s" (org-varde--nothing-needs-you))
      (let ((to (mod (cond (i (+ i n)) ((> n 0) (1- n)) (t n)) len)))
        (org-varde--view-step (nth to tasks))
        (when (> len 1) (message "Needs you: %d of %d" (1+ to) len))))))

(defun org-varde-show-tab ()
  "On a section's heading, fold or unfold it, as TAB does in org; else
show the next task that needs you."
  (interactive)
  (if (get-text-property (point) 'org-varde-section)
      (org-varde-show-toggle)
    (org-varde-show-next-needs-you)))

(defun org-varde-show-previous-needs-you (&optional n)
  "Show the Nth previous task that needs you, round to the last."
  (interactive "p")
  (org-varde-show-next-needs-you (- n)))

(defun org-varde--item ()
  "The line of the cairn at point, as (SECTION ID TEXT), or nil."
  (get-text-property (point) 'org-varde-item))

(defvar org-varde--drafts nil
  "Lines of a cairn left unsent when their prompt was quit, so no typing
is lost: ((EDITING . TARGET) . TEXT).  EDITING is as
`org-varde--editing', and TARGET (\"add\" SECTION), (\"set\" ID) or
\(\"status\").")

(defvar org-varde--typed nil
  "What the prompt of a cairn's line holds, as you type it.")

(defun org-varde--read-line (prompt cap &optional initial note)
  "Read a line of the cairn with PROMPT, its words counted against CAP at
the right.  INITIAL is the text to start from.  NOTE, when given, says
something of it beside the count, such as why varde refused it, until
you change it."
  (setq org-varde--typed initial)
  (minibuffer-with-setup-hook
      (lambda ()
        (let* ((count (make-overlay (point-max) (point-max) nil t t))
               (show (lambda ()
                       (let* ((text (minibuffer-contents))
                              (n (length (split-string text nil t)))
                              (said (format "%d/%d" n cap)))
                         (unless (equal text (or initial "")) (setq note nil))
                         (setq org-varde--typed text)
                         (move-overlay count (point-max) (point-max))
                         (overlay-put count 'after-string
                                      (concat (if note (concat "  " note) "")
                                              " "
                                              (propertize " " 'display `(space :align-to (- right ,(1+ (length said)))))
                                              (propertize said 'face (if (> n cap) 'error 'org-varde-quiet))))))))
          (funcall show)
          (add-hook 'post-command-hook show nil t)))
    (read-string prompt initial)))

(defun org-varde--line-ids (cairn)
  "The ids of CAIRN's lines."
  (seq-mapcat (lambda (s) (mapcar (lambda (b) (plist-get b :id)) (plist-get cairn (intern (concat ":" (car s))))))
              org-varde--sections))

(defun org-varde--change (args &optional text)
  "Change this view's cairn with `varde edit' and ARGS, and show it.
TEXT, when given, goes on stdin for the `-' in ARGS, so a line may start
with a dash.  A line that is new, as one added or brought back, gets
point.  Return nil, or varde's message when it refused."
  (condition-case e
      (let* ((out (org-varde--json (org-varde--call text (append '("edit") org-varde--editing args '("--json")))))
             (cairn (if (plist-member out :cairn) (plist-get out :cairn) out))
             (new (seq-difference (org-varde--line-ids cairn)
                                  (org-varde--line-ids (plist-get org-varde--shown :cairn)))))
        (setq org-varde--shown (plist-put org-varde--shown :cairn cairn))
        (org-varde--view-render)
        (when-let* ((id (and (= (length new) 1) (car new)))
                    (m (save-excursion
                         (goto-char (point-min))
                         (text-property-search-forward 'org-varde-item id (lambda (id item) (equal id (cadr item)))))))
          (goto-char (prop-match-beginning m))
          (org-varde--highlight))
        nil)
    (org-varde-varde-error (cadr e))))

(defun org-varde--ask-change (prompt cap target initial make)
  "Read a line with PROMPT and CAP, starting from INITIAL, and send it
with the arguments MAKE returns.  TARGET is what it is for, as
`org-varde--drafts' keys it.
Refused, it is asked for again with what you wrote, and why beside it,
and the view is read again, as it may have been behind.  Quit, what you
wrote is kept: in the kill ring, and as a draft offered here next time.
To drop a draft, clear the line and quit."
  (unless (car (last org-varde--editing))
    (user-error "This task has no project yet: its first start picks one"))
  (let* ((key (cons org-varde--editing target))
         (draft (cdr (assoc key org-varde--drafts)))
         (text (or draft initial))
         (note (and draft (org-varde--quiet "your draft, as you left it")))
         why)
    (while (progn
             (setq text (condition-case nil
                            (string-trim (org-varde--read-line prompt cap text note))
                          (quit (org-varde--keep-draft key initial) nil)))
             (cond ((null text) nil)
                   ((string-empty-p text)
                    (org-varde--drop-draft key)
                    (user-error "Nothing written, nothing changed"))
                   ((setq why (org-varde--change (funcall make) text))
                    (org-varde--view-reload)
                    ;; Its first line says what is wrong.
                    (setq note (propertize (car (split-string why "\n")) 'face 'error))
                    t)
                   (t (org-varde--drop-draft key)
                      (message "%s"
                               (pcase target
                                 (`("status")
                                  (substitute-command-keys
                                   "Status set, yours now: updates won\\='t change it; \\<org-varde-show-mode-map>\\[org-varde-cairn-remove] hands it back"))
                                 (`("add" ,section) (format "Added to %s, as yours" (cadr (assoc section org-varde--sections))))
                                 (`("set" ,id) (format "Changed line #%d, as yours" id))))
                      nil))))))

(defun org-varde--keep-draft (key initial)
  "Keep what was typed for KEY as its draft, unless it is nothing, or
INITIAL unchanged."
  (let ((text (string-trim (or org-varde--typed ""))))
    (if (or (string-empty-p text) (equal text (string-trim (or initial ""))))
        (progn (org-varde--drop-draft key)
               (message "Quit"))
      (setf (alist-get key org-varde--drafts nil nil #'equal) text)
      (kill-new text)
      ;; Saying how to take it up again, as the terminal does.
      (message "Draft kept for %s: %s continues it"
               (if (eq org-varde--tab 'task)
                   (org-varde--named org-varde--task (plist-get org-varde--task-data :title))
                 (format "%s's project cairn" (org-varde--project-name org-varde--project-dir)))
               (substitute-command-keys
                (concat "\\<org-varde-show-mode-map>"
                        (pcase (cdr key)
                          (`("status") "\\[org-varde-cairn-edit] on the Status")
                          (`("add" ,section) (concat "\\[org-varde-cairn-add] " (substring section 0 1)))
                          (_ "\\[org-varde-cairn-edit] on the line"))))))))

(defun org-varde--drop-draft (key)
  "Forget the draft for KEY."
  (setq org-varde--drafts (assoc-delete-all key org-varde--drafts)))

(defun org-varde--draft-to-add (section id)
  "Make the draft of line ID, just removed, one for adding to SECTION."
  (let ((draft (assoc (cons org-varde--editing (list "set" id)) org-varde--drafts))
        (add (cons org-varde--editing (list "add" section))))
    (when draft
      (setq org-varde--drafts (delq draft org-varde--drafts))
      (unless (assoc add org-varde--drafts)
        (push (cons add (cdr draft)) org-varde--drafts)))))

(defun org-varde--section-here (&optional status)
  "The cairn section of the line at point, or nil.
With STATUS, \"status\" on the Status line."
  (let ((key (or (car (org-varde--item))
                 (get-text-property (point) 'org-varde-section))))
    (cond ((and status (equal key "status")) key)
          ((assoc key (org-varde--view-sections)) key))))

(defun org-varde--room (section)
  "Refuse at once when SECTION is full, before a line is written for it."
  (let ((n (length (plist-get (plist-get org-varde--shown :cairn) (intern (concat ":" section)))))
        (cap (org-varde--limit section)))
    (when (>= n cap)
      (user-error "%s is full (%d of %d): change or remove one of its lines first"
                  (cadr (assoc section org-varde--sections)) n cap))))

(defun org-varde--pick-section ()
  "Ask for the section of the cairn to add to, a key each.
RET picks the one at point, else Findings, or a project's Conventions."
  (let* ((sections (append (org-varde--view-sections) (and (eq org-varde--tab 'task) '(("status" "Status")))))
         (here (org-varde--section-here t))
         (default (if (assoc here sections) here (caar sections)))
         (key (car (read-multiple-choice
                    "Add to"
                    (append (mapcar (lambda (s) (list (aref (car s) 0) (downcase (cadr s)))) sections)
                            (list (list ?\r (cadr (assoc default sections)))))))))
    (if (eq key ?\r)
        default
      (car (seq-find (lambda (s) (eq (aref (car s) 0) key)) sections)))))

(defun org-varde-cairn-add (section)
  "Add a line of your own to SECTION of the cairn: asked for, a key each,
RET the section at point.  In a task's view, the Status is one of them:
it is set."
  (interactive (list (org-varde--pick-section)))
  (if (equal section "status")
      (org-varde-cairn-status)
    (org-varde--room section)
    (org-varde--ask-change (format "%s · new line: " (cadr (assoc section org-varde--sections)))
                           (org-varde--limit 'line) (list "add" section) nil
                           (lambda () (list "--add" section "-")))))

(defun org-varde-cairn-add-here ()
  "Add a line to the section at point, as vi's o opens one; elsewhere, to
a section you pick."
  (interactive)
  (let ((section (org-varde--section-here)))
    (if section
        (org-varde-cairn-add section)
      (call-interactively #'org-varde-cairn-add))))

(defun org-varde-cairn-edit ()
  "Edit the line of the cairn at point: it becomes yours.
On the Status line, set the Status; on a section's heading, add a line
to it."
  (interactive)
  (pcase (org-varde--item)
    (`("status" . ,_) (org-varde-cairn-status))
    (`(,_ ,id ,text)
     (org-varde--ask-change (format "Edit line #%d: " id) (org-varde--limit 'line) (list "set" id) text
                            (lambda () (list "--set" (number-to-string id) "-"))))
    (_ (org-varde-cairn-add (or (org-varde--section-here)
                                (user-error "No line of the cairn here; a adds one"))))))

(defun org-varde-cairn-status ()
  "Set the task's Status: where it stands now, in a line."
  (interactive)
  (unless (eq org-varde--tab 'task) (user-error "A project cairn has no Status"))
  (org-varde--ask-change "Status: " (org-varde--limit 'status) '("status")
                         (plist-get (plist-get org-varde--shown :cairn) :status)
                         (lambda () (list "--add" "status" "-"))))

(defun org-varde-cairn-remove ()
  "Remove the line of the cairn at point, asking nothing.
Recall still finds it, and \\<org-varde-show-mode-map>\\[org-varde-cairn-undo] brings it back.
On a Status you set, hand it back: it stays, and the next update may
change it."
  (interactive)
  (pcase (org-varde--item)
    (`("status" . ,_)
     (unless (plist-get (plist-get org-varde--shown :cairn) :status_mine)
       (user-error "%s" (substitute-command-keys
                         "The Status is already the updates\\=': \\<org-varde-show-mode-map>\\[org-varde-cairn-edit] sets it as yours")))
     (let ((text (plist-get (plist-get org-varde--shown :cairn) :status))
           (why (org-varde--change (list "--rm" "status"))))
       (when why
         (org-varde--view-reload)
         (user-error "varde: %s" why))
       (setq org-varde--removed (cons org-varde--editing (cons "status" text))))
     (message (substitute-command-keys "Status handed back: the next update may change it; \\<org-varde-show-mode-map>\\[org-varde-cairn-undo] undoes")))
    (`(,section ,id ,_)
     (let ((why (org-varde--change (list "--rm" (number-to-string id)))))
       (when why
         (org-varde--view-reload)
         (user-error "varde: %s" why))
       (org-varde--draft-to-add section id)
       (setq org-varde--removed (cons org-varde--editing id))
       (message (substitute-command-keys "Removed: \\<org-varde-show-mode-map>\\[org-varde-cairn-undo] brings it back, and recall still finds it"))))
    (_ (user-error "No line of the cairn here"))))

(defun org-varde-cairn-undo ()
  "Bring back the line last removed here, where it was, or make a Status
handed back yours again.  With nothing of that to undo, reopen the task
in view when d, P or x closed it here."
  (interactive)
  (let ((id (and (equal (car org-varde--removed) org-varde--editing) (cdr org-varde--removed))))
    (cond
     (id
      (let ((why (if (consp id)
                     (org-varde--change (list "--add" "status" "-") (cdr id))
                   (org-varde--change (list "--restore" (number-to-string id))))))
        (when why
          (org-varde--view-reload)
          (user-error "varde: %s" why))
        (setq org-varde--removed nil)
        (message (if (consp id) "The Status is yours again" "Brought back"))))
     ((and org-varde--task
           (equal (mapcar (lambda (r) (plist-get r :task)) (bound-and-true-p org-varde-board--closed))
                  (list org-varde--task)))
      (org-varde-board-reopen))
     (t (user-error "Nothing removed here to bring back")))))

(defun org-varde-cairn-edit-all ()
  "Edit the whole cairn of the view's tab as text, for changes to many
lines at once."
  (interactive)
  (if (eq org-varde--tab 'task)
      (org-varde-edit-cairn org-varde--task)
    (org-varde-edit-project-cairn org-varde--project-dir)))

(defun org-varde-show-go (&optional arg)
  "Go to the session of the task shown, starting or resuming it if needed.
A closed task is reopened once you confirm.  Prefix ARG is as for
`org-varde-start-task'.  On its From line, go where the task came from."
  (interactive "P")
  (unless org-varde--task (user-error "A project cairn has no session; RET on a task goes to it"))
  (let ((title (plist-get org-varde--task-data :title))
        (from (get-text-property (point) 'org-varde-from)))
    (if from
        (org-link-open-from-string from)
      (org-varde--confirm-reopen org-varde--task title (plist-get org-varde--task-data :state))
      (org-varde-go-task org-varde--task title arg))))

(defun org-varde--view-row ()
  "The task this view shows, as a board row; nil in a project's view.
So the board's keys act on it here."
  (when (and (derived-mode-p 'org-varde-show-mode) org-varde--task)
    (or (org-varde--row org-varde--task)
        (list :task org-varde--task :short (plist-get org-varde--task-data :short)
              :title (plist-get org-varde--task-data :title) :project (plist-get org-varde--task-data :project)
              :state (plist-get org-varde--task-data :state)))))

(defun org-varde--task-view-p ()
  "Whether this view shows a task, not a project's cairn alone."
  (and org-varde--task t))

(defun org-varde--here-state ()
  "The state of the task of the board's line, or of the view."
  (plist-get (or (get-text-property (point) 'org-varde-row) (org-varde--view-row)) :state))

(defun org-varde--can-done-p ()
  "Whether done would change the task here: it is not done already."
  (not (member (org-varde--here-state) '("done" "dropped"))))

(defun org-varde--can-park-p ()
  "Whether park would change the task here: it is not parked already."
  (not (equal (org-varde--here-state) "parked")))

;; The board's keys in a view, for the task it shows.
(autoload 'org-varde-board-start-background "org-varde-board" nil t)
(autoload 'org-varde-board-start-fresh "org-varde-board" nil t)
(autoload 'org-varde-board-start-fork "org-varde-board" nil t)
(autoload 'org-varde-board-done "org-varde-board" nil t)
(autoload 'org-varde-board-park "org-varde-board" nil t)
(autoload 'org-varde-board-drop "org-varde-board" nil t)
(autoload 'org-varde-board-move "org-varde-board" nil t)
(autoload 'org-varde-board-create "org-varde-board" nil t)
(autoload 'org-varde-new-here "org-varde-board" nil t)
(autoload 'org-varde-board-toggle-project "org-varde-board" nil t)
(autoload 'org-varde-board-diff "org-varde-board" nil t)

(defun org-varde--evil-p ()
  "Whether evil has the keys here, for the menus to say evil's."
  (bound-and-true-p evil-local-mode))

(defun org-varde--view-legend ()
  "What the view's marks mean, for its menu."
  (concat "A cairn: what a task's sessions learned, handed to the next one.\n"
          (org-varde--bullet t) " you wrote it, and updates keep it   "
          (propertize "?" 'face 'warning) " stale: its code is gone"))

(transient-define-prefix org-varde-show-menu ()
  "Everything the view does."
  [:description org-varde--view-legend
   ["Line"
    ("e" "edit" org-varde-cairn-edit)
    ("o" "add here" org-varde-cairn-add-here)
    ("a" "add to…" org-varde-cairn-add)
    ("x" "remove, hand back" org-varde-cairn-remove)
    ("u" "undo, reopen" org-varde-cairn-undo)
    ("E" "all as text" org-varde-cairn-edit-all)]
   ["Task"
    :if org-varde--task-view-p
    ("RET" "go to it" org-varde-show-go)
    ("s" "start, stay" org-varde-board-start-background)
    ("R" "fresh" org-varde-board-start-fresh)
    ("F" "fork" org-varde-board-start-fork)
    ("d" "done" org-varde-board-done :if org-varde--can-done-p)
    ("P" "park" org-varde-board-park :if org-varde--can-park-p)
    ("X" "drop" org-varde-board-drop)
    ("m" "move" org-varde-board-move)
    ("y" "copy a link" org-varde-copy-link)]
   ;; Evil's keys in columns of their own: transient binds the keys of
   ;; a hidden suffix, but not those of a hidden column, and g cannot be
   ;; both a key and the start of gr.
   ["Move"
    :if-not org-varde--evil-p
    ("n" "next line" org-varde-show-next)
    ("p" "previous line" org-varde-show-previous)
    ("J" "next task" org-varde-show-next-task :if org-varde--task-view-p)
    ("K" "previous task" org-varde-show-previous-task :if org-varde--task-view-p)
    ("TAB" "needs you" org-varde-show-next-needs-you :if org-varde--task-view-p)]
   ["View"
    :if-not org-varde--evil-p
    ("]" "other tab" org-varde-show-next-tab :if org-varde--task-view-p)
    ("z" "fold, TAB too" org-varde-show-toggle)
    ("/" "recall" org-varde-recall)
    ("." "project/all" org-varde-board-toggle-project)
    ("=" "changes" org-varde-board-diff :if org-varde--task-view-p)
    ("g" "refresh" revert-buffer)
    ("q" "close, v too" quit-window)]
   ["Move"
    :if org-varde--evil-p
    ("j" "next line" org-varde-show-next)
    ("k" "previous line" org-varde-show-previous)
    ("gg" "first" org-varde-show-first)
    ("G" "last" org-varde-show-last)
    ("C-j" "next task" org-varde-show-next-task :if org-varde--task-view-p)
    ("C-k" "previous task" org-varde-show-previous-task :if org-varde--task-view-p)
    ("TAB" "needs you" org-varde-show-next-needs-you :if org-varde--task-view-p)]
   ["View"
    :if org-varde--evil-p
    ("]" "other tab" org-varde-show-next-tab :if org-varde--task-view-p)
    ("za" "fold, TAB too" org-varde-show-toggle)
    ("/" "recall" org-varde-recall)
    ("." "project/all" org-varde-board-toggle-project)
    ("=" "changes" org-varde-board-diff :if org-varde--task-view-p)
    ("gr" "refresh" revert-buffer)
    ("q" "close, K too" quit-window)]])

(defvar-keymap org-varde-show-mode-map
  :doc "A view's keys: the cairn's, and the board's for the task shown.
Under evil, its normal state takes them from here too."
  :parent special-mode-map
  "RET" #'org-varde-show-go
  "n" #'org-varde-show-next
  "p" #'org-varde-show-previous
  "j" #'org-varde-show-next
  "k" #'org-varde-show-previous
  "C-n" #'org-varde-show-next
  "C-p" #'org-varde-show-previous
  "M-<" #'org-varde-show-first
  "M->" #'org-varde-show-last
  "J" #'org-varde-show-next-task
  "K" #'org-varde-show-previous-task
  "TAB" #'org-varde-show-tab
  "<backtab>" #'org-varde-show-previous-needs-you
  "[" #'org-varde-show-previous-tab
  "]" #'org-varde-show-next-tab
  "z" #'org-varde-show-toggle
  "e" #'org-varde-cairn-edit
  "o" #'org-varde-cairn-add-here
  "a" #'org-varde-cairn-add
  "x" #'org-varde-cairn-remove
  "u" #'org-varde-cairn-undo
  "E" #'org-varde-cairn-edit-all
  "s" #'org-varde-board-start-background
  "R" #'org-varde-board-start-fresh
  "F" #'org-varde-board-start-fork
  "d" #'org-varde-board-done
  "P" #'org-varde-board-park
  "X" #'org-varde-board-drop
  "m" #'org-varde-board-move
  "/" #'org-varde-recall
  "." #'org-varde-board-toggle-project
  "=" #'org-varde-board-diff
  "v" #'quit-window
  "y" #'org-varde-copy-link
  "?" #'org-varde-show-menu)

(defun org-varde--evil-keys (map &rest bindings)
  "With evil, give MAP's own keys, and BINDINGS, to its normal state.
Else evil's operators, such as d, would shadow them."
  (when (fboundp 'evil-define-key*)
    (map-keymap-internal (lambda (key def)
                           ;; g and z are evil's prefixes, and v and V its visual states.
                           (unless (or (memq key '(?g ?z ?v ?V))
                                       (and (symbolp key) (string-suffix-p "-state" (symbol-name key))))
                             (evil-define-key* 'normal map (vector key) def)))
                         map)
    (apply #'evil-define-key* 'normal map "q" #'quit-window bindings)))

;; evil-snipe would take s and S.
(with-eval-after-load 'evil-snipe
  (defvar evil-snipe-disabled-modes)
  (dolist (mode '(org-varde-board-mode org-varde-recall-mode org-varde-show-mode))
    (add-to-list 'evil-snipe-disabled-modes mode)))

;; What you write, you write in insert state, as in `org-capture'.
(with-eval-after-load 'evil
  (evil-set-initial-state 'org-varde-new-mode 'insert)
  (evil-set-initial-state 'org-varde-cairn-mode 'insert))

(define-derived-mode org-varde-show-mode special-mode "varde show"
  "A task, its cairn first, and its project's cairn, on two tabs.
The cairn is what the task's sessions learned, handed to the next one.
Edit it here a line at a time: what you write is yours, and updates
keep it.
\\{org-varde-show-mode-map}"
  (setq truncate-lines nil
        word-wrap t
        header-line-format '(:eval (org-varde--view-header)))
  (hl-line-mode 1)
  (setq-local revert-buffer-function
              (lambda (&rest _)
                (if org-varde--task
                    (org-varde--show-fill org-varde--task)
                  (org-varde--project-fill org-varde--project-dir))))
  ;; What a cairn is, said where the window has room.
  (add-hook 'window-size-change-functions
            (lambda (w) (with-current-buffer (window-buffer w) (org-varde--view-render)))
            nil t)
  ;; K, evil's lookup, opened the view: it closes it, and C-j and C-k
  ;; step through tasks, as in evil-collection's lists.
  (org-varde--evil-keys org-varde-show-mode-map
                        "gr" #'revert-buffer
                        "gg" #'org-varde-show-first
                        "G" #'org-varde-show-last
                        "za" #'org-varde-show-toggle
                        "K" #'quit-window
                        "J" nil
                        (kbd "C-j") #'org-varde-show-next-task
                        (kbd "C-k") #'org-varde-show-previous-task))

(defvar-keymap org-varde-cairn-mode-map
  "C-c C-c" #'org-varde-cairn-save
  "<remap> <save-buffer>" #'org-varde-cairn-save
  "C-c C-k" #'org-varde-cairn-cancel
  ;; evil's :w, :wq, :x and ZZ save; :q and ZQ cancel.
  "<remap> <evil-write>" #'org-varde-cairn-save
  "<remap> <evil-save-and-close>" #'org-varde-cairn-save
  "<remap> <evil-save-modified-and-close>" #'org-varde-cairn-save
  "<remap> <evil-quit>" #'org-varde-cairn-cancel)

(define-derived-mode org-varde-cairn-mode text-mode "Cairn"
  "Edit a task's or a project's cairn.  \\[org-varde-cairn-save] saves it.
Lines you add or change are marked yours, and later updates keep them.
Lines starting \"#:\" are varde's notes: they cannot be changed, and
are not saved."
  (setq-local font-lock-defaults '((("^#:.*" . font-lock-comment-face))))
  (add-hook 'kill-buffer-query-functions #'org-varde--keep-edit-p nil t))

(defun org-varde--keep-edit-p ()
  "Whether this buffer may go: yes unless it holds an edit you confirm keeping."
  (or (not (buffer-modified-p))
      (yes-or-no-p "Discard what you wrote here? ")))

(defun org-varde--edit-keys (save cancel)
  "The keys of a buffer you write in, for its header line: SAVE and CANCEL
say what they do.  Under evil, its ex commands."
  (if (bound-and-true-p evil-local-mode)
      (format ":w %s  :q %s" save cancel)
    (format "C-c C-c %s  C-c C-k %s" save cancel)))

(defun org-varde-cairn-cancel ()
  "Stop editing the cairn; changes are discarded once you confirm.
The windows are as they were before it was opened."
  (interactive)
  (org-varde--close-edit))

(defun org-varde--close-edit (&optional keep)
  "Close this buffer, which you were writing in, and its window, so the
windows are as they were before it was opened.  KEEP keeps the buffer.
What you wrote is discarded only once you confirm; until then it stays
in view."
  (if (not (or keep (org-varde--keep-edit-p)))
      (message "Kept what you wrote")
    (unless keep (set-buffer-modified-p nil))
    (let ((window (get-buffer-window)))
      (cond (window (quit-window (not keep) window))
            ((not keep) (kill-buffer))))))

;;;###autoload
(defun org-varde-edit-cairn (&optional task)
  "Edit TASK's whole cairn as text.
Its context is above it, and is saved too, unless another client links
the task (`varde link`)."
  (interactive)
  (let ((task (or task (org-varde--task-here))))
    (with-current-buffer (org-varde--edit (format "*cairn %s*" (org-varde--short task)) (list task))
      (setq-local org-varde--task task))))

;;;###autoload
(defun org-varde-edit-project-cairn (&optional directory)
  "Edit the whole cairn of the project DIRECTORY is in as text."
  (interactive)
  (let ((dir (or directory org-varde--project-dir (org-varde--project-here))))
    (org-varde--edit (format "*project cairn %s*" (org-varde--project-name dir))
                     (list "--project" dir))))

(defun org-varde--edit-fill (which)
  "Fill this buffer with the cairn WHICH names to `varde edit', as text.
Its \"#:\" lines, varde's notes, cannot be changed, and point is at the
end of its Status, or else on its first line of the cairn's own."
  (let ((inhibit-read-only t))
    (erase-buffer)
    ;; varde's own text: what it compares the edit against.
    (insert (apply #'org-varde--varde "edit" (append which '("--print"))))
    (goto-char (point-min))
    (while (re-search-forward "^#:.*\n?" nil t)
      ;; Sticky inward only: a line can be written right below it.
      (add-text-properties (match-beginning 0) (match-end 0) '(read-only t))
      (put-text-property (match-beginning 0) (1+ (match-beginning 0)) 'front-sticky '(read-only))
      (put-text-property (1- (match-end 0)) (match-end 0) 'rear-nonsticky '(read-only)))
    (set-buffer-modified-p nil)
    (goto-char (point-min))
    (unless (re-search-forward "^Status:.*$" nil t)
      (while (and (looking-at-p "#:") (zerop (forward-line 1))))
      (end-of-line))))

(defun org-varde--edit (name which)
  "Edit in buffer NAME the cairn that WHICH names to `varde edit'.
Return the buffer."
  (let ((buffer (get-buffer-create name)))
    ;; An edit in progress is kept.
    (unless (buffer-modified-p buffer)
      (with-current-buffer buffer
        (org-varde-cairn-mode)
        (org-varde--edit-fill which)
        (setq-local org-varde--editing which)
        (setq header-line-format (concat " " (org-varde--edit-keys "save" "cancel")))))
    (pop-to-buffer buffer)))

(defun org-varde-cairn-save ()
  "Save the cairn being edited.
When it was updated while you were editing, nothing is saved: the
buffer then shows it as it is now, and what you wrote is in the kill
ring, for \\[yank]."
  (interactive)
  (let ((said (condition-case err
                  (org-varde--call (buffer-string) (append '("edit") org-varde--editing '("--stdin")))
                (org-varde-varde-error
                 (unless (string-match-p "updated while you were editing" (cadr err))
                   (user-error "varde: %s" (cadr err)))
                 (kill-new (buffer-string))
                 (org-varde--edit-fill org-varde--editing)
                 (user-error "It was updated while you were editing: this is it now, and what you wrote is in the kill ring (C-y)")))))
    (message "%s" (string-trim said)))
  (set-buffer-modified-p nil)
  (org-varde--close-edit)
  ;; A cairn on view shows what was just saved.
  (when (get-buffer org-varde-show-buffer)
    (with-current-buffer org-varde-show-buffer (revert-buffer))))

(defun org-varde--project-here ()
  "The project of the task here, or of the board, or one asked for."
  (expand-file-name
   (or (plist-get (get-text-property (point) 'org-varde-row) :project)
       (and (derived-mode-p 'org-varde-board-mode) org-varde-board--project)
       (when-let* ((task (ignore-errors (org-varde--task-here))))
         (plist-get (org-varde--varde-json "show" task "--no-git" "--json") :project))
       (read-directory-name "Project: "))))

;;;###autoload
(defun org-varde-show-project (&optional directory)
  "Show a project cairn: the view's Project tab.
That of the task here, in its view; else that of the project DIRECTORY
is in, alone, asked for when there is no task here."
  (interactive)
  (let ((task (and (not directory) (ignore-errors (org-varde--task-here)))))
    (if task
        (org-varde--show-fill task 'project)
      (org-varde--project-fill (or directory (org-varde--project-here)))))
  (org-varde--pop-view))

(defun org-varde--project-fill (dir)
  "Fill the view with the cairn of the project DIR is in, alone."
  (let ((data (org-varde--varde-json "show" "--project" dir "--json")))
    (org-varde--view nil data 'project (or (plist-get data :project) dir))))

(defvar org-varde--recall-history nil
  "What was recalled before.")

(defun org-varde--matched (kind)
  "What a recall hit of KIND matched, as `varde recall' says it."
  (pcase kind
    ("findings" "finding") ("decisions" "decision") ("history" "earlier") ("notes" "note")
    ("id" "status") ("conventions" "convention") ("gotchas" "gotcha") ("howto" "how-to")
    (_ kind)))

;;;###autoload
(defvar-local org-varde--recall-args nil
  "What the recall buffer shows: (WORDS ALL PROJECT), as
`org-varde-recall' took them, for \\[org-varde-recall-toggle-scope].")

(defun org-varde-recall (words &optional all project)
  "Search what earlier tasks learned for WORDS: in the project here first,
as the board's recall does, else in every project.  With a prefix ALL,
in every project.  PROJECT, if given, is the project here."
  (interactive (list (read-string (if current-prefix-arg "Recall in every project: " "Recall: ")
                                  nil 'org-varde--recall-history)
                     current-prefix-arg))
  (let* ((project (or project
                      (and (derived-mode-p 'org-varde-board-mode) org-varde-board--project)
                      (and (derived-mode-p 'org-varde-show-mode) org-varde--project-dir)
                      (and (not (file-remote-p default-directory)) (expand-file-name default-directory))))
         (where (cond (all '("--all")) (project (list "--project" project))))
         (found (apply #'org-varde--varde-json "recall" "--json" (append where (split-string words))))
         (hits (plist-get found :hits))
         (wide (or all (plist-get found :everywhere) (null hits))))
    (with-current-buffer (get-buffer-create "*varde recall*")
      (let ((inhibit-read-only t))
        (erase-buffer)
        ;; As the terminal's: what was asked, and where it looked.
        (insert (propertize (concat "Recall: " words) 'face 'org-varde-heading)
                (org-varde--quiet (if wide "  in every project" "  in this project"))
                (org-varde--quiet (substitute-command-keys
                                   (cond (all "  (\\<org-varde-recall-mode-map>\\[org-varde-recall-toggle-scope] this project first)")
                                         (wide "")
                                         (t "  (\\<org-varde-recall-mode-map>\\[org-varde-recall-toggle-scope] every project)"))))
                "\n\n")
        (cond ((null hits)
               (insert (org-varde--quiet (if all "Nothing found.\n"
                                           "Nothing found, here or in any other project.\n"))))
              ((plist-get found :everywhere)
               (insert (org-varde--quiet "Nothing in this project; the best from every project:\n\n"))))
        (dolist (h hits)
          (let ((start (point))
                (task (plist-get h :task))
                (state (plist-get h :state))
                (dir (plist-get h :project))
                (more (or (plist-get h :more) 0)))
            (insert (if (string-empty-p task)
                        ;; A project cairn, by its project, as the terminal names it.
                        (propertize (concat (org-varde--project-name dir) " · project cairn") 'face 'org-varde-heading)
                      (concat (plist-get h :short) "  " (propertize (plist-get h :title) 'face 'org-varde-heading)
                              (org-varde--quiet
                               (concat (if (member state '("done" "parked" "dropped")) (format " (%s)" state) "")
                                       (if (and dir wide) (concat "  " (org-varde--project-name dir)) "")))))
                    "\n"
                    ;; Wrapped, a snippet hangs under itself.
                    (propertize (concat "    " (org-varde--quiet (concat (org-varde--matched (plist-get h :kind)) ": "))
                                        (plist-get h :snippet)
                                        ;; Its code is gone.
                                        (if (plist-get h :stale) (propertize " ?" 'face 'warning) "")
                                        (if (> more 0)
                                            (org-varde--quiet (format "  (+%d more in this %s)" more
                                                                      (if (string-empty-p task) "project cairn" "task")))
                                          ""))
                                'wrap-prefix "      ")
                    "\n")
            ;; A task's hit by its full id; a project cairn's by its project.
            (add-text-properties start (point) (if (string-empty-p task)
                                                   (list 'org-varde-recall-project dir)
                                                 (list 'org-varde-task task
                                                       'org-varde-recall-hit (list task (plist-get h :title) state))))
            ;; Where n and p stop.
            (put-text-property start (1+ start) 'org-varde-recall-at t)))
        (goto-char (or (text-property-any (point-min) (point-max) 'org-varde-recall-at t) (point-min))))
      (org-varde-recall-mode)
      (setq org-varde--recall-args (list words all project))
      (pop-to-buffer (current-buffer)))))

(defun org-varde-recall-toggle-scope ()
  "Recall the same words again, in every project, or back in this
project first, as the terminal's . does."
  (interactive)
  (pcase-let ((`(,words ,all ,project) (or org-varde--recall-args (user-error "Nothing recalled here"))))
    (org-varde-recall words (not all) project)))

(defun org-varde-recall-show ()
  "Show the task of the hit at point, or the project cairn it is from.
In this window, where \\<org-varde-show-mode-map>\\[quit-window] brings the hits back."
  (interactive)
  (let ((project (get-text-property (point) 'org-varde-recall-project)))
    (if project
        (org-varde--project-fill project)
      (org-varde--show-fill (org-varde--task-here) 'task))
    (pop-to-buffer org-varde-show-buffer '((display-buffer-reuse-window display-buffer-same-window)))))

(defun org-varde-recall-start ()
  "Start or resume the task of the hit at point, staying here.
A closed one is reopened once you confirm."
  (interactive)
  (pcase-let ((`(,task ,title ,state) (or (get-text-property (point) 'org-varde-recall-hit)
                                          (user-error "A project cairn has no session; v shows it"))))
    (org-varde--confirm-reopen task title state)
    (let ((org-varde--background t))
      (org-varde-start-task task title))))

(defun org-varde-recall-next (&optional n)
  "Move to the Nth next hit; negative N, back."
  (interactive "p")
  (let ((step (if (< (or n 1) 0) -1 1)) (left (abs (or n 1))) found)
    (save-excursion
      (beginning-of-line)
      (while (and (> left 0) (zerop (forward-line step)) (not (eobp)))
        (when (get-text-property (point) 'org-varde-recall-at)
          (setq found (point) left (1- left)))))
    (if found (goto-char found) (user-error "No more hits"))))

(defun org-varde-recall-previous (&optional n)
  "Move to the Nth previous hit."
  (interactive "p")
  (org-varde-recall-next (- (or n 1))))

(defvar-keymap org-varde-recall-mode-map
  :parent special-mode-map
  "RET" #'org-varde-recall-show
  "v" #'org-varde-recall-show
  "K" #'org-varde-recall-show
  "s" #'org-varde-recall-start
  "n" #'org-varde-recall-next
  "p" #'org-varde-recall-previous
  "j" #'org-varde-recall-next
  "k" #'org-varde-recall-previous
  "C-n" #'org-varde-recall-next
  "C-p" #'org-varde-recall-previous
  "." #'org-varde-recall-toggle-scope)

(define-derived-mode org-varde-recall-mode special-mode "varde recall"
  "What `varde recall' found.  On a hit, RET shows its task or project
cairn, and s starts its task.  A yellow ? marks a line about code that
is gone.
\\{org-varde-recall-mode-map}"
  (setq header-line-format " RET view  s start  j k hits  . scope  q close"
        word-wrap t)
  (org-varde--evil-keys org-varde-recall-mode-map))

;;;###autoload
(defun org-varde-doctor ()
  "Check varde and this client."
  (interactive)
  (let ((varde (with-temp-buffer
                 ;; Its report matters most when it fails.
                 (condition-case err
                     (let ((default-directory org-varde--safe-directory))
                       (call-process org-varde-varde-program nil t nil "doctor"))
                   (error (insert (format "!!  varde    %s\n" (error-message-string err)))))
                 (buffer-string))))
    (with-current-buffer (get-buffer-create "*varde doctor*")
      (let ((inhibit-read-only t))
        (erase-buffer)
        (insert varde)
        (insert (format "%s emacs    org-varde-mode %s, watching %s\n"
                        (if (and org-varde-mode org-varde--watch) "ok " "!! ")
                        (if org-varde-mode "on" "off")
                        (org-varde--home))
                (cond ((not (eq org-varde-terminal-function #'org-varde--open-shell))
                       "ok  terminal your own `org-varde-terminal-function'\n")
                      ((locate-library "vterm") "ok  terminal vterm\n")
                      (t "ok  terminal term; install vterm for a faster one\n"))))
      (special-mode)
      (pop-to-buffer (current-buffer)))))

;;; --- Watching varde ----------------------------------------------------------

(defvar org-varde-board-data nil
  "The last board from `varde list --json'.")

(defvar org-varde-after-refresh-hook nil
  "Run after `org-varde-board-data' changes.")

(add-hook 'org-varde-after-refresh-hook #'org-varde--put-away-done)
;; A turn that ends while you watch it is seen as it ends.
(add-hook 'org-varde-after-refresh-hook #'org-varde--mark-seen)

(defvar org-varde--timer nil)
(defvar org-varde--listing nil "The running `varde list', if any.")
(defvar org-varde--again nil "Non-nil when a refresh came in during a listing.")
(defvar org-varde--listed nil "The last `varde list --json', as (TEXT . DATA).")

(defun org-varde--home ()
  "The state directory of varde, where its store changes can be watched.
As varde finds it: a relative XDG_STATE_HOME is ignored."
  (let ((home (getenv "VARDE_HOME"))
        (state (getenv "XDG_STATE_HOME")))
    (file-name-as-directory
     (if (and home (not (string-empty-p home)))
         home
       (expand-file-name "varde" (if (and state (file-name-absolute-p state)) state "~/.local/state"))))))

(defun org-varde--on-store-change (event)
  "Refresh on EVENT when varde wrote to its store.
Only writes count: every `varde list' opens the database too, which
creates and removes its -wal and -shm files, and refreshing on those
would refresh forever."
  (when (and (eq (nth 1 event) 'changed)
             (string-match-p "/varde\\.db\\(?:-wal\\)?\\'" (nth 2 event)))
    (org-varde-refresh)))

(defvar org-varde--due nil
  "When the refresh asked for must run by, however often it is asked again.")

(defun org-varde-refresh ()
  "Fetch the board from varde soon, without blocking.
Soon is 0.2 s after the last time this is called, and at most 1 s
after the first: else a store written steadily would never refresh."
  (interactive)
  (when (timerp org-varde--timer) (cancel-timer org-varde--timer))
  (unless org-varde--due (setq org-varde--due (+ (float-time) 1.0)))
  (setq org-varde--timer
        (run-at-time (max 0 (min 0.2 (- org-varde--due (float-time)))) nil #'org-varde--list)))

(defvar org-varde--too-new nil
  "The version of varde's last board, when it is newer than org-varde reads.")

(defun org-varde--readable (data)
  "Whether DATA, a board from `varde list --json', is one org-varde reads.
A newer one is said once, and noted for the board to show."
  (let ((v (or (plist-get data :v) 0)))
    (if (<= v 2)
        (progn (setq org-varde--too-new nil) t)
      (unless org-varde--too-new
        (message "org-varde: this varde's board (v%s) is newer than org-varde reads; update org-varde" v))
      (setq org-varde--too-new v)
      nil)))

(defun org-varde--list ()
  "Run `varde list --json' asynchronously and store the result."
  (setq org-varde--due nil)
  (if (process-live-p org-varde--listing)
      (setq org-varde--again t)
    (setq org-varde--listing
          (org-varde--varde-async
           '("list" "--json")
           (lambda (out)
             (let ((told org-varde--too-new)
                   ;; The same board as last time is the same object, so
                   ;; nothing redraws it.
                   (data (if (equal out (car org-varde--listed)) (cdr org-varde--listed) (org-varde--json out))))
               (setq org-varde--listed (cons out data))
               (when (org-varde--readable data)
                 (setq org-varde-board-data data))
               ;; A board too new is drawn once, to say so.
               (unless (and told org-varde--too-new)
                 (dolist (f org-varde-after-refresh-hook)
                   (with-demoted-errors "org-varde: %S" (funcall f)))))
             (org-varde--list-again))
           #'org-varde--list-again))))

(defun org-varde--list-again ()
  "Refresh once more if a refresh came in during the last listing."
  (when org-varde--again
    (setq org-varde--again nil)
    (org-varde-refresh)))

;;;###autoload
(define-minor-mode org-varde-mode
  "Keep the board and the varde: links current, and have a heading
marked done offer to close the tasks it links."
  :global t
  :group 'org-varde
  (when org-varde--watch
    (file-notify-rm-watch org-varde--watch)
    (setq org-varde--watch nil))
  (remove-hook 'org-after-todo-state-change-hook #'org-varde--on-done)
  (remove-hook 'window-selection-change-functions #'org-varde--mark-seen)
  (remove-hook 'window-buffer-change-functions #'org-varde--mark-seen)
  (remove-function after-focus-change-function #'org-varde--mark-seen)
  (when org-varde-mode
    (add-hook 'org-after-todo-state-change-hook #'org-varde--on-done)
    (add-hook 'window-selection-change-functions #'org-varde--mark-seen)
    (add-hook 'window-buffer-change-functions #'org-varde--mark-seen)
    (add-function :after after-focus-change-function #'org-varde--mark-seen)
    (let ((home (org-varde--home)))
      (make-directory home t)
      ;; The directory, not the database: SQLite's -wal file comes and goes.
      (setq org-varde--watch
            (condition-case err
                (file-notify-add-watch home '(change) #'org-varde--on-store-change)
              (error (message "org-varde: cannot watch %s (%s); the board refreshes when opened"
                              home (error-message-string err))
                     nil))))
    (org-varde-refresh)))

;; Heading per task, which org-varde 1.1 dropped: tasks live on the board,
;; and org links to them.
(declare-function org-varde-new-here "org-varde-board" ())

(defun org-varde-start ()
  "Go to the open task the heading at point links, else make one from it."
  (declare (obsolete org-varde-new-here "1.1"))
  (interactive)
  (if-let* ((row (and (org-varde--heading-p) (car (org-varde--entry-rows)))))
      (org-varde-go-task (plist-get row :task) (plist-get row :title))
    (org-varde-new-here)))

(define-obsolete-function-alias 'org-varde-link #'org-varde-insert-link "1.1")

(defun org-varde-agenda-status ()
  "Nothing: tasks are no longer headings.  See the varde: links' statuses."
  (declare (obsolete "a varde: link in the heading shows its task's status." "1.1"))
  "")

(provide 'org-varde)
;;; org-varde.el ends here
