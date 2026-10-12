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

(defface org-varde-board-group '((t :inherit (bold font-lock-function-name-face)))
  "Group headings on the board."
  :group 'org-varde)
(defface org-varde-board-empty '((t :inherit shadow))
  "Empty groups and quiet columns."
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
(defface org-varde-board-delegated '((t :inherit font-lock-constant-face))
  "A session whose sub-agents are at work."
  :group 'org-varde)

(defface org-varde-board-git '((t :inherit shadow))
  "Where a task's git work stands."
  :group 'org-varde)

(defconst org-varde-board-buffer "*varde*"
  "The board's buffer name.")

(defun org-varde-board--age (ms)
  "MS milliseconds as a short age."
  (let ((s (/ (max 0 ms) 1000)))
    (cond ((< s 60) (format "%ds" s))
          ((< s 3600) (format "%dm" (/ s 60)))
          ((< s 86400) (format "%dh" (/ s 3600)))
          (t (format "%dd" (/ s 86400))))))

(defun org-varde-board--background (row)
  "What ROW's session left running, e.g. \"  2 agents · 1 shell\"."
  (let ((parts (delq nil (mapcar (lambda (k)
                                   (let ((n (or (plist-get row (car k)) 0)))
                                     (and (> n 0) (format "%d %s%s" n (cdr k) (if (= n 1) "" "s")))))
                                 '((:agents . "agent") (:shells . "shell"))))))
    (if parts
        (propertize (concat "  " (string-join parts " · ")) 'face 'org-varde-board-empty)
      "")))

(defun org-varde-board--git (row)
  "ROW's git label, as varde worded it, padded to its column."
  (let* ((git (plist-get row :git))
         (face (cond ((equal (plist-get git :checks) "failing") 'org-varde-board-blocked)
                     ((equal (plist-get git :stage) "edited") 'org-varde-board-waiting)
                     ((equal (plist-get git :stage) "landed") 'org-varde-board-working)
                     (t 'org-varde-board-git))))
    (propertize (truncate-string-to-width (or (plist-get git :label) "") 10 nil ?\s "…") 'face face)))

(defun org-varde-board--row (row now group)
  "Insert one board ROW of GROUP, at time NOW in ms."
  (let* ((status (plist-get row :status))
         (face (pcase status
                 ("blocked" 'org-varde-board-blocked)
                 ("waiting" 'org-varde-board-waiting)
                 ("working" 'org-varde-board-working)
                 ("delegated" 'org-varde-board-delegated)
                 (_ 'org-varde-board-empty)))
         (glyph (pcase status
                  ;; Hollow once you have looked and left it.
                  ((or "blocked" "waiting") (if (plist-get row :unread) "●" "○"))
                  ((or "working" "delegated") "●")
                  ("done" "✓")
                  ("parked" "‖")
                  (_ "○")))
         (project (plist-get row :project))
         (start (point)))
    ;; The title goes last so it has the rest of the window and never
    ;; pushes a column out of line.
    (insert "  " (propertize glyph 'face face)
            " " (propertize (format "%-9s" status) 'face face)
            " " (org-varde-board--git row)
            " " (propertize (format "%4s" (org-varde-board--age (- now (plist-get row :last))))
                            'face 'org-varde-board-empty)
            "  " (propertize (truncate-string-to-width
                              (if project (file-name-nondirectory (directory-file-name project)) "-")
                              14 nil ?\s "…")
                             'face 'org-varde-board-empty)
            ;; Marks a task without a heading, so `f' and `l' are found
            ;; where it waits on you too; the Inbox group needs no mark.
            (if (or (plist-get row :external) (equal group "inbox"))
                "   "
              (propertize " ◇ " 'face 'org-varde-board-empty))
            (plist-get row :title)
            (let ((origin (plist-get row :origin)))
              (if origin (propertize (concat "  ← " (plist-get origin :short)) 'face 'org-varde-board-empty) ""))
            (org-varde-board--background row)
            "\n")
    (add-text-properties start (point) (list 'org-varde-task (plist-get row :task)
                                             'org-varde-row row))))

(defun org-varde-board--goto-task (task line)
  "Move to TASK's row, or to LINE when it is gone."
  (goto-char (point-min))
  (let ((match (and task (text-property-search-forward 'org-varde-task task t))))
    (if match
        (goto-char (prop-match-beginning match))
      (forward-line (1- line)))))

(defvar-local org-varde-board--toggled nil
  "Keys of the groups opened or closed here, against how varde shows them.")

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
             (inhibit-read-only t))
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
            (let* ((rows (plist-get group :rows))
                   (key (plist-get group :key))
                   (closed (xor (plist-get group :collapsed)
                                (member key org-varde-board--toggled))))
              (unless (bobp) (insert "\n"))
              (insert (propertize (plist-get group :label)
                                  'face (if (and rows (not closed)) 'org-varde-board-group 'org-varde-board-empty))
                      (propertize (format "  %d%s" (length rows) (if (and rows closed) " …" ""))
                                  'face 'org-varde-board-empty)
                      "\n")
              (put-text-property (line-beginning-position 0) (point) 'org-varde-group key)
              (unless closed
                (dolist (row rows) (org-varde-board--row row now key)))))))
        (org-varde-board--goto-task (car here) (cdr here))
        (dolist (w windows)
          (save-excursion
            (org-varde-board--goto-task (cadr w) (cddr w))
            (set-window-point (car w) (point))))))))

(defun org-varde-board--row-at-point ()
  "The board row at point."
  (or (get-text-property (point) 'org-varde-row)
      (user-error "No task on this line")))

(defun org-varde-board--rows ()
  "The rows in the active region, or else the row at point."
  (if (use-region-p)
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

(defun org-varde-board--next (prop n)
  "Move to the Nth line after this one that carries PROP.
Negative N moves back."
  (let ((step (if (< n 0) -1 1))
        (left (abs n))
        found)
    (save-excursion
      (beginning-of-line)
      (while (and (> left 0) (zerop (forward-line step)))
        (when (get-text-property (point) prop)
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

(defun org-varde-board-start (&optional arg)
  "Start, resume or go to the task at point.  Prefix ARG as `org-varde-start'.
Starting an Inbox task takes it out of the Inbox; it is not filed.
With a region, start every task in it in the background, going to no
terminal; one that cannot start is skipped, and a quit stops the rest.
Done, parked and dropped tasks are skipped, so none is reopened.
The worktree question is asked once for them all, and each project
question offers the last answer."
  (interactive "P")
  (if (not (use-region-p))
      (let ((row (org-varde-board--row-at-point)))
        (org-varde-start-task (plist-get row :task) (plist-get row :title) arg))
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
            (org-varde--answers nil))
        (funcall next rows)))))

(defun org-varde-board-done ()
  "Close the task at point, or every task in the region."
  (interactive)
  (dolist (row (org-varde-board--rows))
    (org-varde-done-task (plist-get row :task) (plist-get row :title))))

(defun org-varde-board-park ()
  "Park the task at point, or every task in the region.
That puts it aside, its worktree left as it is."
  (interactive)
  (dolist (row (org-varde-board--rows))
    (org-varde-park-task (plist-get row :task) (plist-get row :title))))

(defun org-varde-board-accept ()
  "Move the Inbox task at point, or every task in the region, into Open."
  (interactive)
  (apply #'org-varde--varde "accept" (mapcar (lambda (r) (plist-get r :task)) (org-varde-board--rows)))
  (org-varde-refresh))

(defun org-varde-board-toggle-group ()
  "Open or close the group at point, such as Parked, which starts closed."
  (interactive)
  (let ((key (save-excursion
               (end-of-line)
               (let ((match (text-property-search-backward 'org-varde-group nil nil)))
                 (if match (prop-match-value match) (user-error "Not in a group"))))))
    (setq org-varde-board--toggled
          (if (member key org-varde-board--toggled)
              (delete key org-varde-board--toggled)
            (cons key org-varde-board--toggled)))
    (org-varde-board--render)
    (goto-char (text-property-any (point-min) (point-max) 'org-varde-group key))))

(defun org-varde-board--headless ()
  "The row at point, refused when it already has a heading."
  (let ((row (org-varde-board--row-at-point)))
    (when (org-varde--find-heading (plist-get row :task))
      (user-error "This task has a heading; o goes to it"))
    row))

(defun org-varde-board-file ()
  "Give the task at point an org heading.
Beside the one it was deferred from, else where you choose.  It leaves
the Inbox."
  (interactive)
  (let* ((row (org-varde-board--headless))
         (at (org-varde--place (plist-get row :task))))
    (message "Filed in %s" (buffer-name (marker-buffer at))))
  (org-varde-refresh))

(defun org-varde-board-link ()
  "Link the task at point to an existing heading you pick."
  (interactive)
  (org-varde-link-task (plist-get (org-varde-board--headless) :task)))

(defun org-varde-board-throw-away ()
  "Throw the task at point away, once you confirm.
Its work is kept under refs/varde/discarded/."
  (interactive)
  (let ((row (org-varde-board--row-at-point)))
    (org-varde-throw-away-task (plist-get row :task) (plist-get row :title))))

(defun org-varde-board-visit ()
  "Go to the org heading of the task at point."
  (interactive)
  (org-varde--visit (plist-get (org-varde-board--row-at-point) :task)))

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
  "C-c C-k" #'kill-current-buffer)

(define-derived-mode org-varde-new-mode text-mode "varde new"
  "Write a new task: its title on the first line, and below it the
context for the agent.  \\[org-varde-new-create] creates it.")

(defvar-local org-varde--new-offer nil
  "The project offered when the task being written is created.")

(defun org-varde-board-create ()
  "Write a new task in a buffer of its own, and start nothing.
Its project is asked for when it is created, offering that of the task
at point."
  (interactive)
  (let ((project (plist-get (get-text-property (point) 'org-varde-row) :project))
        (buffer (get-buffer-create "*varde new*")))
    (with-current-buffer buffer
      ;; A task being written is kept.
      (unless (buffer-modified-p)
        (erase-buffer)
        (org-varde-new-mode)
        (setq header-line-format
              "First line: the title.  Below it: context for the agent.  C-c C-c create, C-c C-k cancel"))
      (setq org-varde--new-offer project))
    (pop-to-buffer buffer)))

(defun org-varde-new-create (directory)
  "Create the task written in this buffer in the project DIRECTORY is in."
  (interactive
   (let ((title (car (org-varde--new-text))))
     (when (string-blank-p title) (user-error "A task needs a title"))
     (list (read-directory-name "Project directory for this task: "
                                (and org-varde--new-offer
                                     (file-name-as-directory org-varde--new-offer))))))
  (pcase-let* ((`(,title . ,body) (org-varde--new-text))
               (project (expand-file-name directory))
               (body (and (not (string-blank-p body)) body))
               (new (org-varde--json
                     (org-varde--call body (append (list "new" "--json" "--project" project)
                                                   (and body (list "--body" "-"))
                                                   (list "--" title))))))
    (kill-current-buffer)
    (message "Created %s; RET starts it" (plist-get new :short)))
  (org-varde-refresh))

(defun org-varde--new-text ()
  "The task written in this buffer, as (TITLE . BODY)."
  (let* ((text (string-trim-left (buffer-string)))
         (split (string-search "\n" text)))
    (cons (string-trim (substring text 0 split))
          (string-trim (if split (substring text split) "")))))

(defun org-varde-board-move (directory)
  "Move the task at point to the project DIRECTORY is in.
Asked for, offering its project.  varde refuses while the task has an
active worktree or a live session.  A heading of the task that names a
VARDE_PROJECT, its own or inherited, gets the new one, so that its next
start does not move the task back."
  (interactive
   (let ((project (plist-get (org-varde-board--row-at-point) :project)))
     (list (read-directory-name "Move to project: " (and project (file-name-as-directory project))))))
  (let* ((task (plist-get (org-varde-board--row-at-point) :task))
         (dir (directory-file-name (expand-file-name directory)))
         (said (condition-case e
                   (string-trim (org-varde--varde "set" task "--project" dir))
                 (org-varde-varde-error (user-error "varde: %s" (cadr e))))))
    (org-varde--at-heading
     task (lambda ()
            (when (org-entry-get nil "VARDE_PROJECT" t)
              (org-varde--saving-if-clean
                (org-entry-put nil "VARDE_PROJECT" (abbreviate-file-name dir))))))
    (message "%s" (if (string-empty-p said) "Already in that project" said)))
  (org-varde-refresh))

(transient-define-prefix org-varde-board-menu ()
  "Everything the board does.  Start, done, park and accept act on every
task in the region."
  [["Start"
    ("RET" "start, or go to it (C-u fresh, C-u C-u fork)" org-varde-board-start)
    ("n" "new task" org-varde-board-create)
    ("N" "session of your own" org-varde-board-new)
    ("m" "move to another project" org-varde-board-move)]
   ["Close"
    ("d" "done" org-varde-board-done)
    ("P" "park" org-varde-board-park)
    ("x" "throw away" org-varde-board-throw-away)]
   ["Inbox"
    ("a" "accept" org-varde-board-accept)
    ("f" "file in org" org-varde-board-file)
    ("l" "link to a heading" org-varde-board-link)]
   ["Look"
    ("o" "heading" org-varde-board-visit)
    ("s" "show (e there edits the cairn)" org-varde-show)
    ("S" "its project cairn" org-varde-show-project)
    ("E" "edit the project cairn" org-varde-edit-project-cairn)
    ("/" "recall" org-varde-recall)]])

(defvar-keymap org-varde-board-mode-map
  :parent special-mode-map
  "RET" #'org-varde-board-start
  "d" #'org-varde-board-done
  "f" #'org-varde-board-file
  "x" #'org-varde-board-throw-away
  "/" #'org-varde-recall
  "?" #'org-varde-board-menu
  "P" #'org-varde-board-park
  "a" #'org-varde-board-accept
  "l" #'org-varde-board-link
  "o" #'org-varde-board-visit
  "s" #'org-varde-show
  "S" #'org-varde-show-project
  "E" #'org-varde-edit-project-cairn
  "n" #'org-varde-board-create
  "N" #'org-varde-board-new
  "m" #'org-varde-board-move
  "TAB" #'org-varde-board-toggle-group
  "g" #'org-varde-refresh
  "C-n" #'org-varde-board-next-task
  "C-p" #'org-varde-board-previous-task
  "M-n" #'org-varde-board-next-group
  "M-p" #'org-varde-board-previous-group)

(defun org-varde-board--header ()
  "The key hints for the header line.
What is used most, and where the rest is."
  (mapconcat (lambda (pair)
               (concat (propertize (car pair) 'face 'help-key-binding) " " (cdr pair)))
             '(("RET" . "start") ("n" . "new") ("d" . "done") ("f" . "file in org") ("x" . "throw away")
               ("?" . "more"))
             "  "))

(define-derived-mode org-varde-board-mode special-mode "varde"
  "The varde board.
\\{org-varde-board-mode-map}"
  (setq truncate-lines t)
  (setq header-line-format (concat " " (org-varde-board--header)))
  (org-varde--evil-keys org-varde-board-mode-map
                        "gr" #'org-varde-refresh
                        "gj" #'org-varde-board-next-group
                        "gk" #'org-varde-board-previous-group
                        (kbd "C-j") #'org-varde-board-next-task
                        (kbd "C-k") #'org-varde-board-previous-task)
  (when (fboundp 'evil-define-key*)
    ;; What acts on the region acts on the selected lines.
    (evil-define-key* 'visual org-varde-board-mode-map
      (kbd "RET") #'org-varde-board-start
      "d" #'org-varde-board-done
      "P" #'org-varde-board-park
      "a" #'org-varde-board-accept))
  ;; Git work changes outside varde's sight; showing the board looks again.
  (add-hook 'window-buffer-change-functions #'org-varde-board--shown nil t)
  (hl-line-mode 1))

(defun org-varde-board--shown (_window)
  "Refresh the board as it comes into view."
  (org-varde-refresh))

;;;###autoload
(defun org-varde-board ()
  "Show the varde board."
  (interactive)
  (unless org-varde-mode (org-varde-mode 1))
  (with-current-buffer (get-buffer-create org-varde-board-buffer)
    (unless (derived-mode-p 'org-varde-board-mode) (org-varde-board-mode))
    (org-varde-board--render))
  (pop-to-buffer-same-window org-varde-board-buffer)
  ;; Headings and tasks changed while Emacs was away come into step now.
  (org-varde-reconcile)
  (org-varde-refresh))

(add-hook 'org-varde-after-refresh-hook #'org-varde-board--render)

(provide 'org-varde-board)
;;; org-varde-board.el ends here
