;;; org-varde.el --- Org-mode client for varde -*- lexical-binding: t; -*-

;; Copyright (C) 2026 Morten Smebye

;; Author: Morten Smebye <mortensmeb@gmail.com>
;; URL: https://github.com/Smebbs/varde
;; Version: 1.0.3
;; Package-Requires: ((emacs "29.1") (org "9.6") (transient "0.4"))
;; Keywords: tools, outlines
;; SPDX-License-Identifier: MIT

;; This file is not part of GNU Emacs.  It is free software under the
;; MIT License; see the LICENSE file that comes with it.

;;; Commentary:

;; varde runs AI coding-agent sessions (Claude Code or pi) as tasks and
;; keeps what they learned.  It is a separate command-line program: see
;; the README at the URL above to install it.  This package makes org
;; headings into varde tasks and drives varde from Emacs: start or
;; resume a heading's session in a terminal, watch the board
;; (`org-varde-board'), close tasks, read and edit their cairns, and
;; search earlier work.
;;
;; varde owns everything durable.  Org keeps the VARDE_TASK property on
;; a heading you start, link or file, and the heading owns its title and
;; body, which go to varde each time it starts.  Besides that property,
;; org-varde writes only todo keywords: when you close, park or throw
;; away a task from the board, and when a task is closed, parked or
;; reopened elsewhere, its heading follows.  It saves such a change only
;; when it is the buffer's only one.

;;; Code:

(require 'filenotify)
(require 'json)
(require 'org)
(require 'org-refile)
(require 'seq)
(require 'subr-x)

(declare-function vterm "vterm" (&optional arg))
(declare-function vterm-send-string "vterm" (string &optional paste-p))
(declare-function vterm-send-return "vterm" ())
(declare-function term-char-mode "term" ())
(declare-function +workspace-list-names "ext:workspaces" ())
(declare-function +workspace-current-name "ext:workspaces" ())
(declare-function +workspace-switch "ext:workspaces" (name &optional auto-create-p))
(declare-function +workspace-kill "ext:workspaces" (name))
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

(defcustom org-varde-agent-command "claude"
  "What `org-varde-new-session' runs for a session of your own."
  :type 'string)

(defcustom org-varde-use-worktree 'ask
  "Whether a task's first run gets its own git worktree.
`ask' asks, the first time a task starts, when varde needs to know."
  :type '(choice (const ask) (const always) (const never)))

(defcustom org-varde-context-words 150
  "Most words of org context (outline and parent prose) sent with a start."
  :type 'integer)

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

(defcustom org-varde-park-keywords '("SOMEDAY")
  "Todo keywords that park a linked heading's task: put aside, not done.
The first is what parking from the board sets, when the heading's file
knows it."
  :type '(repeat string))

(defconst org-varde-property "VARDE_TASK"
  "The property that links a heading to its varde task.")

(define-error 'org-varde-varde-error "varde")

;; Bound by org around `org-after-todo-state-change-hook'.
(defvar org-last-state)

(defvar org-varde-mode)
(defvar org-varde--watch nil "The watch on varde's state directory.")

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
                          (progn (insert input)
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
         (done (lambda (ok text)
                 (when (process-live-p stderr) (delete-process stderr))
                 (kill-buffer out)
                 (kill-buffer err)
                 (condition-case e
                     (if ok
                         (funcall callback text)
                       (message "varde: %s" (string-remove-prefix "varde: " (string-trim text)))
                       (when on-error (funcall on-error)))
                   (error (message "org-varde: %s" (error-message-string e)))))))
    (condition-case e
        (let ((proc (make-process
                     :name "varde" :buffer out :stderr stderr :noquery t :connection-type 'pipe
                     :command (cons org-varde-varde-program args)
                     :sentinel (lambda (proc _)
                                 (unless (process-live-p proc)
                                   (let ((ok (eq (process-exit-status proc) 0)))
                                     (funcall done ok (with-current-buffer (if ok out err)
                                                        (buffer-string)))))))))
          (when input (process-send-string proc input))
          (process-send-eof proc)
          proc)
      (error (funcall done nil (error-message-string e)) nil))))

(defun org-varde--short (task)
  "The six characters of TASK's id people see and type."
  (downcase (substring task (max 0 (- (length task) 6)))))

(defun org-varde--details (task)
  "TASK as `varde show --json --no-git' gives it."
  (org-varde--varde-json "show" task "--json" "--no-git"))

;;; --- Headings ----------------------------------------------------------------

(defmacro org-varde--saving-if-clean (&rest body)
  "Run BODY, and save the buffer when BODY made its only change.
Unsaved edits of your own are never saved for you."
  (declare (indent 0) (debug t))
  (let ((clean (make-symbol "clean")))
    `(let ((,clean (and buffer-file-name (not (buffer-modified-p)))))
       (prog1 (progn ,@body)
         (when ,clean (save-buffer))))))

(defun org-varde--heading-p ()
  "Non-nil when point is on or under an org heading."
  (and (derived-mode-p 'org-mode) (not (org-before-first-heading-p))))

(defun org-varde--title ()
  "The heading's title, without keyword, priority, tags or fontification."
  (substring-no-properties (org-get-heading t t t t)))

(defun org-varde--body ()
  "The heading's own text: below its metadata, above its first child.
Drawers are left out; they are org's bookkeeping, not the task."
  (save-excursion
    (org-back-to-heading t)
    (let ((end (save-excursion (outline-next-heading) (point))))
      (org-end-of-meta-data t)
      (let ((text (buffer-substring-no-properties (min (point) end) end)))
        (string-trim
         (replace-regexp-in-string
          "^[ \t]*:[A-Za-z_-]+:[ \t]*\n\\(?:.*\n\\)*?[ \t]*:END:[ \t]*\n?" "" text))))))

(defun org-varde--project ()
  "The project directory for a new task at point.
From an inherited VARDE_PROJECT property, else asked for."
  (expand-file-name
   (or (org-entry-get nil "VARDE_PROJECT" t)
       (read-directory-name "Project directory for this task: "))))

(defun org-varde--moved-project (task)
  "The heading's inherited VARDE_PROJECT, when TASK may be elsewhere.
Nil when the heading names none, one not on this machine (a synced
file), or the board last saw TASK there."
  (let ((dir (org-entry-get nil "VARDE_PROJECT" t))
        (row (org-varde--row task)))
    (when (and dir (file-directory-p (expand-file-name dir)))
      (setq dir (directory-file-name (expand-file-name dir)))
      ;; Unknown to the board: varde compares, and a move to where the
      ;; task already is changes nothing.
      (unless (and row (equal (plist-get row :project) dir))
        dir))))

(defun org-varde--context ()
  "Where the heading sits in org, for a fresh session.
That is its file, its outline path and the nearest ancestor's prose,
within `org-varde-context-words'.  The file is named in full and as
org-mode: a bare \"notes.org\" reads as a web address, and the session
can read the rest of the file itself."
  (save-excursion
    (org-back-to-heading t)
    (let* ((path (org-get-outline-path))
           (file (buffer-file-name (buffer-base-buffer)))
           (parent (when (and path (org-up-heading-safe)) (org-varde--body)))
           (text (concat "This task is a heading in an org-mode file"
                         (when file (concat ", " (expand-file-name file)))
                         (when path (concat ", under: " (string-join path " > ")))
                         "."
                         (when (and parent (not (string-empty-p parent)))
                           (concat "\n\nThe parent heading says:\n" parent))))
           (words (split-string text "[ \t]+" t)))
      (if (<= (length words) org-varde-context-words)
          text
        (concat (string-join (seq-take words org-varde-context-words) " ") " …")))))

(defun org-varde--ensure-task ()
  "Return the varde task of the heading at point, making one if needed.
Its title and body are sent to varde each time: the heading owns them.
So is its inherited VARDE_PROJECT when it is not the task's, which
moves the task there; when varde refuses (the task has an active
worktree or a live session), you are told why and it stays.
A new link is saved when it is the buffer's only change, so a revert
cannot lose it; unsaved edits of your own are never saved for you."
  (save-excursion
    (org-back-to-heading t)
    (let ((id (org-varde--own-link))
          (title (org-varde--title))
          (body (org-varde--body)))
      (if id
          (let* ((args (list "set" id (concat "--title=" title) "--body" "-"))
                 (project (org-varde--moved-project id))
                 (moving (and project (append args (list "--project" project)))))
            ;; A live session is gone to, not started: its text can follow.
            (if (member (plist-get (org-varde--row id) :status) '("working" "delegated" "waiting" "blocked"))
                (org-varde--varde-async (or moving args) #'ignore
                                        ;; Refused, the title and body still go.
                                        (and moving (lambda () (org-varde--varde-async args #'ignore nil body)))
                                        body)
              (condition-case e
                  (let ((said (string-trim (org-varde--call body (or moving args)))))
                    (unless (string-empty-p said) (message "varde: %s" said)))
                (org-varde-varde-error
                 (unless moving (signal (car e) (cdr e)))
                 (org-varde--call body args)
                 (message "varde: %s" (cadr e)))))
            id)
        (let* ((project (org-varde--project))
               (new (org-varde--json
                     (org-varde--call body (list "new" "--project" project "--external" "org"
                                                 "--body" "-" "--json" "--" title))))
               (id (plist-get new :id)))
          (org-varde--put-link id)
          id)))))

(defun org-varde--own-link ()
  "The task the heading at point links to, unless another heading does too.
Such as a copy, which `org-copy-subtree' makes with the link: you are
asked to give this heading a task of its own, and refusing changes
nothing."
  (let* ((id (org-entry-get nil org-varde-property))
         (here (cons (org-varde--file) (point)))
         (others (seq-remove (lambda (l) (equal (cons (nth 1 l) (nth 2 l)) here))
                             (and id (org-varde--headings id)))))
    (cond ((null others) id)
          ((y-or-n-p (format "%s is linked from %s too; give this heading a task of its own? "
                             (org-varde--short id) (org-varde--where (car others))))
           (org-varde--saving-if-clean (org-entry-delete nil org-varde-property))
           nil)
          (t (user-error "Remove VARDE_TASK from the copy of %s's heading" (org-varde--short id))))))

(defun org-varde--put-link (task)
  "Link the heading at point to TASK.
Saved when the link is the buffer's only change, so a revert cannot lose it."
  (org-varde--saving-if-clean
    (org-entry-put nil org-varde-property task))
  (unless (member (org-varde--file) (org-varde-files t))
    (message "org-varde: this file is outside `org-directory' and the agenda; \
add its directory to `org-varde-directories' so the heading is found when it is closed")))

;;;###autoload
(defun org-varde-link ()
  "Link the heading at point to a task that has no heading.
Such as a session you started outside Emacs: Inbox tasks are offered
first.  The task takes the heading's title and body, which the heading
owns from then on, as if started here."
  (interactive)
  (unless (org-varde--heading-p)
    (user-error "Not on an org heading"))
  (when (org-entry-get nil org-varde-property)
    (user-error "This heading is already linked to a task"))
  (let ((choices (org-varde--unlinked)))
    (unless choices (user-error "Every task already has a heading"))
    (org-varde--link-here (cdr (assoc (completing-read "Link to task: " choices nil t) choices)))
    (org-varde-refresh)))

(defun org-varde--unlinked ()
  "Tasks without a heading, as (LABEL . TASK), Inbox first.
Those never linked, and those whose heading is gone."
  (let* ((headed (mapcar #'car (org-varde--linked)))
         (data (org-varde--varde-json "list" "--json" "--all"))
         (groups (if (org-varde--readable data)
                     (plist-get data :groups)
                   (user-error "Update org-varde to read this varde's tasks")))
         (groups (append (seq-filter (lambda (g) (equal (plist-get g :key) "inbox")) groups)
                         (seq-remove (lambda (g) (equal (plist-get g :key) "inbox")) groups))))
    (seq-mapcat (lambda (g)
                  (seq-keep (lambda (r)
                              (unless (member (plist-get r :task) headed)
                                (cons (format "%s  %-9s %s" (plist-get r :short) (plist-get g :label)
                                              (plist-get r :title))
                                      (plist-get r :task))))
                            (plist-get g :rows)))
                groups)))

(defun org-varde--link-here (task)
  "Link the heading at point to TASK.
The task leaves the Inbox and takes the heading's title and body, but
keeps its project."
  (org-varde--varde "link" task "org")
  (save-excursion
    (org-back-to-heading t)
    (org-varde--put-link task)
    (org-varde--saving-if-clean (org-varde--keep-project task))
    (org-varde--ensure-task)))

(defun org-varde--keep-project (task &optional details)
  "Name TASK's project on the heading at point when it inherits another.
So linking never moves a task.  DETAILS, from `org-varde--details', saves
asking varde."
  (when-let* ((dir (org-entry-get nil "VARDE_PROJECT" t))
              (project (plist-get (or details (org-varde--row task) (org-varde--details task))
                                  :project)))
    (unless (equal (directory-file-name (file-truename dir))
                   (directory-file-name (file-truename project)))
      (org-entry-put nil "VARDE_PROJECT" (abbreviate-file-name project)))))

(defun org-varde--pick-heading (prompt)
  "Ask with PROMPT for a heading among your refile targets.
Without any, the headings of the agenda files, or else of the files
`org-varde-files' finds, are offered."
  (let ((org-refile-targets (or (seq-filter #'car org-refile-targets)
                                '((org-varde--refile-files :maxlevel . 3))))
        (org-refile-use-outline-path (or org-refile-use-outline-path 'file)))
    (org-refile-get-location prompt)))

(defun org-varde--refile-files ()
  "Files to pick a heading in when you have no refile targets."
  (or (org-agenda-files t) (seq-remove #'org-varde--archive-file-p (org-varde-files t))))

(defun org-varde-link-task (task)
  "Link TASK to an existing heading you pick.
The heading's title and body replace the task's."
  (let ((target (org-varde--pick-heading "Link to heading")))
    (unless (nth 3 target) (user-error "Pick a heading, not a file"))
    (with-current-buffer (find-file-noselect (nth 1 target))
      ;; The position is in the whole buffer, which may be narrowed.
      (org-with-wide-buffer
        (goto-char (nth 3 target))
        (let ((other (org-entry-get nil org-varde-property)))
          (when other
            (user-error "That heading is already linked to %s" (org-varde--short other))))
        (org-varde--link-here task)))
    (org-varde-refresh)))

(defun org-varde--place (task)
  "Write a heading for TASK, linked to it, and return a marker at it.
A deferred task goes beside the heading it was deferred from; any
other, or one whose origin has no live heading, where you choose among
your refile targets.  Refile it later as any heading."
  (let* ((d (org-varde--details task))
         (beside (let ((origin (plist-get d :origin)))
                   (and origin
                        (org-varde--at-heading
                         origin (lambda () (unless (org-varde--archived-p) (point-marker)))))))
         (target (unless beside (org-varde--pick-heading "File task under")))
         (buffer (if beside (marker-buffer beside) (find-file-noselect (nth 1 target))))
         (body (plist-get d :body))
         (level 1))
    (with-current-buffer buffer
      (org-varde--saving-if-clean
        (org-with-wide-buffer
         (cond (beside (goto-char beside)
                       (setq level (org-current-level))
                       (org-end-of-subtree t t))
               ((nth 3 target) (goto-char (nth 3 target))
                (setq level (1+ (org-current-level)))
                (org-end-of-subtree t t))
               (t (goto-char (point-max))))
         (unless (bolp) (insert "\n"))
         (let ((at (point-marker)))
           (insert (make-string level ?*) " " (or (car org-todo-keywords-1) "TODO") " "
                   (plist-get d :title) "\n")
           (when body
             ;; A line starting with a star would become a heading.
             (insert (replace-regexp-in-string "^\\*" " *" body) "\n"))
           (goto-char at)
           (org-entry-put nil org-varde-property task)
           (org-varde--keep-project task d)
           (org-varde--varde "link" task "org")
           at))))))

(defcustom org-varde-directories nil
  "Directories besides `org-directory' whose org files may hold linked headings.
Headings are looked up in every .org and *_archive file under these
directories and `org-directory', in the agenda files and their archives,
and in every org buffer visiting a file.  A heading elsewhere is found
only while its file is open."
  :type '(repeat directory))

(defvar org-varde--trees nil
  "Cached walks of the search directories: (ROOT DIRS FILES).
DIRS pairs each directory with its modification time, so a walk is
redone only when a file is added, renamed or removed under ROOT.")

(defun org-varde--mtime (file)
  "FILE's modification time, or nil when it is gone."
  (file-attribute-modification-time (file-attributes file)))

(defun org-varde--tree-files (root)
  "The .org and *_archive files under ROOT, skipping hidden directories.
ROOT is a true name, and so is each file."
  (let ((hit (assoc root org-varde--trees)))
    (if (and hit (seq-every-p (lambda (d) (equal (org-varde--mtime (car d)) (cdr d))) (nth 1 hit)))
        (nth 2 hit)
      (let (dirs files)
        (named-let walk ((dir root))
          (push (cons dir (org-varde--mtime dir)) dirs)
          ;; An unreadable directory is skipped, not an error.
          (dolist (f (ignore-errors (directory-files-and-attributes dir t "\\`[^.]" t)))
            (cond ((eq (file-attribute-type (cdr f)) t) (walk (car f)))
                  ((string-match-p "\\.org\\'\\|_archive\\'" (car f))
                   ;; A symlink by its true name, as `org-varde-files' has them.
                   (push (if (stringp (file-attribute-type (cdr f))) (file-truename (car f)) (car f))
                         files)))))
        (setq org-varde--trees (cons (list root dirs files) (delq hit org-varde--trees)))
        files))))

(defun org-varde--agenda-archives ()
  "Each agenda file's archive per the global `org-archive-location'.
By their true names."
  (let ((loc (car (split-string (default-value 'org-archive-location) "::"))))
    (mapcar (lambda (f)
              (file-truename
               (if (string-empty-p loc)
                   f
                 (expand-file-name (format loc (file-name-nondirectory f)) (file-name-directory f)))))
            (org-agenda-files t))))

(defun org-varde--archive-file-p (file)
  "Whether FILE is an archive: named *_archive, or an agenda file's archive."
  (and file (or (string-suffix-p "_archive" file)
                (member (file-truename file) (org-varde--agenda-archives)))
       t))

(defun org-varde--file ()
  "The current buffer's file by its true name, or nil.
An indirect buffer's is its base buffer's."
  (let ((file (buffer-file-name (buffer-base-buffer))))
    (and file (file-truename file))))

(defun org-varde-files (&optional unvisited)
  "Org files that may hold linked headings, by their true names.
See `org-varde-directories'.  With UNVISITED, leave out files found
only because a buffer visits them.  A directory is resolved once, not
each file in it."
  (seq-uniq
   (seq-filter #'file-readable-p
               (append (mapcar #'file-truename (org-agenda-files t))
                       (org-varde--agenda-archives)
                       (seq-mapcat (lambda (d) (and d (file-directory-p d)
                                                    (org-varde--tree-files (file-truename d))))
                                   (cons org-directory org-varde-directories))
                       (unless unvisited
                         (delq nil (mapcar (lambda (b) (with-current-buffer b
                                                       (and buffer-file-name (derived-mode-p 'org-mode)
                                                            (file-truename buffer-file-name))))
                                           (buffer-list))))))))

(defun org-varde--done-words (lines)
  "The done keywords in LINES.
Each line is a list of keywords as `org-todo-keywords' or a #+TODO
line has them, and its done keywords are those after \"|\", or else
the last."
  (seq-mapcat (lambda (words)
                (let* ((words (mapcar (lambda (k) (replace-regexp-in-string "(.*" "" k)) words))
                       (bar (seq-position words "|")))
                  (if bar (seq-drop words (1+ bar)) (last words))))
              lines))

(defvar org-varde--scans (make-hash-table :test #'equal)
  "Linked headings per file: FILE to (STAMP . HEADINGS).
STAMP is the file's modification time, or the visiting buffer's tick.")

(defun org-varde--scan (file)
  "The linked headings in FILE.
Read from its buffer when it is open, so unsaved links count.  Each is
\(TASK FILE POSITION DONE-P ARCHIVED-P PARKED-P TIME): archived means
in an archive file, or tagged ARCHIVE on itself or an ancestor, and
TIME is when it was closed or archived, if it says."
  (let* ((buf (find-buffer-visiting file))
         (stamp (list (if buf (buffer-chars-modified-tick buf) (org-varde--mtime file))
                      org-todo-keywords org-varde-park-keywords))
         (hit (gethash file org-varde--scans)))
    (if (equal (car hit) stamp)
        (cdr hit)
      (let ((found (org-varde--scan-text file buf)))
        (puthash file (cons stamp found) org-varde--scans)
        found))))

(defun org-varde--scan-text (file buf)
  "Scan FILE, or BUF when it visits FILE, for linked headings."
  (with-temp-buffer
    ;; Widened, so positions are the buffer's even while it is narrowed.
    (if buf
        (insert (with-current-buffer buf (org-with-wide-buffer (buffer-substring-no-properties (point-min) (point-max)))))
      (insert-file-contents file))
    (let ((done (if (and buf (buffer-local-value 'org-done-keywords buf))
                    (buffer-local-value 'org-done-keywords buf)
                  (org-varde--done-words
                   (append (mapcar #'cdr org-todo-keywords)
                           (save-excursion
                             (goto-char (point-min))
                             (let (lines)
                               (while (re-search-forward "^#\\+\\(?:SEQ_\\|TYP_\\)?TODO:[ \t]*\\(.*\\)$" nil t)
                                 (push (split-string (match-string 1)) lines))
                               lines))))))
          (in-archive (org-varde--archive-file-p file))
          (property (format "^[ \t]*:%s:[ \t]+\\(\\S-+\\)" org-varde-property))
          (case-fold-search nil)
          (archived-at nil)             ; levels of ARCHIVE-tagged ancestors
          found)
      (goto-char (point-min))
      (while (re-search-forward "^\\(\\*+\\) +\\(\\S-*\\)" nil t)
        (let* ((pos (line-beginning-position))
               (level (length (match-string 1)))
               (keyword (match-string 2))
               (tagged (string-match-p "[ \t]:\\(?:[^ \t:]+:\\)*ARCHIVE:\\(?:[^ \t:]+:\\)*[ \t]*$"
                                       (buffer-substring pos (line-end-position))))
               closed
               ;; A property drawer sits right below the heading and
               ;; its planning line.
               (drawer-end (save-excursion
                             (forward-line 1)
                             (when (looking-at "[ \t]*\\(?:CLOSED\\|SCHEDULED\\|DEADLINE\\):")
                               (when (re-search-forward "CLOSED: *\\[\\([^]]+\\)\\]" (line-end-position) t)
                                 (setq closed (match-string 1)))
                               (forward-line 1))
                             (and (looking-at "[ \t]*:PROPERTIES:")
                                  (re-search-forward "^[ \t]*:END:" nil t)))))
          (setq archived-at (seq-filter (lambda (l) (< l level)) archived-at))
          (let ((archived (or in-archive tagged archived-at)))
            (when tagged (push level archived-at))
            (when (and drawer-end (save-excursion (re-search-forward property drawer-end t)))
              (let ((task (match-string 1))
                    (archive-time (and archived
                                       (save-excursion
                                         (re-search-forward "^[ \t]*:ARCHIVE_TIME:[ \t]+\\(.+?\\)[ \t]*$"
                                                            drawer-end t))
                                       (match-string 1))))
                (push (list task file pos (and (member keyword done) t) (and archived t)
                            (and (member keyword org-varde-park-keywords) t)
                            (org-varde--org-time (or closed archive-time)))
                      found))))))
      (nreverse found))))

(defun org-varde--org-time (stamp)
  "Org timestamp text STAMP as Unix milliseconds, or nil.
At the start of its minute: org keeps no seconds, so a task changed in
that same minute counts as later.  A keyword set in org reaches varde
within a second, so what still disagrees then is a change made
elsewhere afterwards, such as a reopen right after a close."
  (and stamp
       (ignore-errors (* 1000 (truncate (float-time (org-time-string-to-time stamp)))))))

(defun org-varde--linked ()
  "Every linked heading, as `org-varde--scan' gives them."
  (seq-mapcat #'org-varde--scan (org-varde-files)))

(defun org-varde--headings (task)
  "TASK's linked headings: one, unless a heading was copied with its link."
  (seq-filter (lambda (l) (equal (car l) task)) (org-varde--linked)))

(defun org-varde--where (linked)
  "Where the LINKED heading is, as FILE:LINE."
  (format "%s:%d" (file-name-nondirectory (nth 1 linked))
          (with-current-buffer (find-file-noselect (nth 1 linked))
            (org-with-wide-buffer (line-number-at-pos (nth 2 linked))))))

(defun org-varde--find-heading (task)
  "Return (FILE . POSITION) of TASK's heading, or nil.
Refused when it has more than one: either may be the one meant."
  (pcase (org-varde--headings task)
    ('nil nil)
    (`(,l) (cons (nth 1 l) (nth 2 l)))
    (all (user-error "%s is linked from %s; remove VARDE_TASK from the copy"
                     (org-varde--short task) (mapconcat #'org-varde--where all " and ")))))

(defun org-varde--at-heading (task fn)
  "Call FN with point on TASK's heading, if it has one.  Return FN's value."
  (let ((where (org-varde--find-heading task)))
    (when where
      (with-current-buffer (find-file-noselect (car where))
        (save-excursion
          (save-restriction
            (widen)
            (goto-char (cdr where))
            (funcall fn)))))))

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
        (with-current-buffer buffer (term-char-mode))
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
  "Close TASK's workspace, if it has one."
  (let ((name (org-varde--workspace task)))
    (when name (org-varde--workspace-try "close" #'+workspace-kill name))))

(defcustom org-varde-start-goes nil
  "Whether starting a session also goes to its terminal.
When nil it runs in the background, and starting the task again goes to it."
  :type 'boolean)

(defvar org-varde--background nil
  "Non-nil while starting tasks without going to any terminal.
The board binds it to start every task in the region.")

(defun org-varde--open-terminal (task title cwd command)
  "Run COMMAND for TASK in its terminal, opening one in CWD if needed.
TITLE names a workspace made for it."
  (let ((from (org-varde--workspace-current)))
    (org-varde--workspace-open task title)
    (let ((buffer (or (org-varde--terminal task)
                      (funcall org-varde-terminal-function cwd
                               (format "*varde %s*" (org-varde--short task))))))
      ;; Something already running there, such as the session a start a
      ;; moment ago launched, would take the command as its input.
      (if (org-varde--terminal-busy-p buffer)
          (progn (unless org-varde--background (pop-to-buffer-same-window buffer))
                 (message "%s is already starting in its terminal" (org-varde--short task)))
        (with-current-buffer buffer
          (setq org-varde--task task)
          (funcall org-varde-send-function command))
        (cond ((and org-varde-start-goes (not org-varde--background))
               (pop-to-buffer-same-window buffer))
              ((not (equal from (org-varde--workspace-current)))
               ;; Lay the task's workspace out with its terminal, then come back.
               (pop-to-buffer-same-window buffer)
               (org-varde--workspace-return from)))
        (unless (and org-varde-start-goes (not org-varde--background))
          (message "Started %s; start it again to go to it" (org-varde--short task))))
      buffer)))

(defun org-varde--terminal-busy-p (buffer)
  "Whether BUFFER's terminal is running something, rather than at its shell."
  (and buffer (get-buffer-process buffer)
       (process-running-child-p (get-buffer-process buffer))))

;;; --- Starting ----------------------------------------------------------------

(defun org-varde--plan (task want context title then fail)
  "Ask varde to start TASK, without waiting.
WANT is nil, \"--fresh\" or \"--fork\".
CONTEXT, when non-nil, is org context for a fresh session.  TITLE names
the task in a question.  THEN and FAIL are as for
`org-varde--plan-answering'."
  (let* ((file (and context (make-temp-file "varde-context-" nil ".md")))
         (args (append (list "start" task "--json")
                       (and want (list want))
                       (and file (list "--context-file" file))
                       (pcase org-varde-use-worktree
                         ('always '("--worktree" "always"))
                         ('never '("--worktree" "never")))))
         (cleanup (lambda () (when file (delete-file file)))))
    ;; `make-temp-file' given the text announces "Wrote ..." on every start.
    (when file (write-region context nil file nil 'silent))
    (org-varde--plan-answering args title
                               (lambda (plan) (funcall cleanup) (funcall then plan))
                               (lambda (why) (funcall cleanup) (funcall fail why)))))

(defvar org-varde--answers nil
  "Answers given while the board starts the tasks in a region, as a plist.
The worktree answer holds for every task in it.  The project answer is
only the next one's default: each task may belong to another project.")

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

(defun org-varde--answer (plan args title)
  "ARGS with the answer to what PLAN asks added, or nil if it asks nothing.
TITLE names the task in the question."
  (pcase (and (equal (plist-get plan :action) "ask") (plist-get plan :ask))
    ((and "project" (guard (not (member "--project" args))))
     (let* ((choices (append (plist-get plan :choices) nil))
            (default (or (plist-get org-varde--answers :project) (car choices)))
            (project (expand-file-name
                      (read-directory-name (format "Project directory for %s: " (or title "this task"))
                                           default default))))
       (when org-varde--background
         (setq org-varde--answers (plist-put org-varde--answers :project project)))
       (append args (list "--project" project))))
    ((and "worktree" (guard (not (member "--worktree" args))))
     (let ((worktree (or (plist-get org-varde--answers :worktree)
                         (if (y-or-n-p (if org-varde--background
                                           "Work in separate git worktrees, for every task in the region? "
                                         "Work in a separate git worktree? "))
                             "always" "never"))))
       (when org-varde--background
         (setq org-varde--answers (plist-put org-varde--answers :worktree worktree)))
       (append args (list "--worktree" worktree))))
    ('nil nil)
    (other (error "Varde asked again about the %s" other))))

(defun org-varde--start (task title want context &optional planned finished)
  "Start, resume or go to TASK's session, titled TITLE, without waiting.
WANT and CONTEXT are as for `org-varde--plan'.  PLANNED, if given, is
called once varde has made its plan, after any question was answered.
FINISHED, if given, is called with nil once the session is started or
gone to, or with why not: a message, or `quit'."
  ;; A task has one terminal here: a second session would have nowhere
  ;; to run but inside the first.
  (when (and want (org-varde--terminal-busy-p (org-varde--terminal task)))
    (user-error "%s is running in its terminal; end that session to start another"
                (org-varde--short task)))
  (let ((finished (or finished #'ignore)))
    (org-varde--plan
     task want context title
     (lambda (plan)
       (funcall finished
                (condition-case e
                    (progn
                      (when planned (funcall planned))
                      (org-varde--follow-plan task title plan)
                      (org-varde-refresh)
                      nil)
                  (error (message "org-varde: %s" (error-message-string e))
                         (error-message-string e)))))
     (lambda (why)
       (when (eq why 'quit) (message "Quit"))
       (funcall finished why)))))

(defun org-varde--follow-plan (task title plan)
  "Do what varde's PLAN says for TASK, titled TITLE."
  (pcase (plist-get plan :action)
    ("focus"
     (let ((ours (org-varde--terminal task))
           (typed (org-varde--tty-terminal (plist-get plan :tty))))
       (cond (org-varde--background
              (message "%s is already running" (org-varde--short task)))
             (ours
              (org-varde--workspace-open task title)
              (pop-to-buffer-same-window ours))
             (typed (org-varde--go-to-buffer typed))
             (t (message "org-varde: %s is running in a terminal outside Emacs; C-u C-u forks it"
                         (org-varde--short task))))))
    ("run" (org-varde--open-terminal task title (plist-get plan :cwd) (plist-get plan :command)))
    (other (error "Unexpected plan from varde: %S" other))))

(defun org-varde--want (arg)
  "The start flag for prefix ARG."
  (pcase arg ('(4) "--fresh") ('(16) "--fork")))

;;;###autoload
(defun org-varde-start (&optional arg finished)
  "Start the heading at point as a varde task, or pick it back up.
It resumes where it ran, in the background, or goes to its terminal if
it is running.  With a \\[universal-argument] prefix ARG, start a fresh
session; with two, fork the last one.  FINISHED is as for
`org-varde--start'."
  (interactive "P")
  (unless (org-varde--heading-p)
    (user-error "Not on an org heading; the board (`org-varde-board') starts any task"))
  ;; An archived heading would close its task again.
  (when (org-varde--archived-p)
    (user-error "This heading is archived; unarchive it to pick its task back up"))
  ;; Else marking the heading DONE would not close the task.
  (unless org-varde-mode (org-varde-mode 1))
  (let* ((closed (cond ((org-entry-is-done-p) "done")
                       ((member (org-get-todo-state) org-varde-park-keywords) "park")))
         (linked (org-entry-get nil org-varde-property))
         (task (org-varde--ensure-task))
         ;; Also for a copied heading given a task of its own.
         (made (not (equal task linked)))
         (title (org-varde--title))
         (context (save-excursion (org-varde--context))))
    ;; Starting reopens the task: the heading follows, once varde has
    ;; made its plan, so a question left unanswered changes neither.
    (let ((heading (point-marker)))
      (org-varde--start task title (org-varde--want arg) context
                        (lambda ()
                          (org-with-point-at heading
                            (when closed
                              (org-varde--set-keyword (org-varde--open-keyword)))))
                        (lambda (why)
                          (set-marker heading nil)
                          ;; A task made for a closed heading just now is as closed as it.
                          (when (and (eq why 'quit) made closed) (org-varde--varde closed task))
                          (when finished (funcall finished why)))))))

(defun org-varde--archived-p ()
  "Whether the heading at point is archived.
That is in an archive file, or tagged ARCHIVE on itself or an ancestor."
  (or (member "ARCHIVE" (org-get-tags))
      (org-varde--archive-file-p (org-varde--file))))

(defun org-varde-start-task (task title &optional arg finished)
  "Start TASK, titled TITLE, from outside org, with prefix ARG.
A task linked to a heading is started from it, so its text is current.
FINISHED is as for `org-varde--start'."
  (or (org-varde--at-heading task (lambda () (org-varde-start arg finished) t))
      (org-varde--start task title (org-varde--want arg) nil nil finished)))

;;;###autoload
(defun org-varde-new-session (&optional directory)
  "Start a session of your own in DIRECTORY, else `default-directory'.
varde puts it in the Inbox once you ask it something."
  (interactive)
  (let* ((directory (file-name-as-directory (expand-file-name (or directory default-directory))))
         (buffer (funcall org-varde-terminal-function directory
                          (format "*%s %s*" org-varde-agent-command
                                  (file-name-nondirectory (directory-file-name directory))))))
    (with-current-buffer buffer (funcall org-varde-send-function org-varde-agent-command))
    (pop-to-buffer-same-window buffer)))

;;; --- Finishing ---------------------------------------------------------------

(defun org-varde--ask-stop (task title close &optional undo)
  "Whether TASK's session, titled TITLE, may be stopped.
Yes when it is not busy, else you are asked: CLOSE says the task closes
too.  A quit counts as no: it calls UNDO, if given, and then quits."
  (or (not (org-varde--busy-p task))
      (condition-case nil
          (y-or-n-p (format (if close "%s is busy; stop it and close the task? "
                              "%s is busy; stop its session? ")
                            title))
        (quit (when undo (funcall undo))
              (signal 'quit nil)))))

(defun org-varde--finish (task title &optional undo asked)
  "Close TASK, titled TITLE, in varde, and put its windows away.
varde stops the session; one that is busy is closed only if you agree,
unless ASKED says you already have.  UNDO, if given, is called when you
decline or varde fails.  The worktree is removed once its work has
landed, now or later; nothing is asked.  A task varde already has done
or dropped is only put away: any session kept running was settled when
it was closed."
  (cond ((member (plist-get (org-varde--details task) :state) '("done" "dropped"))
         (org-varde--put-away-idle task)
         (org-varde-refresh))
        ((not (or asked (org-varde--ask-stop task title t undo)))
         (when undo (funcall undo)))
        ;; Removing a worktree can take seconds: Emacs need not wait.
        (t (org-varde--varde-async
            (list "done" task "--json")
            (lambda (out)
              (let ((wt (plist-get (org-varde--json out) :worktree)))
                (when (equal (plist-get wt :action) "kept")
                  (message "org-varde: %s kept until its work lands" (plist-get wt :branch))))
              (org-varde--put-away task)
              (org-varde-refresh))
            undo))))

(defun org-varde-throw-away-task (task title)
  "Throw TASK, titled TITLE, away once you confirm.
Work in its worktree is kept under refs/varde/discarded/, so it can be
recovered.  A heading it has is marked DONE."
  (let ((work (equal (plist-get (plist-get (org-varde--details task) :worktree) :state) "active")))
    (when (if work
              (yes-or-no-p (format "Throw away %s and its work (kept under refs/varde/discarded/)? "
                                   title))
            (y-or-n-p (format "Throw away %s? " title)))
      (let ((saved (plist-get (plist-get (org-varde--varde-json "drop" task "--json") :worktree) :saved)))
        (message "org-varde: thrown away%s"
                 (if (stringp saved) (format "; the work is kept at %s" saved) "")))
      ;; Marks its heading DONE, which only puts the task away now.
      (org-varde-done-task task title))))

(defvar org-varde-board-data)

(defun org-varde--row (task)
  "TASK's row on the board as it was last fetched, if it has one."
  (seq-some (lambda (g) (seq-find (lambda (r) (equal (plist-get r :task) task)) (plist-get g :rows)))
            (plist-get org-varde-board-data :groups)))

(defconst org-varde--busy '("working" "delegated" "blocked")
  "Statuses of a session that is never stopped without asking.
A session whose sub-agents are still working counts as working.")

(defun org-varde--busy-p (task)
  "Whether TASK's session is busy, as the board last saw it."
  (member (plist-get (org-varde--row task) :status) org-varde--busy))

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

(defun org-varde--park (task title &optional undo answer)
  "Park TASK, titled TITLE, in varde and put its windows away.
Its worktree stays.  varde stops the session; one that is busy is
stopped only if you agree, unless ANSWER, `stop' or `keep', already
says.  UNDO, if given, is called when varde fails or you quit."
  (if (equal (plist-get (org-varde--details task) :state) "parked")
      (org-varde--put-away-idle task)
    (let ((keep (if answer
                    (eq answer 'keep)
                  (not (org-varde--ask-stop task title nil undo)))))
      (org-varde--varde-async (append (list "park" task "--json") (and keep '("--keep-running")))
                              (lambda (_)
                                (unless keep (org-varde--put-away task))
                                (org-varde-refresh))
                              undo))))

(defcustom org-varde-settle-delay 1.0
  "Seconds a linked heading's todo keyword must rest before varde follows it.
Cycling through keywords, or a repeater that sets DONE and then TODO
again, then acts once, on where the heading ends up."
  :type 'number)

(defvar org-varde--pending nil
  "Keyword changes waiting to settle: (TASK TIMER MARKER TITLE FROM).")

(defun org-varde--kind (state)
  "What todo STATE means for a task: `done', `parked' or `open'."
  (cond ((member state org-done-keywords) 'done)
        ((member state org-varde-park-keywords) 'parked)
        (t 'open)))

(defvar org-varde--quiet nil
  "Non-nil while org-varde sets a keyword that varde must not follow.
A variable, not the hook removed: a buffer can hold the hook locally.")

(defun org-varde--on-todo-change ()
  "Have varde follow a linked heading's todo keyword once it settles.
A done keyword closes the task, a park keyword parks it, and leaving
either for a not-done keyword reopens it, so a DONE set by mistake is
undone by setting it back.  See `org-varde-settle-delay'."
  (let ((task (and (not org-varde--quiet) (org-entry-get nil org-varde-property))))
    (when task
      (let ((p (assoc task org-varde--pending)))
        (if p
            (cancel-timer (nth 1 p))
          (setq p (list task nil (save-excursion (org-back-to-heading t) (point-marker))
                        (org-varde--title) org-last-state))
          (push p org-varde--pending))
        (setcar (cdr p) (run-at-time org-varde-settle-delay nil #'org-varde--settle task))))))

(defun org-varde--settle (task)
  "Have varde follow TASK's heading from where it was before it changed."
  (let ((p (assoc task org-varde--pending)))
    (setq org-varde--pending (delq p org-varde--pending))
    (pcase-let ((`(,_ ,_ ,heading ,title ,from) p))
      (when (buffer-live-p (marker-buffer heading))
        (with-current-buffer (marker-buffer heading)
          (org-with-point-at heading
            (when (equal (org-entry-get nil org-varde-property) task)
              (let ((undo (lambda () (org-with-point-at heading (org-varde--set-keyword from))))
                    (to (org-varde--kind (org-get-todo-state))))
                (unless (eq to (org-varde--kind from))
                  (let ((all (org-varde--headings task)))
                    (if (cdr all)
                        ;; Either may be the one meant: a copy must not close the original's task.
                        (progn
                          (funcall undo)
                          (message "org-varde: %s is linked from %s; remove VARDE_TASK from the copy"
                                   (org-varde--short task) (mapconcat #'org-varde--where all " and ")))
                      (condition-case err
                          (pcase to
                            ('done (org-varde--finish task title undo))
                            ('parked (org-varde--park task title undo))
                            (_ (org-varde--varde-async (list "reopen" task)
                                                       (lambda (_) (org-varde-refresh)) undo)))
                        (org-varde-varde-error
                         (message "varde: %s" (cadr err))
                         (funcall undo))))))))))))))

(defun org-varde--set-keyword (state)
  "Set the heading at point to todo STATE without varde following it.
Its time is logged as org is set to, but no note is asked for: org-varde
sets it away from the heading.  The file is saved when this is its only
change."
  ;; `org-inhibit-logging' covers notes on state changes only: a DONE
  ;; note comes from `org-log-done'.
  (let ((org-varde--quiet t)
        (org-inhibit-logging 'note)
        (org-log-done (and org-log-done 'time)))
    (org-varde--saving-if-clean
      (org-todo (or state 'none))
      (org-varde--log-now))))

(defun org-varde--log-now ()
  "Write now, as a time stamp, the log line org left for after the command.
Org writes it from `post-command-hook', after the save, and asks there
for a note when a repeat or a keyword wants one."
  (when (and org-log-setup (eq (marker-buffer org-log-note-marker) (current-buffer)))
    (remove-hook 'post-command-hook #'org-add-log-note)
    (setq org-log-setup nil
          org-log-note-window-configuration (current-window-configuration))
    (move-marker org-log-note-return-to (point))
    ;; It stores what the note buffer holds, here nothing: the time alone.
    (let ((inhibit-message t))
      (with-temp-buffer (org-store-log-note)))))

(defun org-varde--open-keyword ()
  "The first not-done keyword that does not park."
  (car (seq-difference org-not-done-keywords org-varde-park-keywords)))

;;;###autoload
(defun org-varde-done ()
  "Mark the heading at point DONE, closing its varde task."
  (interactive)
  (unless (org-varde--heading-p) (user-error "Not on an org heading"))
  (org-todo 'done))

(defun org-varde--set-back (from)
  "A function that sets the heading at point back to keyword FROM."
  (let ((heading (point-marker)))
    (lambda () (org-with-point-at heading (org-varde--set-keyword from)))))

(defun org-varde-park-task (task title)
  "Park TASK, titled TITLE: through its heading, if it has one.
That needs the heading's file to know the first of
`org-varde-park-keywords'.  The keyword is saved as a DONE is.  A busy
session is asked about now, as you press the key."
  (let ((answer (if (and (org-varde--busy-p task)
                         (not (equal (plist-get (org-varde--details task) :state) "parked"))
                         (not (org-varde--ask-stop task title nil)))
                    'keep 'stop)))
    (or (org-varde--at-heading
         task (lambda ()
                ;; An archived heading is left as it is.
                (unless (org-varde--archived-p)
                  (let ((keyword (car org-varde-park-keywords))
                        (from (org-get-todo-state)))
                    (if (or (member from org-varde-park-keywords)
                            (not (member keyword org-todo-keywords-1)))
                        (org-varde--park task title nil answer)
                      (let ((undo (org-varde--set-back from)))
                        (org-varde--set-keyword keyword)
                        (org-varde--park task title undo answer))))
                  t)))
        (org-varde--park task title nil answer))))

(defun org-varde-done-task (task title)
  "Close TASK, titled TITLE: through its heading when it has one.
A busy session is asked about now, as you press the key, and declining
changes nothing.  The DONE is saved when it is the buffer's only change,
as the buffer may be out of sight; unsaved edits of your own are never
saved for you."
  (when (or (not (org-varde--busy-p task))
            ;; Thrown away, or closed elsewhere: nothing is stopped.
            (member (plist-get (org-varde--details task) :state) '("done" "dropped"))
            (org-varde--ask-stop task title t))
    (or (org-varde--at-heading
         task (lambda ()
                ;; An archived heading is left as it is.
                (unless (org-varde--archived-p)
                  (if (org-entry-is-done-p)
                      (org-varde--finish task title nil t)
                    (let ((undo (org-varde--set-back (org-get-todo-state))))
                      (org-varde--set-keyword 'done)
                      ;; A repeater has set it back to TODO already.
                      (org-varde--finish task title (and (org-entry-is-done-p) undo) t)))
                  t)))
        (org-varde--finish task title nil t))))

(defvar org-varde--said nil
  "What reconciling has reported since the board was last opened.")

(defun org-varde--say-once (format &rest args)
  "Show the message FORMAT makes of ARGS, unless it has been shown.
Reconciling runs on every refresh, and a heading it cannot write fails
the same way each time."
  (let ((text (apply #'format-message format args)))
    (unless (member text org-varde--said)
      (push text org-varde--said)
      (message "%s" text))))

(defun org-varde-reconcile ()
  "Bring every linked heading and its task into step, from `varde list --all'.
See `org-varde--reconcile'."
  (setq org-varde--said nil)
  (org-varde--varde-async '("list" "--json" "--all")
                          (lambda (out)
                            (let ((data (org-varde--json out)))
                              (when (org-varde--readable data)
                                (org-varde--reconcile (plist-get data :groups)))))))

(defun org-varde--catch-up ()
  "Revert each org buffer whose file changed on disk, say by a sync.
Reconciling runs in the background, where nothing may ask, so a buffer
without edits of its own is reverted without asking.  Return the files
of those with edits: left for later, as reverting would lose them."
  (let (stale)
    (dolist (b (buffer-list) stale)
      (with-current-buffer b
        (when (and buffer-file-name (derived-mode-p 'org-mode)
                   (file-exists-p buffer-file-name)
                   (not (verify-visited-file-modtime b)))
          (if (buffer-modified-p)
              (push (org-varde--file) stale)
            (condition-case err
                (revert-buffer t t t)
              (error (org-varde--say-once "org-varde: could not reread %s: %s"
                                          buffer-file-name (error-message-string err))
                     (push (org-varde--file) stale)))))))))

(defun org-varde--reconcile (groups &optional tasks)
  "Bring linked headings and the tasks in board GROUPS into step.
With TASKS, only those tasks.  When a heading and its task disagree on
done, parked or open, the more recent change wins:

- The heading's change is when it was closed (CLOSED) or archived
  (ARCHIVE_TIME), else when its file was saved.  A heading left open
  has no such time, so its task's change always wins.
- The task's change is the row's `changed': when its state last
  changed in varde.

A heading that wins closes, parks or reopens its task, unless its
session is busy: that is never stopped without asking.  Dated only by
its file, it also leaves a waiting session alone, and its task out of
step.  A task that
wins sets its heading's keyword, saved when that is the buffer's only
change; archived and repeating headings are left alone.

A buffer whose file changed on disk is reverted first, unless it has
edits of its own; then its headings are left for later.  So is a task
linked from more than one heading, as a copied heading is.  Return the
tasks left out of step that way, or because their heading could not be
written; each such problem is said once."
  (let ((rows (make-hash-table :test #'equal))
        (stale (org-varde--catch-up))
        (linked (org-varde--linked))
        (headings (make-hash-table :test #'equal))
        follow left)
    (dolist (g groups)
      (dolist (r (plist-get g :rows))
        (puthash (plist-get r :task) (cons (plist-get g :key) r) rows)))
    (dolist (l linked)
      (puthash (car l) (1+ (gethash (car l) headings 0)) headings))
    (dolist (l linked)
      (let ((hit (gethash (car l) rows)))
        (when (and hit (or (null tasks) (member (car l) tasks))
                   ;; A copied heading may disagree with the one it copies.
                   (or (= (gethash (car l) headings) 1)
                       (progn (org-varde--say-once "org-varde: %s is linked from more than one heading; \
remove VARDE_TASK from the copy" (org-varde--short (car l)))
                              (unless (member (car l) left) (push (car l) left))
                              nil))
                   ;; A keyword change still settling is the user's latest word.
                   (not (assoc (car l) org-varde--pending)))
          (let* ((task (car l))
                 (row (cdr hit))
                 (varde (pcase (or (plist-get row :state) (car hit))
                          ((or "done" "dropped") 'done) ("parked" 'parked) (_ 'open)))
                 (org (cond ((or (nth 3 l) (nth 4 l)) 'done) ((nth 5 l) 'parked) (t 'open))))
            (cond ((eq org varde))
                  ((member (nth 1 l) stale)
                   (org-varde--say-once "org-varde: %s changed on disk and has unsaved edits; \
save or revert it to bring its headings into step" (nth 1 l))
                   (push task left))
                  ((and (not (eq org 'open))
                        (> (or (nth 6 l) (org-varde--file-time (nth 1 l))) (or (plist-get row :changed) (plist-get row :last) 0)))
                   (cond ((member (plist-get row :status) org-varde--busy))
                         ;; A file's save may be an unrelated edit: by that
                         ;; alone, a waiting session is not closed either.
                         ((and (not (nth 6 l)) (equal (plist-get row :status) "waiting"))
                          (org-varde--say-once "org-varde: the heading of %s is %s, with no CLOSED time, \
and its session is waiting; %s it on the board, or set the heading back"
                                               (org-varde--short task) (if (eq org 'done) "done" "parked")
                                               (if (eq org 'done) "close" "park"))
                          (push task left))
                         (t (org-varde--varde-async (list (if (eq org 'done) "done" "park") task)
                                                    (lambda (_) (org-varde-refresh))))))
                  ((not (nth 4 l))
                   (push (cons l varde) follow)))))))
    ;; Bottom up: a keyword sets or clears a CLOSED line, which moves
    ;; every heading below it.
    (dolist (f (sort follow (lambda (a b) (> (nth 2 (car a)) (nth 2 (car b))))) left)
      (let ((l (car f)))
        (condition-case err
            (unless (org-varde--follow-task l (cdr f))
              (push (car l) left))
          (error (org-varde--say-once "org-varde: could not set the heading of %s in %s: %s"
                                      (org-varde--short (car l)) (nth 1 l) (error-message-string err))
                 (push (car l) left)))))))

(defun org-varde--file-time (file)
  "When FILE was last saved, in Unix milliseconds; 0 when it is gone.
Unsaved edits do not count: they may be anywhere in the file, and a
heading's own keyword change is settled before it is reconciled."
  (let ((mtime (org-varde--mtime file)))
    (if mtime (truncate (* 1000 (float-time mtime))) 0)))

(defun org-varde--follow-task (linked kind)
  "Set the LINKED heading's keyword to what task KIND means.
KIND is `done', `parked' or `open', and the task changed last.  Return
nil when the heading is no longer where LINKED found it."
  (with-current-buffer (find-file-noselect (nth 1 linked))
    (org-with-wide-buffer
     (goto-char (nth 2 linked))
     (when (and (org-at-heading-p) (equal (org-entry-get nil org-varde-property) (car linked)))
       (unless (org-get-repeat)
         (let ((keyword (pcase kind
                          ('done (car org-done-keywords))
                          ('parked (seq-find (lambda (k) (member k org-todo-keywords-1)) org-varde-park-keywords))
                          (_ (org-varde--open-keyword)))))
           (when keyword (org-varde--set-keyword keyword))))
       t))))

(defvar org-varde--closed nil
  "Tasks the last board had as done, parked or dropped.")

(defun org-varde--follow-closed ()
  "Have headings follow tasks just closed, parked, dropped or reopened.
Say by an agent, or in a terminal."
  (let* ((groups (plist-get org-varde-board-data :groups))
         (tasks (lambda (pick)
                  (seq-mapcat (lambda (g) (and (funcall pick (member (plist-get g :key) '("done" "parked" "dropped")))
                                               (mapcar (lambda (r) (plist-get r :task)) (plist-get g :rows))))
                              groups)))
         (closed (funcall tasks #'identity))
         (new (seq-difference closed org-varde--closed))
         ;; Seen closed, and back on the board open.
         (reopened (seq-intersection org-varde--closed (funcall tasks #'not)))
         (left (and (or new reopened) (org-varde--reconcile groups (append new reopened)))))
    ;; Those left out of step are tried again on the next refresh.
    (setq org-varde--closed (append (seq-difference closed left) (seq-intersection reopened left)))))

;;; --- Memory ----------------------------------------------------------------

(defun org-varde--task-here ()
  "The varde task here: the board row, the buffer's task, or the heading."
  (or (get-text-property (point) 'org-varde-task)
      org-varde--task
      (and (org-varde--heading-p) (org-entry-get nil org-varde-property))
      (user-error "No varde task here")))

;;;###autoload
(defun org-varde-show (&optional task)
  "Show TASK's cairn, runs and worktree.
Its git status, which can take a second, follows when varde has it."
  (interactive)
  (org-varde--show-fill (or task (org-varde--task-here)))
  (pop-to-buffer "*varde show*"))

(defun org-varde--show-fill (task)
  "Fill the show buffer with TASK at once, and its git status when varde has it."
  (org-varde--show-text task (org-varde--varde "show" task "--no-git"))
  (org-varde--varde-async (list "show" task)
                          (lambda (text)
                            (let ((b (get-buffer "*varde show*")))
                              (when (and b (equal (buffer-local-value 'org-varde--task b) task))
                                (org-varde--show-text task text))))))

(defun org-varde--show-text (task text)
  "Put TEXT, what varde shows for TASK, in the show buffer."
  (with-current-buffer (get-buffer-create "*varde show*")
    (let ((inhibit-read-only t)
          (line (line-number-at-pos)))
      (erase-buffer)
      (insert text)
      (goto-char (point-min))
      (forward-line (1- line)))
    (unless (derived-mode-p 'org-varde-show-mode) (org-varde-show-mode))
    (setq-local org-varde--task task)))

(defvar-keymap org-varde-show-mode-map
  :parent special-mode-map
  "e" #'org-varde-edit-cairn)

(defun org-varde--evil-keys (map &rest bindings)
  "With evil, give MAP's own keys, and BINDINGS, to its normal state.
Else evil's operators, such as d, would shadow them."
  (when (fboundp 'evil-define-key*)
    (map-keymap-internal (lambda (key def)
                           (unless (or (eq key ?g) (and (symbolp key) (string-suffix-p "-state" (symbol-name key))))
                             (evil-define-key* 'normal map (vector key) def)))
                         map)
    (apply #'evil-define-key* 'normal map "q" #'quit-window bindings)))

;; evil-snipe would take s and S, which show a task and its project cairn.
(with-eval-after-load 'evil-snipe
  (defvar evil-snipe-disabled-modes)
  (dolist (mode '(org-varde-board-mode org-varde-recall-mode))
    (add-to-list 'evil-snipe-disabled-modes mode)))

(define-derived-mode org-varde-show-mode special-mode "varde show"
  "A task as `varde show' prints it.
\\{org-varde-show-mode-map}"
  (setq header-line-format " e edit cairn  q quit")
  (setq-local revert-buffer-function (lambda (&rest _) (org-varde--show-fill org-varde--task)))
  (org-varde--evil-keys org-varde-show-mode-map "gr" #'revert-buffer))

(defvar-keymap org-varde-cairn-mode-map
  "C-c C-c" #'org-varde-cairn-save
  "C-c C-k" #'kill-current-buffer)

(define-derived-mode org-varde-cairn-mode text-mode "Cairn"
  "Edit a task's or a project's cairn.  \\[org-varde-cairn-save] saves it.
Lines you add or change are marked yours, and later updates keep them.")

(defvar-local org-varde--project-dir nil
  "The project directory whose cairn this buffer shows.")

(defvar-local org-varde--editing nil
  "The cairn being edited, as `varde edit' is told it.
A task, or \"--project\" and its directory.")

;;;###autoload
(defun org-varde-edit-cairn (&optional task)
  "Edit TASK's cairn.
Its context is above it, and is saved too, unless a heading owns it."
  (interactive)
  (let ((task (or task (org-varde--task-here))))
    (with-current-buffer (org-varde--edit (format "*cairn %s*" (org-varde--short task)) (list task))
      (setq-local org-varde--task task))))

;;;###autoload
(defun org-varde-edit-project-cairn (&optional directory)
  "Edit the cairn of the project DIRECTORY is in."
  (interactive)
  (let ((dir (or directory org-varde--project-dir (org-varde--project-here))))
    (org-varde--edit (format "*project cairn %s*" (file-name-nondirectory (directory-file-name dir)))
                     (list "--project" dir))))

(defun org-varde--edit (name which)
  "Edit in buffer NAME the cairn that WHICH names to `varde edit'.
Return the buffer."
  (let ((buffer (get-buffer-create name)))
    ;; An edit in progress is kept.
    (unless (buffer-modified-p buffer)
      (with-current-buffer buffer
        (erase-buffer)
        ;; varde's own text: what it compares the edit against.
        (insert (apply #'org-varde--varde "edit" (append which '("--print"))))
        (set-buffer-modified-p nil)
        (goto-char (point-min))
        (org-varde-cairn-mode)
        (setq-local org-varde--editing which)
        (setq header-line-format "C-c C-c save, C-c C-k cancel")))
    (pop-to-buffer buffer)))

(defun org-varde-cairn-save ()
  "Save the cairn being edited."
  (interactive)
  (message "%s" (string-trim (org-varde--call (buffer-string)
                                              (append '("edit") org-varde--editing '("--stdin")))))
  (kill-current-buffer)
  ;; A cairn on view shows what was just saved.
  (dolist (name '("*varde show*" "*varde project*"))
    (when (get-buffer name)
      (with-current-buffer name (revert-buffer)))))

(defun org-varde--project-here ()
  "The project of the task here, or one asked for."
  (expand-file-name
   (or (plist-get (get-text-property (point) 'org-varde-row) :project)
       (when-let* ((task (ignore-errors (org-varde--task-here))))
         (plist-get (org-varde--varde-json "show" task "--no-git" "--json") :project))
       (read-directory-name "Project: "))))

;;;###autoload
(defun org-varde-show-project (&optional directory)
  "Show the cairn of the project DIRECTORY is in.
Interactively, that of the task here."
  (interactive)
  (org-varde--project-fill (or directory (org-varde--project-here)))
  (pop-to-buffer "*varde project*"))

(defun org-varde--project-fill (dir)
  "Fill the project buffer with the cairn of the project DIR is in."
  (let ((text (org-varde--varde "show" "--project" dir)))
    (with-current-buffer (get-buffer-create "*varde project*")
      (let ((inhibit-read-only t)
            (line (line-number-at-pos)))
        (erase-buffer)
        (insert text)
        (goto-char (point-min))
        (forward-line (1- line)))
      (unless (derived-mode-p 'org-varde-project-mode) (org-varde-project-mode))
      (setq-local org-varde--project-dir dir))))

(defvar-keymap org-varde-project-mode-map
  :parent special-mode-map
  "e" #'org-varde-edit-project-cairn)

(define-derived-mode org-varde-project-mode special-mode "varde project"
  "A project's cairn as `varde show --project' prints it.
\\{org-varde-project-mode-map}"
  (setq header-line-format " e edit  q quit")
  (setq-local revert-buffer-function (lambda (&rest _) (org-varde--project-fill org-varde--project-dir)))
  (org-varde--evil-keys org-varde-project-mode-map "gr" #'revert-buffer))

;;;###autoload
(defun org-varde-recall (words)
  "Search what earlier tasks learned for WORDS, in every project."
  (interactive "sRecall: ")
  (let ((text (apply #'org-varde--varde "recall" "--all" (split-string words))))
    (with-current-buffer (get-buffer-create "*varde recall*")
      (let ((inhibit-read-only t))
        (erase-buffer)
        (insert text)
        ;; A task's hit starts with the six characters of its id, and
        ;; its lines below are indented; a project's starts "project".
        (goto-char (point-min))
        (let (task)
          (while (not (eobp))
            (cond ((looking-at "\\([0-9a-z]\\{6\\}\\)  ") (setq task (match-string 1)))
                  ((not (looking-at "[ \t]")) (setq task nil)))
            (when task
              (put-text-property (point) (line-beginning-position 2) 'org-varde-task task))
            (forward-line 1)))
        (goto-char (point-min)))
      (org-varde-recall-mode)
      (pop-to-buffer (current-buffer)))))

(defun org-varde-recall-visit ()
  "Go to the org heading of the task of the hit at point."
  (interactive)
  (let* ((short (org-varde--task-here))
         (task (seq-find (lambda (id) (equal (org-varde--short id) short))
                         (mapcar #'car (org-varde--linked)))))
    (unless task (user-error "This task has no org heading"))
    (org-varde--visit task)))

(defun org-varde--visit (task)
  "Go to TASK's org heading."
  (let ((where (or (org-varde--find-heading task) (user-error "This task has no org heading"))))
    (find-file (car where))
    (unless (<= (point-min) (cdr where) (point-max)) (widen))
    (goto-char (cdr where))
    (org-fold-show-context)))

(defvar-keymap org-varde-recall-mode-map
  :parent special-mode-map
  "RET" #'org-varde-show
  "s" #'org-varde-show
  "o" #'org-varde-recall-visit)

(define-derived-mode org-varde-recall-mode special-mode "varde recall"
  "What `varde recall' found.  On a task's hit, RET shows it.
\\{org-varde-recall-mode-map}"
  (setq header-line-format " RET show  o heading  q quit")
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
(add-hook 'org-varde-after-refresh-hook #'org-varde--follow-closed)
;; A turn that ends while you watch it is seen as it ends.
(add-hook 'org-varde-after-refresh-hook #'org-varde--mark-seen)

(defvar org-varde--timer nil)
(defvar org-varde--listing nil "The running `varde list', if any.")
(defvar org-varde--again nil "Non-nil when a refresh came in during a listing.")

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
                   (data (org-varde--json out)))
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
  "Link org headings to varde tasks and keep the board current."
  :global t
  :group 'org-varde
  (when org-varde--watch
    (file-notify-rm-watch org-varde--watch)
    (setq org-varde--watch nil))
  (remove-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change)
  (remove-hook 'window-selection-change-functions #'org-varde--mark-seen)
  (remove-hook 'window-buffer-change-functions #'org-varde--mark-seen)
  (remove-function after-focus-change-function #'org-varde--mark-seen)
  (when org-varde-mode
    (add-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change)
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

(provide 'org-varde)
;;; org-varde.el ends here
