;;; org-varde-board.el --- The varde board -*- lexical-binding: t; -*-

;; Copyright (C) 2026 Morten Smebye

;; Author: Morten Smebye <mortensmeb@gmail.com>
;; URL: https://github.com/Smebbs/varde
;; SPDX-License-Identifier: MIT

;; This file is not part of GNU Emacs.  It is free software under the
;; MIT License; see the LICENSE file that comes with it.

;;; Commentary:

;; Renders the rows `varde list --json' returns, already grouped, ordered
;; and labelled: this decides nothing about what is shown.  Rendering
;; spawns nothing; the data arrives asynchronously from `org-varde-mode'.

;;; Code:

(require 'org-varde)
(require 'text-property-search)
(require 'transient)

(declare-function +workspace-list-names "ext:workspaces" ())

(defface org-varde-board-group '((t :inherit bold))
  "Group headings on the board."
  :group 'org-varde)
(defface org-varde-board-empty '((t :inherit shadow))
  "Empty groups and quiet columns."
  :group 'org-varde)
(defface org-varde-board-git '((t :inherit shadow))
  "Where a task's git work stands."
  :group 'org-varde)

(defcustom org-varde-board-scope 'project
  "Whether the board opens kept to the project you open it from.
`project' keeps it to the repository of the buffer `org-varde-board' is
called from, once that project has a task on the board; `all' shows
every project.  \\<org-varde-board-mode-map>\\[org-varde-board-toggle-project] on the board switches."
  :type '(choice (const :tag "The project it is opened from" project)
                 (const :tag "Every project" all))
  :group 'org-varde)

(defvar-local org-varde-board--project nil
  "The project the board was opened from; nil outside a repository.")

(defvar-local org-varde-board--scoped nil
  "Whether the board keeps to `org-varde-board--project'.")

(defun org-varde-board--project-of (dir)
  "The project DIR is in, as varde records it; nil outside a repository."
  (unless (file-remote-p dir)
    (when-let* ((project (ignore-errors
                           (plist-get (org-varde--varde-json "show" "--project" (expand-file-name dir) "--json")
                                      :project))))
      (and (file-exists-p (expand-file-name ".git" project)) project))))

(defun org-varde-board--has (project)
  "Whether PROJECT has a task on the board."
  (seq-some (lambda (group)
              (seq-some (lambda (row) (equal (plist-get row :project) project))
                        (plist-get group :rows)))
            (plist-get org-varde-board-data :groups)))

(defun org-varde-board--scope-line (kept hidden need-you room)
  "What the board keeps to, for the header line, in ROOM columns.
KEPT is whether it keeps to its project, leaving out HIDDEN tasks, of
which NEED-YOU need you.  Said shorter where it must be; else nothing."
  (let* ((project org-varde-board--project)
         (name (and project (propertize (file-name-nondirectory (directory-file-name project))
                                        'face 'bold)))
         (key (propertize "." 'face 'help-key-binding))
         (needs (and (> need-you 0)
                     (propertize (format "%d need%s you" need-you (if (= need-you 1) "s" ""))
                                 'face 'org-varde-board-waiting))))
    (or (seq-find (lambda (s) (<= (string-width s) room))
                  (cond
                   ((not project) nil)
                   ((and kept (> hidden 0))
                    (list (concat name " · " (format "%d in other projects" hidden)
                                  (if needs (concat ", " needs) "") " (" key " all)")
                          (concat name " · " (format "%d elsewhere" hidden) (if needs (concat ", " needs) ""))
                          (if needs (concat name " · " needs " elsewhere") (concat name " · +" (number-to-string hidden)))
                          name))
                   (kept (list name))
                   ((not org-varde-board--scoped)
                    (list (concat "every project (" key " only " name ")") "every project"))
                   (t (list (concat "every project (none in " name " yet)") "every project"))))
        "")))

(defun org-varde-board--meta (row now kept group)
  "What follows ROW's title at time NOW in ms, as the terminal board shows it:
a live session's status, what it left running, where deferred work came
from, git, the project unless KEPT to one, how long ago, and its short
id.  In Needs you, GROUP says it is your turn: the row does not again."
  (let* ((git (plist-get row :git))
         (label (plist-get git :label))
         (project (plist-get row :project))
         (count (lambda (k what)
                  (let ((n (or (plist-get row k) 0)))
                    (and (> n 0) (format "%d %s%s" n what (if (= n 1) "" "s"))))))
         (parts
          (delq nil
                (list (pcase (plist-get row :status)
                        ((or "working" "delegated") (propertize "working" 'face 'org-varde-board-working))
                        ("waiting" (unless (equal group "needs_you")
                                     (propertize "your turn" 'face 'org-varde-board-waiting)))
                        ("blocked" (propertize "needs input" 'face 'org-varde-board-blocked)))
                      (funcall count :agents "agent")
                      (funcall count :shells "shell")
                      (let ((origin (plist-get row :origin)))
                        (and origin (concat "← " (plist-get origin :short))))
                      (and label (not (string-empty-p label))
                           (propertize label 'face
                                       (cond ((equal (plist-get git :checks) "failing") 'org-varde-board-blocked)
                                             ((equal (plist-get git :stage) "edited") 'org-varde-board-waiting)
                                             ((equal (plist-get git :stage) "landed") 'org-varde-board-working)
                                             (t 'org-varde-board-git))))
                      (and project (not kept) (file-name-nondirectory (directory-file-name project)))
                      ;; Waiting for its schedule, it shows when it starts.
                      (let ((schedule (plist-get row :schedule)))
                        (if (and schedule (not (member (plist-get row :status)
                                                       '("working" "delegated" "waiting" "blocked"))))
                            (propertize (plist-get schedule :label)
                                        'face (if (plist-get schedule :late) 'org-varde-board-blocked
                                                'org-varde-board-empty))
                          (org-varde--age (- now (plist-get row :last)))))))))
    (concat (mapconcat (lambda (p) (if (get-text-property 0 'face p) p (propertize p 'face 'org-varde-board-empty)))
                       parts (propertize " · " 'face 'org-varde-board-empty))
            (propertize (concat "  " (plist-get row :short)) 'face 'org-varde-board-empty))))

(defvar org-varde-board--width 100
  "The width of the board's window, while it is drawn.")

(defun org-varde-board--row (row now group &optional kept)
  "Insert one board ROW of GROUP, at time NOW in ms: a glyph for where it
stands, its title, and on the right what else there is to know.  KEPT
says the board keeps to one project, which its header line names, so
the row leaves it out."
  (let* ((glyph (org-varde--status-mark row))
         (closed (member (plist-get row :state) '("done" "parked" "dropped")))
         (meta (org-varde-board--meta row now kept group))
         (room (- org-varde-board--width 6 (string-width meta)))
         (start (point)))
    (insert " " glyph " "
            (propertize (truncate-string-to-width (plist-get row :title) (max 12 room) nil nil "…")
                        'face (if closed 'org-varde-board-empty 'default))
            ;; What follows the title keeps to the right edge, however wide
            ;; the window.
            (propertize " " 'display `(space :align-to (- right ,(1+ (string-width meta)))))
            meta
            "\n")
    (add-text-properties start (point) (list 'org-varde-task (plist-get row :task)
                                             'org-varde-row row
                                             'org-varde-in group))))

(defvar-local org-varde-board--keep nil
  "Where the board keeps its place: (TASK GROUP TIME) after an action on
TASK, there in GROUP.  When TASK leaves GROUP, point stays on its line,
as the terminal board's does, and is on the task that took its place.")

(defvar-local org-varde-board--select nil
  "A task to put point on once the board shows it, such as one just made.")

(defun org-varde-board--keep-place ()
  "Keep the board's place after an action on the task at point."
  (when (derived-mode-p 'org-varde-board-mode)
    (setq org-varde-board--keep (list (get-text-property (point) 'org-varde-task)
                                      (get-text-property (point) 'org-varde-in)
                                      (float-time)))))

(defun org-varde-board--snap ()
  "Move to the nearest line a move stops at: this one, or else below,
or else above."
  (beginning-of-line)
  (unless (org-varde-board--stop-p 'org-varde-task)
    (let ((here (point)))
      (or (ignore-error user-error (org-varde-board--next 'org-varde-task 1) t)
          (progn (goto-char here)
                 (ignore-error user-error (org-varde-board--next 'org-varde-task -1)))))))

(defun org-varde-board--goto-task (task line)
  "Move to TASK's row, or to LINE when it is gone or left the group the
board keeps its place in."
  (goto-char (point-min))
  (let* ((select (and org-varde-board--select
                      (text-property-search-forward 'org-varde-task org-varde-board--select t)))
         (match (or select
                    (and task (progn (goto-char (point-min))
                                     (text-property-search-forward 'org-varde-task task t)))))
         (keep org-varde-board--keep))
    (if (and match
             (or select
                 (not (equal (car keep) task))
                 (equal (get-text-property (prop-match-beginning match) 'org-varde-in) (nth 1 keep))))
        (goto-char (prop-match-beginning match))
      (goto-char (point-min))
      (forward-line (1- line))
      (org-varde-board--snap))))

(defvar-local org-varde-board--placed nil
  "Whether point was put on the board's first task, as it opened.")

(defvar-local org-varde-board--toggled nil
  "Keys of the groups opened or closed here, against how varde shows them.")

(defvar-local org-varde-board--drawn nil
  "What the board was last drawn from: the data, and the window's width.")

(defun org-varde-board--folded-p (group)
  "Whether GROUP shows only its heading: as varde has it, unless toggled."
  (xor (plist-get group :collapsed)
       (member (plist-get group :key) org-varde-board--toggled)))

(defun org-varde-board--render ()
  "Redraw the board from `org-varde-board-data'.
Every window showing it stays on the task it was on."
  (when-let* ((buffer (get-buffer org-varde-board-buffer)))
    (with-current-buffer buffer
      (let* ((spot (lambda (pos)
                     (save-excursion
                       (goto-char pos)
                       (cons (get-text-property (point) 'org-varde-task) (line-number-at-pos)))))
             (windows (mapcar (lambda (w) (cons w (funcall spot (window-point w))))
                              (get-buffer-window-list buffer nil t)))
             (here (funcall spot (point)))
             (now (truncate (* 1000 (float-time))))
             (org-varde-board--width (if windows (window-width (caar windows)) 100))
             (project (and org-varde-board--scoped org-varde-board--project))
             ;; A project with no task on the board yet keeps to nothing.
             (kept (and project (org-varde-board--has project)))
             (hidden 0)
             (need-you 0)
             (any nil)
             (inhibit-read-only t))
        (setq org-varde-board--drawn (cons org-varde-board-data org-varde-board--width)
              org-varde--kept (and kept project))
        (erase-buffer)
        (cond
         (org-varde--too-new
          (insert (propertize (format "This varde's board is v%s, newer than org-varde reads: update org-varde.\n"
                                      org-varde--too-new)
                              'face 'org-varde-board-waiting)))
         ((not org-varde-board-data)
          (insert (propertize "Waiting for varde…\n" 'face 'org-varde-board-empty)))
         (t
          (dolist (group (plist-get org-varde-board-data :groups))
            (let* ((all (plist-get group :rows))
                   (rows (if kept
                             (seq-filter (lambda (row) (equal (plist-get row :project) project)) all)
                           all))
                   (key (plist-get group :key)))
              ;; An empty group has nothing to fold: once tasks come back,
              ;; they show as varde has them.
              (unless rows
                (setq org-varde-board--toggled (delete key org-varde-board--toggled)))
              (let ((closed (org-varde-board--folded-p group)))
                (if rows
                    (progn
                      (setq any t)
                      (insert (propertize (plist-get group :label) 'face 'org-varde-board-group)
                              (propertize (format "  %d%s" (length rows) (if closed " ▸" ""))
                                          'face 'org-varde-board-empty)
                              "\n")
                      (put-text-property (line-beginning-position 0) (point) 'org-varde-group key)
                      ;; Folded, its heading stands in for its rows.
                      (when closed
                        (put-text-property (line-beginning-position 0) (point) 'org-varde-folded t)))
                  ;; An empty group is one quiet line, no stop: the board
                  ;; never jumps.
                  (insert (propertize (format "%s  0\n" (plist-get group :label)) 'face 'org-varde-board-empty)))
                (setq hidden (+ hidden (- (length all) (length rows))))
                (when (equal key "needs_you")
                  (setq need-you (- (length all) (length rows))))
                (unless closed
                  (dolist (row rows) (org-varde-board--row row now key kept))))))
          (unless any
            (insert (propertize "\nNo tasks here yet.  n writes one, or start an agent in a repository:\nits session shows up here once you ask it something.\n"
                                'face 'org-varde-board-empty)))))
        ;; The keys first, so `? help' is never cut; what it keeps to after.
        (setq header-line-format
              (let* ((keys (concat " " (org-varde-board--header)))
                     (scope (org-varde-board--scope-line
                             kept hidden need-you (- org-varde-board--width (string-width keys) 4))))
                (concat keys (if (string-empty-p scope) "" (concat "   " scope)))))
        (org-varde-board--goto-task (car here) (cdr here))
        (dolist (w windows)
          (save-excursion
            (org-varde-board--goto-task (cadr w) (cddr w))
            (set-window-point (car w) (point))))
        ;; A task to select is selected once it shows.
        (when (and org-varde-board--select
                   (equal (get-text-property (point) 'org-varde-task) org-varde-board--select))
          (setq org-varde-board--select nil))
        ;; A place kept for a task that has moved on, or long ago, is done.
        (let ((keep org-varde-board--keep))
          (when (and keep (or (< (+ (nth 2 keep) 10) (float-time))
                              (not (equal (nth 1 keep)
                                          (save-excursion
                                            (goto-char (point-min))
                                            (let ((m (and (car keep) (text-property-search-forward
                                                                      'org-varde-task (car keep) t))))
                                              (and m (get-text-property (prop-match-beginning m) 'org-varde-in))))))))
            (setq org-varde-board--keep nil)))
        ;; It opens on the first task, from where TAB goes round what needs you.
        (when (and org-varde-board-data (not org-varde-board--placed))
          (setq org-varde-board--placed t)
          (ignore-error user-error (org-varde-board-first-task))
          (dolist (w windows) (set-window-point (car w) (point))))
        (org-varde--highlight)))))

(defun org-varde-board--refreshed ()
  "Redraw the board after a refresh, when a window shows it and it changed.
Hidden, it is drawn as it comes into view."
  (when-let* ((buffer (get-buffer org-varde-board-buffer))
              (window (get-buffer-window buffer t)))
    (unless (equal (buffer-local-value 'org-varde-board--drawn buffer)
                   (cons org-varde-board-data (window-width window)))
      (org-varde-board--render))))

(defvar org-varde-board--ticking nil
  "The timer that keeps the board's ages current while it is in sight.")

(defun org-varde-board--tick ()
  "Draw the board again, its ages a minute on, when it is in sight.
Not while a region is active there: drawing would lose it."
  (let ((buffer (get-buffer org-varde-board-buffer)))
    (cond ((not buffer)
           (cancel-timer org-varde-board--ticking)
           (setq org-varde-board--ticking nil))
          ((and (get-buffer-window buffer t) (not (buffer-local-value 'mark-active buffer)))
           (org-varde-board--render)))))

(defun org-varde-board-toggle-project ()
  "Keep the board to the project it was opened from, or show every one.
From a view too: what TAB and J go through follows."
  (interactive)
  (with-current-buffer (or (get-buffer org-varde-board-buffer) (user-error "The board is not open"))
    (let* ((project (or org-varde-board--project
                        (user-error "The board was opened outside a project, so it shows every one")))
           (name (file-name-nondirectory (directory-file-name project))))
      (unless (org-varde-board--has project)
        (user-error "No tasks in %s yet, so the board shows every project" name))
      (setq org-varde-board--scoped (not org-varde-board--scoped))
      (org-varde-board--render)
      (message (if org-varde-board--scoped "Only %s" "Every project") name))))

(defun org-varde-board--row-at-point ()
  "The board row at point; in a task's view, the task it shows."
  (or (get-text-property (point) 'org-varde-row)
      (org-varde--view-row)
      (user-error "No task on this line")))

(defun org-varde-board--rows ()
  "The rows in the board's active region, or else the row at point."
  (if (and (use-region-p) (derived-mode-p 'org-varde-board-mode))
      (let ((end (region-end)) rows)
        (save-excursion
          (goto-char (region-beginning))
          (beginning-of-line)
          (while (< (point) end)
            (let ((row (get-text-property (point) 'org-varde-row)))
              (when row (push row rows)))
            (forward-line 1)))
        (deactivate-mark)
        (or (nreverse rows) (user-error "No tasks in the region")))
    (list (org-varde-board--row-at-point))))

(defun org-varde-board--stop-p (prop)
  "Whether this line is one PROP moves to: a task, or a folded group
standing in for its tasks, for `org-varde-task'; else a group heading."
  (or (get-text-property (point) prop)
      (and (eq prop 'org-varde-task) (get-text-property (point) 'org-varde-folded))))

(defun org-varde-board--next (prop n)
  "Move to the Nth line after this one that `org-varde-board--stop-p' PROP.
Negative N moves back."
  (let ((step (if (< n 0) -1 1))
        (left (abs n))
        found)
    (save-excursion
      (beginning-of-line)
      (while (and (> left 0) (zerop (forward-line step)) (not (eobp)))
        (when (org-varde-board--stop-p prop)
          (setq found (point) left (1- left)))))
    (if found (goto-char found) (user-error "No more %s" (if (eq prop 'org-varde-task) "tasks" "groups")))))

(defun org-varde-board-next-task (&optional n)
  "Move to the Nth next task."
  (interactive "p")
  (org-varde-board--next 'org-varde-task (or n 1)))

(defun org-varde-board-previous-task (&optional n)
  "Move to the Nth previous task."
  (interactive "p")
  (org-varde-board--next 'org-varde-task (- (or n 1))))

(defun org-varde-board-next-group (&optional n)
  "Move to the Nth next group heading."
  (interactive "p")
  (org-varde-board--next 'org-varde-group (or n 1)))

(defun org-varde-board-previous-group (&optional n)
  "Move to the Nth previous group heading."
  (interactive "p")
  (org-varde-board--next 'org-varde-group (- (or n 1))))

(defun org-varde-board-first-task ()
  "Move to the first task."
  (interactive)
  (goto-char (point-min))
  (unless (org-varde-board--stop-p 'org-varde-task) (org-varde-board-next-task)))

(defun org-varde-board-last-task ()
  "Move to the last task."
  (interactive)
  (goto-char (point-max))
  (org-varde-board-previous-task))

(defun org-varde-board--unfold (key)
  "Open the group KEY, if it is folded, and draw the board."
  (let ((group (seq-find (lambda (g) (equal (plist-get g :key) key))
                         (plist-get org-varde-board-data :groups))))
    (when (and group (org-varde-board--folded-p group))
      (setq org-varde-board--toggled (if (member key org-varde-board--toggled)
                                         (delete key org-varde-board--toggled)
                                       (cons key org-varde-board--toggled)))
      (org-varde-board--render))))

(defun org-varde-board-next-needs-you (&optional n)
  "Move to the Nth next task that needs you, round to the first.
Negative N moves back, round to the last.  Its group opens if it was
folded."
  (interactive "p")
  (let* ((n (or n 1))
         (tasks (mapcar (lambda (r) (plist-get r :task)) (org-varde--needs-you-here)))
         (len (length tasks))
         (i (seq-position tasks (get-text-property (point) 'org-varde-task))))
    (if (zerop len)
        (message "%s" (org-varde--nothing-needs-you))
      (let ((to (mod (cond (i (+ i n)) ((> n 0) (1- n)) (t n)) len)))
        (org-varde-board--unfold "needs_you")
        (org-varde--board-select (nth to tasks))
        (when (> len 1) (message "Needs you: %d of %d" (1+ to) len))))))

(defun org-varde-board-previous-needs-you (&optional n)
  "Move to the Nth previous task that needs you, round to the last."
  (interactive "p")
  (org-varde-board-next-needs-you (- (or n 1))))

(defun org-varde-board-tab ()
  "On a task, move to the next that needs you; on a group's heading, fold
or unfold it.  Elsewhere, nothing."
  (interactive)
  (cond ((get-text-property (point) 'org-varde-row) (org-varde-board-next-needs-you))
        ((get-text-property (point) 'org-varde-group) (org-varde-board-toggle-group))))

(defun org-varde-board-start (&optional arg)
  "Go to the session of the task at point, starting or resuming it first
if needed.  Prefix ARG as `org-varde-start-task'.  On a group's heading, fold
or unfold it.
Starting an Inbox task takes it out of the Inbox.  A
done, parked or dropped task is reopened once you confirm.
With a region, start every task in it in the background, going to no
terminal; one that cannot start is skipped, and a quit stops the rest.
Done, parked and dropped tasks are skipped, so none is reopened.
The worktree question is asked once for them all, and each project
question offers the last answer."
  (interactive "P")
  (cond
   ((and (not (use-region-p)) (not (get-text-property (point) 'org-varde-row))
         (get-text-property (point) 'org-varde-group))
    (org-varde-board-toggle-group))
   ((not (and (use-region-p) (derived-mode-p 'org-varde-board-mode)))
    (let ((row (org-varde-board--row-at-point)))
      (org-varde--confirm-reopen (plist-get row :task) (plist-get row :title) (plist-get row :state))
      (if org-varde--background
          (progn (org-varde-board--keep-place)
                 (org-varde-start-task (plist-get row :task) (plist-get row :title) arg))
        (org-varde-go-task (plist-get row :task) (plist-get row :title) arg))))
   (t
    (let* ((all (org-varde-board--rows))
           (rows (seq-remove (lambda (r) (member (plist-get r :state) '("done" "parked" "dropped")))
                             all))
           (failed nil)
           (said (lambda ()
                   (message "Started, or already running: %d of %d%s%s"
                            (- (length rows) (length failed)) (length rows)
                            (if (= (length rows) (length all)) ""
                              (format "; %d closed or parked skipped" (- (length all) (length rows))))
                            (if failed (concat "; " (string-join (reverse failed) "; ")) ""))))
           (next nil))
      ;; One at a time, each once the last is started: a question is
      ;; asked once for them all, and two never ask at once.
      (setq next (lambda (left)
                   (if (null left)
                       (funcall said)
                     (let* ((row (car left))
                            (fail (lambda (why)
                                    (push (format "%s: %s" (plist-get row :short) why) failed))))
                       (condition-case err
                           (org-varde-start-task
                            (plist-get row :task) (plist-get row :title) arg
                            (lambda (why)
                              ;; A quit stops the rest.
                              (unless (eq why 'quit)
                                (when why (funcall fail why))
                                (funcall next (cdr left)))))
                         (error (funcall fail (error-message-string err))
                                (funcall next (cdr left))))))))
      (let ((org-varde--background t)
            (org-varde--answers (list :region t)))
        (funcall next rows))))))

(defun org-varde-board-start-background (&optional arg)
  "Start or resume the task at point, or each task in the region, and stay
on the board.  Prefix ARG as `org-varde-start-task'."
  (interactive "P")
  (let ((org-varde--background t))
    (org-varde-board-start arg)))

(defun org-varde-board-start-fresh ()
  "Start a fresh conversation of the task at point, as \\[universal-argument] RET does."
  (interactive)
  (org-varde-board-start '(4)))

(defun org-varde-board-start-fork ()
  "Fork the conversation of the task at point, as \\[universal-argument] \\[universal-argument] RET does."
  (interactive)
  (org-varde-board-start '(16)))

(defvar org-varde-board--closed nil
  "The rows of the tasks d, P or x last closed, on the board or in a view,
for \\<org-varde-board-mode-map>\\[org-varde-board-reopen] to reopen.")

(defun org-varde-board--note-closed (closed)
  "Keep CLOSED, the rows closed last first, for u to reopen.
In a view, u then reopens it, rather than bring back a line removed
before."
  (when closed
    (setq org-varde-board--closed (nreverse closed))
    (when (derived-mode-p 'org-varde-show-mode) (setq org-varde--removed nil))))

(defun org-varde-board-done ()
  "Close the task at point, or every task in the region."
  (interactive)
  (let ((rows (org-varde-board--rows))
        (closed nil))
    (org-varde-board--keep-place)
    (unwind-protect
        (let ((org-varde--on-board t))
          (dolist (row rows)
            (org-varde--finish (plist-get row :task) (plist-get row :title))
            (push row closed)))
      (org-varde-board--note-closed closed))))

(defun org-varde-board-park ()
  "Park the task at point, or every task in the region.
That puts it aside, its worktree left as it is.  A done or dropped task
is parked once you confirm, as starting it is."
  (interactive)
  (let ((rows (org-varde-board--rows))
        (closed nil))
    (org-varde-board--keep-place)
    (unwind-protect
        (let ((org-varde--on-board t))
          (dolist (row rows)
            (unless (equal (plist-get row :state) "parked")
              (org-varde--confirm-reopen (plist-get row :task) (plist-get row :title) (plist-get row :state)
                                         "park it instead"))
            (org-varde--park (plist-get row :task) (plist-get row :title))
            (push row closed)))
      (org-varde-board--note-closed closed))))

(defun org-varde-board-reopen ()
  "Reopen the tasks d, P or x last closed, back to Open, starting none.
The board is then on the first of them."
  (interactive)
  (let ((rows (or org-varde-board--closed
                  (user-error "Nothing closed here to reopen"))))
    (dolist (row rows)
      (condition-case e
          (org-varde--varde "reopen" (plist-get row :task))
        (org-varde-varde-error (user-error "varde: %s" (cadr e)))))
    (setq org-varde-board--closed nil)
    (when-let* ((board (get-buffer org-varde-board-buffer)))
      (with-current-buffer board
        (setq org-varde-board--select (plist-get (car rows) :task))))
    (message "Reopened %s" (if (cdr rows)
                               (format "%d tasks" (length rows))
                             (org-varde--named (plist-get (car rows) :task) (plist-get (car rows) :title)))))
  (org-varde-refresh))

(defun org-varde-board-accept ()
  "Move the Inbox task at point, or every Inbox task in the region, into
Open.  Others in the region are left as they are."
  (interactive)
  (let* ((rows (org-varde-board--rows))
         (inbox (seq-filter (lambda (r) (plist-get r :inbox)) rows))
         (left (- (length rows) (length inbox))))
    (unless inbox
      (user-error "%s in the Inbox: a accepts Inbox tasks"
                  (if (cdr rows) "None of them is"
                    (concat (org-varde--named (plist-get (car rows) :task) (plist-get (car rows) :title))
                            " is not"))))
    (org-varde-board--keep-place)
    (condition-case e
        (apply #'org-varde--varde "accept" (mapcar (lambda (r) (plist-get r :task)) inbox))
      (org-varde-varde-error (user-error "varde: %s" (cadr e))))
    (message "Accepted %s%s"
             (if (cdr inbox)
                 (format "%d tasks" (length inbox))
               (org-varde--named (plist-get (car inbox) :task) (plist-get (car inbox) :title)))
             (cond ((= left 1) "; 1 not in the Inbox is left as it is")
                   ((> left 1) (format "; %d not in the Inbox are left as they are" left))
                   (t ""))))
  (org-varde-refresh))

(defun org-varde-board-toggle-group ()
  "Open or close the group of the line at point, such as Parked, which
starts closed.  An empty group has nothing to fold."
  (interactive)
  (let ((key (or (get-text-property (point) 'org-varde-group)
                 (get-text-property (point) 'org-varde-in)
                 (user-error "Not on a group with tasks"))))
    (setq org-varde-board--toggled
          (if (member key org-varde-board--toggled)
              (delete key org-varde-board--toggled)
            (cons key org-varde-board--toggled)))
    (org-varde-board--render)
    (goto-char (or (text-property-any (point-min) (point-max) 'org-varde-group key) (point)))
    (org-varde--highlight)))

(define-obsolete-function-alias 'org-varde-board-throw-away #'org-varde-board-drop "1.0.4")

(defun org-varde-board-drop ()
  "Drop the task at point, or every task in the region, once you confirm.
Asked once for them all.  Work in a worktree is kept under
refs/varde/discarded/."
  (interactive)
  (let* ((rows (org-varde-board--rows))
         (org-varde--on-board t)
         (asked (and (cdr rows)
                     (let ((work (seq-some (lambda (r) (org-varde--has-work-p (plist-get r :task))) rows)))
                       (funcall (if work #'yes-or-no-p #'y-or-n-p)
                                (format "Drop %d tasks%s? " (length rows)
                                        (if work " and the work in their worktrees (kept under refs/varde/discarded/)" ""))))))
         (closed nil))
    (org-varde-board--keep-place)
    (when (or asked (not (cdr rows)))
      (dolist (row rows)
        (when (org-varde-drop-task (plist-get row :task) (plist-get row :title) asked)
          (push row closed))))
    (org-varde-board--note-closed closed)))

(declare-function magit-status-setup-buffer "ext:magit-status" (&optional directory))

(defun org-varde-board-diff ()
  "Show what changed in the task's work, in Emacs's own way: magit's
status where magit is, else `vc-dir'.  In its worktree, while it has
one; else in its project."
  (interactive)
  (let* ((row (org-varde-board--row-at-point))
         (d (org-varde--details (plist-get row :task)))
         (worktree (plist-get d :worktree))
         (dir (or (and (equal (plist-get worktree :state) "active") (plist-get worktree :path))
                  (plist-get d :project)
                  (user-error "This task has no project yet: its first start picks one"))))
    (unless (file-directory-p dir) (user-error "%s is gone" (abbreviate-file-name dir)))
    (if (require 'magit nil t)
        (magit-status-setup-buffer dir)
      (vc-dir dir))))

(defun org-varde-board-new (directory)
  "Start a session of your own in DIRECTORY.
Asked for, offering the project of the task at point."
  (interactive
   (let ((project (plist-get (get-text-property (point) 'org-varde-row) :project)))
     (list (read-directory-name "Project directory: "
                                (and project (file-name-as-directory project))))))
  (org-varde-new-session directory))

(defvar-keymap org-varde-new-mode-map
  "C-c C-c" #'org-varde-new-create
  "<remap> <save-buffer>" #'org-varde-new-create
  "C-c C-k" #'org-varde-new-cancel
  "RET" #'org-varde-new-return
  ;; As Customize's and eww's forms: TAB and S-TAB go from field to
  ;; field, and M-TAB completes in one.
  "TAB" #'org-varde-new-tab
  "<backtab>" #'org-varde-new-previous-field
  "C-M-i" #'org-varde-new-complete
  ;; evil's :w, :wq, :x and ZZ create; :q and ZQ put it away.
  "<remap> <evil-write>" #'org-varde-new-create
  "<remap> <evil-save-and-close>" #'org-varde-new-create
  "<remap> <evil-save-modified-and-close>" #'org-varde-new-create
  "<remap> <evil-quit>" #'org-varde-new-cancel)

(define-derived-mode org-varde-new-mode text-mode "varde new"
  "Write a new task, or change one, in a form, as `message-mode' writes
a mail: its Title, which varde makes from the context when left empty;
its Project, which \\[org-varde-new-complete] picks; When varde starts it,
if at all (`at mon 09:00', `every weekday 09:00', `every 4h', an
org timestamp); and below the line, the context for the agent.
\\[org-varde-new-tab] and \\[org-varde-new-return] go from a field to the next, and \\[org-varde-new-previous-field] back; under
evil, C-j and C-k too.  \\[org-varde-new-create] creates it, and
\\[org-varde-new-cancel] puts it away, for `n' on the board to bring
back; cleared first, it is dropped."
  (add-hook 'after-change-functions #'org-varde--form-hints nil t)
  (add-hook 'kill-buffer-query-functions #'org-varde--keep-edit-p nil t)
  ;; Normal state's own TAB would win, and C-j and C-k go between
  ;; fields as between evil-collection's sections.  An empty field's
  ;; place is the end of its line: normal state stays there, not on
  ;; the field's name.
  (setq-local evil-move-beyond-eol t)
  (when (fboundp 'evil-define-key*)
    (evil-define-key* 'normal org-varde-new-mode-map
                      (kbd "TAB") #'org-varde-new-tab
                      (kbd "<backtab>") #'org-varde-new-previous-field
                      (kbd "C-j") #'org-varde-new-next-field
                      (kbd "C-k") #'org-varde-new-previous-field)))

(defface org-varde-form-label '((t :inherit bold))
  "A field's name in the form that writes a task."
  :group 'org-varde)

(defface org-varde-form-hint '((t :inherit org-varde-quiet :slant italic))
  "What an empty field of the form is for, said in its place."
  :group 'org-varde)

(defvar-local org-varde--new-offer nil
  "The project offered when the task being written is created.")

(defvar-local org-varde--new-task nil
  "The task whose title and context this buffer edits, or nil for a new one.")

(defvar-local org-varde--new-when ""
  "The schedule the task this buffer edits had when the form opened.")

(defvar-local org-varde--new-then nil
  "What to do once the task being written is made, or nil: a function of
its short id and title that returns what to add to the message.")

(defvar-local org-varde--form-hints nil
  "The overlays that say what an empty field is for: (FIELD . OVERLAY).")

(defun org-varde--form-label (field text &optional first)
  "TEXT, the name of FIELD, as the form shows it: it cannot be changed,
and what is typed after it is the field's.  FIRST: nothing goes before it."
  ;; As the minibuffer's prompt: a field of its own, so C-a, and evil's
  ;; ^ and cc, stop at the value.
  (propertize text 'field field 'read-only t 'rear-nonsticky t
              'front-sticky (and first '(read-only))
              'font-lock-face (if (eq field 'body) 'org-varde-quiet 'org-varde-form-label)))

(defun org-varde--form-fill (title project body &optional rule)
  "Fill this buffer with the form: its TITLE, its PROJECT unless that is
:none, the RULE of when varde starts it, and its context BODY.  Point is
at the end of the title."
  (let ((inhibit-read-only t)
        (inhibit-modification-hooks t))
    ;; The mode forgot them, so they are found by their mark.
    (remove-overlays (point-min) (point-max) 'org-varde-hint t)
    (setq org-varde--form-hints nil)
    (erase-buffer)
    (insert (org-varde--form-label 'title "Title:   " t) title)
    (unless (eq project :none)
      (insert (org-varde--form-label 'project "\nProject: ")
              (if project (abbreviate-file-name (directory-file-name project)) "")))
    (insert (org-varde--form-label 'when "\nWhen:    ") (or rule ""))
    ;; The line's newline is not its own, so evil's dd can take the
    ;; context's last line; `org-varde--form-hints' puts it back.
    (insert (org-varde--form-label 'body (concat "\n── context " (make-string 30 ?─))) "\n" body))
  (org-varde--form-hints)
  (set-buffer-modified-p nil)
  (org-varde--form-goto 'title))

(defun org-varde--form-bounds (field)
  "Where the value of FIELD is in this form, as (BEG . END), or nil."
  (when-let* ((m (save-excursion
                   (goto-char (point-min))
                   (text-property-search-forward 'field field t))))
    (let ((beg (prop-match-end m)))
      (cons beg (or (text-property-not-all beg (point-max) 'field nil) (point-max))))))

(defun org-varde--form-value (field)
  "The value of FIELD in this form, trimmed, or nil when it has none."
  (when-let* ((b (org-varde--form-bounds field)))
    (string-trim (buffer-substring-no-properties (car b) (cdr b)))))

(defun org-varde--form-fields ()
  "The fields of this form, in order."
  (seq-filter #'org-varde--form-bounds '(title project when body)))

(defun org-varde--form-at ()
  "The field point is in: on a name, the field before it."
  (or (seq-find (lambda (f) (<= (car (org-varde--form-bounds f)) (point)))
                (reverse (org-varde--form-fields)))
      'title))

(defun org-varde--form-goto (field)
  "Go to FIELD: to the start of the context, or the end of another."
  (let ((b (org-varde--form-bounds field)))
    (goto-char (if (eq field 'body) (1+ (car b)) (cdr b)))))

(defun org-varde--form-hints (&rest _)
  "Say in its place what an empty field is for, and keep the context
below its line."
  (when-let* ((b (org-varde--form-bounds 'body)))
    (unless (eq (char-after (car b)) ?\n)
      (save-excursion (goto-char (car b)) (insert "\n"))))
  (pcase-dolist (`(,field . ,hint)
                 `((title . ,(unless org-varde--new-task "left empty, varde names it from the context"))
                   (project . "asked for on create, or M-TAB picks it")
                   (when . "not scheduled: at mon 09:00, every weekday 09:00, every 4h")))
    (let ((b (org-varde--form-bounds field))
          (o (alist-get field org-varde--form-hints)))
      (if (and hint b (string-blank-p (buffer-substring-no-properties (car b) (cdr b))))
          (progn
            (unless o
              (setq o (make-overlay 1 1))
              (overlay-put o 'org-varde-hint t)
              (setf (alist-get field org-varde--form-hints) o))
            (move-overlay o (car b) (car b))
            ;; The cursor before it, not after.
            (overlay-put o 'after-string (propertize hint 'face 'org-varde-form-hint 'cursor t)))
        (when o (delete-overlay o))
        (setf (alist-get field org-varde--form-hints nil t) nil)))))

(defun org-varde--form-blank-p ()
  "Whether nothing is written in this form: no title, and no context."
  (and (string-empty-p (org-varde--form-value 'title))
       (string-empty-p (org-varde--form-value 'body))))

(defun org-varde-new-return ()
  "In a field above the line, go to the next; in the context, a new line."
  (interactive)
  (if (eq (org-varde--form-at) 'body)
      (call-interactively #'newline)
    (org-varde-new-next-field)))

(defun org-varde-new-tab ()
  "In a field above the line, go to the next; in the context, indent."
  (interactive)
  (if (eq (org-varde--form-at) 'body)
      (call-interactively #'indent-for-tab-command)
    (org-varde-new-next-field)))

(defun org-varde-new-next-field (&optional n)
  "Go to the next field, from the context round to the Title; N back
when negative."
  (interactive)
  (let* ((fields (org-varde--form-fields))
         (i (seq-position fields (org-varde--form-at))))
    (org-varde--form-goto (nth (mod (+ i (or n 1)) (length fields)) fields))))

(defun org-varde-new-previous-field ()
  "Go to the field before, from the Title round to the context."
  (interactive)
  (org-varde-new-next-field -1))

(defun org-varde-new-complete ()
  "On the Project, pick its directory; elsewhere, complete at point."
  (interactive)
  (if (eq (org-varde--form-at) 'project)
      (org-varde-new-pick-project)
    (completion-at-point)))

(defun org-varde-new-pick-project ()
  "Pick the directory of the task's project, for its Project field.
It offers the one there, or else the one offered for the task."
  (interactive)
  (let* ((b (or (org-varde--form-bounds 'project) (user-error "This task's project is not changed here")))
         (now (org-varde--form-value 'project))
         (dir (read-directory-name "Project directory for this task: "
                                   (file-name-as-directory (if (string-empty-p now) (or org-varde--new-offer default-directory) now)))))
    (goto-char (car b))
    (delete-region (car b) (cdr b))
    (insert (abbreviate-file-name (directory-file-name (expand-file-name dir))))))

(defun org-varde-board-edit ()
  "Edit the title and context of the task at point, in a buffer of its own."
  (interactive)
  (let* ((row (org-varde-board--row-at-point))
         (d (org-varde--details (plist-get row :task)))
         (buffer (get-buffer-create (format "*varde edit %s*" (plist-get row :short)))))
    (with-current-buffer buffer
      ;; An edit in progress is kept.
      (unless (buffer-modified-p)
        (org-varde-new-mode)
        (setq org-varde--new-task (plist-get row :task)
              org-varde--new-when (or (plist-get (plist-get d :schedule) :rule) ""))
        (org-varde--form-fill (or (plist-get d :title) "") :none (or (plist-get d :body) "")
                              org-varde--new-when))
      (setq org-varde--new-task (plist-get row :task)
            header-line-format (concat " " (org-varde--edit-keys "save" "cancel"))))
    (pop-to-buffer buffer)))

(defun org-varde-board-create ()
  "Write a new task in a buffer of its own, and start nothing.
On a board kept to a project, it goes there; else its project is asked
for when it is created, offering that of the task at point.  A draft put
away with \\<org-varde-new-mode-map>\\[org-varde-new-cancel] comes back."
  (interactive)
  (org-varde--new (plist-get (get-text-property (point) 'org-varde-row) :project)
                  ;; Kept to a project only once it has tasks: a repository
                  ;; of notes, say, is no place for one unasked.
                  (and org-varde-board--scoped org-varde-board--project
                       (org-varde-board--has org-varde-board--project)
                       org-varde-board--project)))

;;;###autoload
(defun org-varde-new-here ()
  "Write a new task from the heading at point, or the region, and start nothing.
The board's new-task buffer opens with the heading's title, or the
region's first line, and below it the heading's text, or the rest of the
region, and a From line: a link back here, in a buffer with a file.  The
task goes to the heading's VARDE_PROJECT, inherited, or else to this
file's repository once that has tasks; otherwise its project is asked
for.  Once it is made, a varde: link to it goes into the heading's text,
or below the region, unsaved; outside org, into the kill ring.  On a
heading that links an open task already, it offers to go to that."
  (interactive)
  (let* ((file (buffer-file-name (buffer-base-buffer)))
         (region (use-region-p))
         (heading (and (not region) (org-varde--heading-p)))
         (line (and region (line-number-at-pos (region-beginning))))
         (text (cond (region (org-varde-board--region-text (region-beginning) (region-end)))
                     (heading
                      (when-let* ((task (org-entry-get nil org-varde-property)))
                        (user-error "This heading is linked to %s already; M-x org-varde-convert-links makes it a link"
                                    (org-varde--short task)))
                      (concat (org-varde--title) "\n\n" (org-varde--body)))
                     (t (user-error "Not on a heading: mark the text to make a task from"))))
         (linked (and heading (car (org-varde--entry-rows))))
         (from (and file
                    (org-link-make-string
                     (concat "file:" file "::" (if region (number-to-string line) (org-link-heading-search-string)))
                     (if region
                         (format "%s:%d" (file-name-nondirectory file) line)
                       (string-join (cons (file-name-nondirectory file) (org-get-outline-path t)) " › ")))))
         (at (copy-marker (if region (region-end) (save-excursion (org-back-to-heading t) (point)))))
         (org (derived-mode-p 'org-mode))
         ;; Taken once, as the title and text are: varde never reads org.
         (scheduled (and heading (org-entry-get nil "SCHEDULED")))
         (dir (and org (org-entry-get nil "VARDE_PROJECT" t)))
         (repo (unless dir (org-varde-board--project-of default-directory))))
    (if (and linked (y-or-n-p (format "This heading links %s: go to it? (n makes a new task) "
                                      (org-varde--named (plist-get linked :task) (plist-get linked :title)))))
        (org-varde-go-task (plist-get linked :task) (plist-get linked :title))
      (deactivate-mark)
      (org-varde--new
       repo (cond (dir (expand-file-name dir)) ((and repo (org-varde-board--has repo)) repo))
       (concat (string-trim-right text) (if from (concat "\n\nFrom: " from) ""))
       (lambda (short title)
         (let ((buffer (marker-buffer at)))
           (if (not (and org buffer))
               (progn (kill-new (with-current-buffer (or buffer (current-buffer))
                                  (org-varde--link-text (concat "varde:" short) title)))
                      "; its link is in the kill ring")
             (with-current-buffer buffer
               (save-excursion
                 (goto-char at)
                 (set-marker at nil)
                 (if region (org-varde--link-below-region short title) (org-varde--insert-link short title)))
               (format "; linked in %s" (buffer-name))))))
       scheduled))))

(defun org-varde-board--region-text (beg end)
  "The text between BEG and END, as a task: its first line, the title, as
org shows a heading's when it starts before the heading's own words, and
without a list item's bullet, number or checkbox."
  (let* ((text (string-trim (buffer-substring-no-properties beg end)))
         (nl (string-search "\n" text))
         (first (substring text 0 nl))
         (title (save-excursion
                  (goto-char beg)
                  (if (and (derived-mode-p 'org-mode) (org-at-heading-p)
                           (progn (beginning-of-line)
                                  (looking-at org-complex-heading-regexp)
                                  (<= beg (or (match-beginning 4) (line-end-position)))))
                      (org-varde--title)
                    (replace-regexp-in-string
                     "\\`\\(?:[-+*]\\|[0-9]+[.)]\\)[ \t]+\\(?:\\[[ xX-]\\][ \t]+\\)?" "" first)))))
    (concat (if (string-empty-p title) first title) (if nl (substring text nl) ""))))

(defun org-varde--link-below-region (short title)
  "Insert a varde: link to SHORT, described by TITLE, below a region of
org text that ends at point.  Below its line in a paragraph; below the
table, block, drawer or list it ends in, never inside; in a heading's
text when it ends on the heading or among its planning and drawers."
  ;; A region of whole lines ends at the start of the next.
  (when (and (bolp) (not (bobp))) (backward-char))
  (if (or (org-at-heading-p)
          (and (not (org-before-first-heading-p))
               (< (point) (save-excursion (org-back-to-heading t) (org-end-of-meta-data t) (point)))))
      (org-varde--insert-link short title)
    (let ((el (org-element-at-point)))
      ;; The outermost element of the section it is in.
      (while (not (memq (org-element-type (org-element-property :parent el)) '(nil section org-data)))
        (setq el (org-element-property :parent el)))
      (if (eq (org-element-type el) 'paragraph)
          (forward-line 1)
        (goto-char (org-element-property :end el))
        (forward-line (- (org-element-property :post-blank el))))
      (unless (bolp) (insert "\n"))
      (insert (org-varde--link-text (concat "varde:" short) title) "\n"))))

(defun org-varde--new (offer here &optional text then rule)
  "Open the buffer that writes a new task.  OFFER is the project offered
when it is asked for, and HERE the one its Project starts as, which it
goes to without asking.  TEXT, when given, is what it starts with: its
first line the title, and the rest the context; a draft there goes to
the kill ring.  THEN is as `org-varde--new-then'.  RULE is what its When
starts as."
  (let ((buffer (get-buffer-create "*varde new*")))
    (with-current-buffer buffer
      (cond ((and (buffer-modified-p) (not text))
             (message "Your draft, as you left it"))
            (t (when (and (buffer-modified-p) (not (org-varde--form-blank-p)))
                 (kill-new (org-varde--form-text))
                 (message "Your draft is in the kill ring"))
               (org-varde-new-mode)
               (let* ((text (substring-no-properties (or text "")))
                      (nl (string-search "\n" text)))
                 ;; Unchanged, C-c C-k drops it.
                 (org-varde--form-fill (string-trim (substring text 0 nl)) here
                                       (if nl (string-trim (substring text nl)) "") rule))
               (setq org-varde--new-then then)))
      (setq org-varde--new-offer offer
            header-line-format (concat " " (org-varde--edit-keys "create" "later"))))
    (pop-to-buffer buffer)))

(defun org-varde-new-cancel ()
  "Put the task being written away, for `n' on the board to bring back.
Nothing written, all of it cleared, or left as it was filled in, it is
dropped: so a draft is dropped by clearing it first."
  (interactive)
  (cond (org-varde--new-task (org-varde--close-edit))
        ((or (not (buffer-modified-p)) (org-varde--form-blank-p))
         (let ((said (if (org-varde--form-blank-p) "Nothing written" "Left as it was filled in")))
           (set-buffer-modified-p nil)
           (org-varde--close-edit)
           (message "%s, nothing kept" said)))
        ;; Brought back by n on the board, it is a task of the board's:
        ;; nowhere to link it.
        (t (setq org-varde--new-then nil)
           (org-varde--close-edit t)
           (message "Draft kept: n on the board brings it back; cleared, it is dropped"))))

(defun org-varde-new-create (directory)
  "Create the task written in this buffer in the project DIRECTORY is in.
That of its Project field; when it has none, it is asked for.  A
buffer that edits a task saves its title and context instead."
  (interactive
   (pcase-let ((`(,title . ,body) (org-varde--new-text)))
     (when (and (string-blank-p title) (or org-varde--new-task (string-blank-p body)))
       (user-error (if org-varde--new-task "A task needs a title"
                     "A task needs a title, or context to make one from")))
     (let ((project (org-varde--form-value 'project)))
       (list (cond (org-varde--new-task nil)
                   ((not (string-empty-p project)) project)
                   (t (read-directory-name "Project directory for this task: "
                                           (and org-varde--new-offer
                                                (file-name-as-directory org-varde--new-offer)))))))))
  (if org-varde--new-task
      (org-varde--save-task)
    (org-varde--create-task directory)))

(defun org-varde--save-task ()
  "Save the title, context and schedule written in this buffer to its task."
  (pcase-let ((`(,title . ,body) (org-varde--new-text))
              (rule (or (org-varde--form-value 'when) "")))
    (org-varde--call body (list "set" org-varde--new-task (concat "--title=" title) "--body" "-"))
    (cond ((equal rule org-varde--new-when))
          ((string-empty-p rule) (org-varde--varde "schedule" org-varde--new-task "--off"))
          (t (org-varde--scheduled (list "schedule" org-varde--new-task "--" rule))))
    (set-buffer-modified-p nil)
    (org-varde--close-edit)
    (message "Saved \"%s\"" title))
  (org-varde-refresh))

(defun org-varde--scheduled (args &optional input)
  "Run varde with ARGS, which schedule a task, INPUT on stdin; return its
output.  When varde says nobody will be there to answer whether its
first start gets a worktree, it is asked now, and ARGS run again with
the answer."
  (condition-case e
      (org-varde--call input args)
    (org-varde-varde-error
     (if (not (string-search "--worktree" (cadr e)))
         (signal (car e) (cdr e))
       (let ((args (copy-sequence args))
             (tree (if (y-or-n-p "Give it a worktree of its own when it starts? ") "always" "never")))
         ;; Before the `--' that ends the options.
         (let ((at (or (seq-position args "--") (length args))))
           (org-varde--call input (append (seq-take args at) (list "--worktree" tree) (seq-drop args at)))))))))

(defun org-varde--create-task (directory)
  "Create the task written in this buffer in the project DIRECTORY is in."
  (pcase-let* ((`(,title . ,body) (org-varde--new-text))
               (project (expand-file-name directory))
               (body (and (not (string-blank-p body)) body))
               (rule (or (org-varde--form-value 'when) ""))
               (then org-varde--new-then)
         (new (org-varde--json
                     (condition-case e
                         (org-varde--scheduled (append (list "new" "--json" "--project" project)
                                                       (and body (list "--context" "-"))
                                                       (and (not (string-empty-p rule)) (list "--when" rule))
                                                       ;; None: varde makes one from the context.
                                                       (and (not (string-empty-p title)) (list "--" title)))
                                               body)
                       (org-varde-varde-error
                        ;; One older than titles made from the context.
                        (if (and (string-empty-p title) (string-search "<TITLE>" (cadr e)))
                            (user-error "This varde needs a title: write one, or update varde")
                          (signal (car e) (cdr e))))))))
    (set-buffer-modified-p nil)
    (org-varde--close-edit)
    ;; The board puts point on it once it shows it.
    (when-let* ((board (get-buffer org-varde-board-buffer)))
      (with-current-buffer board (setq org-varde-board--select (plist-get new :id))))
    (org-varde-board--reveal project)
    (message "Created %s in %s%s; %s"
             (if (string-empty-p title) (format "task %s" (plist-get new :short)) (format "\"%s\"" title))
             (file-name-nondirectory (directory-file-name project))
             (if then (funcall then (plist-get new :short) title) "")
             (if (string-empty-p rule) "RET on the board starts it" (concat "varde starts it: " rule))))
  (org-varde-refresh))

(defun org-varde-board--reveal (project)
  "Show every project on the board when it keeps to one other than PROJECT:
a task just made or moved there is in view."
  (when-let* ((board (get-buffer org-varde-board-buffer)))
    (with-current-buffer board
      (when (and org-varde-board--scoped org-varde-board--project
                 (not (equal (file-truename (file-name-as-directory project))
                             (file-truename (file-name-as-directory org-varde-board--project)))))
        (setq org-varde-board--scoped nil)
        (org-varde-board--render)))))

(defun org-varde--new-text ()
  "The task written in this buffer, as (TITLE . BODY).  TITLE may be
empty, and is one line: lines opened in it are joined."
  (cons (replace-regexp-in-string "[ \t]*\n[ \t\n]*" " " (org-varde--form-value 'title))
        (org-varde--form-value 'body)))

(defun org-varde--form-text ()
  "The task written in this buffer as one text: its title, a blank line
and its context."
  (pcase-let ((`(,title . ,body) (org-varde--new-text)))
    (string-trim (concat title "\n\n" body))))

(defun org-varde-board-move (directory)
  "Move the task at point to the project DIRECTORY is in.
Asked for, offering its project.  varde refuses while the task has an
active worktree or a live session."
  (interactive
   (let ((project (plist-get (org-varde-board--row-at-point) :project)))
     (list (read-directory-name "Move to project: " (and project (file-name-as-directory project))))))
  (let* ((task (plist-get (org-varde-board--row-at-point) :task))
         (dir (directory-file-name (expand-file-name directory)))
         (said (condition-case e
                   (string-trim (org-varde--varde "set" task "--project" dir))
                 (org-varde-varde-error (user-error "varde: %s" (cadr e))))))
    (org-varde-board--reveal dir)
    (message "%s" (if (string-empty-p said) "Already in that project" said)))
  (org-varde-refresh))

(defun org-varde-board--legend ()
  "What the board's groups hold, for its menu: in two columns, worded as
the terminal board's help has it."
  (let ((say (lambda (g) (concat (propertize (string-pad (car g) 11) 'face 'org-varde-board-group) (cdr g))))
        (groups '(("Needs you" . "turn ended, or it asks")
                  ("Working" . "a session at work")
                  ("Open" . "no session running")
                  ("Inbox" . "not made by you; a accepts")
                  ("Done" . "closed today, or work not landed")
                  ("Parked" . "put aside for later, folded"))))
    (mapconcat (lambda (i)
                 (concat (string-pad (funcall say (nth i groups)) 36) (funcall say (nth (+ i 3) groups))))
               '(0 1 2) "\n")))

(defun org-varde-board--menu-head ()
  "The head of the board's menu: what its groups hold, and what done and
park ask of a busy session."
  (concat (org-varde-board--legend) "\n"
          (propertize "d and P on a busy session ask: y stops it, n leaves it running" 'face 'org-varde-quiet) "\n"
          (propertize "u reopens what d, P or x closed; in a view, u first undoes a removal" 'face 'org-varde-quiet)))

(transient-define-prefix org-varde-board-menu ()
  "Everything the board does.  Start, done, park and accept act on every
task in the region."
  [:description org-varde-board--menu-head
   ["Go"
    ("RET" "go to it" org-varde-board-start)
    ("s" "start, stay" org-varde-board-start-background)
    ("R" "fresh" org-varde-board-start-fresh)
    ("F" "fork" org-varde-board-start-fork)]
   ["Task"
    ("n" "new" org-varde-board-create)
    ("e" "edit" org-varde-board-edit)
    ("m" "move" org-varde-board-move)
    ("N" "own session" org-varde-board-new)]
   ["Close"
    ("d" "done" org-varde-board-done :if org-varde--can-done-p)
    ("P" "park" org-varde-board-park :if org-varde--can-park-p)
    ("x" "drop" org-varde-board-drop)
    ("u" "reopen last" org-varde-board-reopen)
    ("a" "accept (Inbox)" org-varde-board-accept)]]
  [["Look"
    ("v" "view, cairns" org-varde-show :if-not org-varde--evil-p)
    ("K" "view, cairns" org-varde-show :if org-varde--evil-p)
    ("E" "cairn as text" org-varde-edit-cairn)
    ("=" "changes" org-varde-board-diff)
    ("/" "recall" org-varde-recall)
    ("y" "copy a link" org-varde-copy-link)]
   ;; Evil's keys in columns of their own: transient binds the keys of
   ;; a hidden suffix, but not those of a hidden column, and g cannot be
   ;; both a key and the start of gr.
   ["Move"
    :if-not org-varde--evil-p
    ("TAB" "needs you, S-TAB back" org-varde-board-next-needs-you)
    ("j" "next" org-varde-board-next-task)
    ("k" "previous" org-varde-board-previous-task)
    ("}" "next group" org-varde-board-next-group)
    ("{" "prev. group" org-varde-board-previous-group)]
   ["Board"
    :if-not org-varde--evil-p
    ("." "project/all" org-varde-board-toggle-project)
    ("z" "fold, TAB too" org-varde-board-toggle-group)
    ("g" "refresh" org-varde-refresh)
    ("q" "quit" quit-window)]
   ["Move"
    :if org-varde--evil-p
    ("TAB" "needs you, S-TAB back" org-varde-board-next-needs-you)
    ("j" "next" org-varde-board-next-task)
    ("k" "previous" org-varde-board-previous-task)
    ("gg" "first" org-varde-board-first-task)
    ("G" "last" org-varde-board-last-task)
    ("}" "next group" org-varde-board-next-group)
    ("{" "prev. group" org-varde-board-previous-group)]
   ["Board"
    :if org-varde--evil-p
    ("." "project/all" org-varde-board-toggle-project)
    ("za" "fold, TAB too" org-varde-board-toggle-group)
    ("gr" "refresh" org-varde-refresh)
    ("q" "quit" quit-window)]])

(defvar-keymap org-varde-board-mode-map
  :doc "The board's keys: the terminal board's wherever Emacs allows.
Rebind them here; under evil, its normal state takes them from here too."
  :parent special-mode-map
  "RET" #'org-varde-board-start
  "s" #'org-varde-board-start-background
  "R" #'org-varde-board-start-fresh
  "F" #'org-varde-board-start-fork
  "n" #'org-varde-board-create
  "e" #'org-varde-board-edit
  "N" #'org-varde-board-new
  "m" #'org-varde-board-move
  "d" #'org-varde-board-done
  "P" #'org-varde-board-park
  "x" #'org-varde-board-drop
  "u" #'org-varde-board-reopen
  "a" #'org-varde-board-accept
  "v" #'org-varde-show
  "K" #'org-varde-show
  "E" #'org-varde-edit-cairn
  "y" #'org-varde-copy-link
  "=" #'org-varde-board-diff
  "/" #'org-varde-recall
  "." #'org-varde-board-toggle-project
  "?" #'org-varde-board-menu
  "g" #'org-varde-refresh
  "j" #'org-varde-board-next-task
  "k" #'org-varde-board-previous-task
  "C-n" #'org-varde-board-next-task
  "C-p" #'org-varde-board-previous-task
  "<down>" #'org-varde-board-next-task
  "<up>" #'org-varde-board-previous-task
  "}" #'org-varde-board-next-group
  "{" #'org-varde-board-previous-group
  "M-n" #'org-varde-board-next-group
  "M-p" #'org-varde-board-previous-group
  "M-<" #'org-varde-board-first-task
  "M->" #'org-varde-board-last-task
  "TAB" #'org-varde-board-tab
  "<backtab>" #'org-varde-board-previous-needs-you
  "z" #'org-varde-board-toggle-group)

(defun org-varde-board--header ()
  "The key hints for the header line.
What is used most, and where the rest is."
  (mapconcat (lambda (pair)
               (concat (propertize (car pair) 'face 'help-key-binding) " " (cdr pair)))
             `(("RET" . "go") ("TAB" . "needs you") (,(if (org-varde--evil-p) "K" "v") . "view")
               ("n" . "new") ("d" . "done") ("?" . "help"))
             "  "))

(define-derived-mode org-varde-board-mode special-mode "varde"
  "The varde board.
\\{org-varde-board-mode-map}"
  (setq truncate-lines t)
  (setq header-line-format (concat " " (org-varde-board--header)))
  (org-varde--evil-keys org-varde-board-mode-map
                        "gr" #'org-varde-refresh
                        "gg" #'org-varde-board-first-task
                        "G" #'org-varde-board-last-task
                        "za" #'org-varde-board-toggle-group)
  (when (fboundp 'evil-define-key*)
    ;; What acts on the region acts on the selected lines.
    (evil-define-key* 'visual org-varde-board-mode-map
                      (kbd "RET") #'org-varde-board-start
                      "s" #'org-varde-board-start-background
                      "d" #'org-varde-board-done
                      "P" #'org-varde-board-park
                      "x" #'org-varde-board-drop
                      "a" #'org-varde-board-accept))
  ;; Git work changes outside varde's sight; showing the board looks again.
  (add-hook 'window-buffer-change-functions #'org-varde-board--shown nil t)
  ;; Titles are cut to the window, and what follows them kept to its edge.
  (add-hook 'window-size-change-functions #'org-varde-board--resized nil t)
  (hl-line-mode 1))

(defun org-varde-board--resized (window)
  "Fit the board to WINDOW's new size.  One that fits it shows from its
top, as when a view below it closes."
  (org-varde-board--refreshed)
  (when (and (window-live-p window)
             (with-current-buffer (window-buffer window)
               (<= (count-lines (point-min) (point-max)) (window-body-height window))))
    (set-window-start window 1)))

(defun org-varde-board--shown (_window)
  "Draw the board as it comes into view, and refresh it."
  (org-varde-board--render)
  (org-varde-refresh))

(defvar org-varde-board--home nil
  "The workspace the board lives in, under Doom's: where it was opened
while it had none.")

;;;###autoload
(defun org-varde-board ()
  "Show the varde board: where it already is, else in this window.
It keeps to the project of the buffer it is called from, as
`org-varde-board-scope' says: in org, the VARDE_PROJECT of the entry at
point, inherited, else the buffer's repository.  Come back to from a
task's terminal or view, it is as you left it, on that task."
  (interactive)
  (unless org-varde-mode (org-varde-mode 1))
  (let* ((board (get-buffer org-varde-board-buffer))
         (left (and board (or org-varde--task
                              (derived-mode-p 'org-varde-show-mode 'org-varde-recall-mode))))
         (opened (not (or left (equal (buffer-name) org-varde-board-buffer))))
         (dir (and opened (derived-mode-p 'org-mode) (org-entry-get nil "VARDE_PROJECT" t)))
         (project (and opened (org-varde-board--project-of (if dir (expand-file-name dir) default-directory)))))
    (with-current-buffer (get-buffer-create org-varde-board-buffer)
      (unless (derived-mode-p 'org-varde-board-mode) (org-varde-board-mode))
      (when (and (stringp left) (org-varde--row left))
        (setq org-varde-board--select left))
      (when opened
        (setq org-varde-board--project project
              org-varde-board--scoped (eq org-varde-board-scope 'project)))))
  (unless org-varde-board--ticking
    (setq org-varde-board--ticking (run-at-time 60 60 #'org-varde-board--tick)))
  ;; Under Doom, in its own workspace, whichever it is shown in too.
  (when (org-varde--workspaces-p)
    (if (member org-varde-board--home (+workspace-list-names))
        (unless (equal org-varde-board--home (org-varde--workspace-current))
          (org-varde--workspace-return org-varde-board--home))
      (setq org-varde-board--home (org-varde--workspace-current))))
  (let ((window (get-buffer-window org-varde-board-buffer)))
    (if window (select-window window) (pop-to-buffer-same-window org-varde-board-buffer)))
  ;; Drawn once in sight: in a workspace come back to, its window then
  ;; keeps its place.
  (org-varde-board--render)
  (org-varde-refresh))

(add-hook 'org-varde-after-refresh-hook #'org-varde-board--refreshed)

(provide 'org-varde-board)
;;; org-varde-board.el ends here
