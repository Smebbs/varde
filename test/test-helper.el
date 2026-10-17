;;; test-helper.el --- Batch test setup for org-varde -*- lexical-binding: t; -*-

;;; Commentary:

;; Every test runs against a fake `varde': a script that records how it
;; was called and plays back canned output.  No test touches the real
;; varde store, the real org tree, or spawns an agent.  Terminals and
;; workspaces are replaced with recorders.

;;; Code:

(require 'cl-lib)
(require 'org)

(defvar org-varde--timer)
(defvar org-varde--listing)
(defvar org-varde--agent)
(defvar org-varde-board-data)
(defvar org-varde--too-new)
(defvar org-varde--due)
(defvar org-varde--last-needed)
(defvar org-varde--outside)
(defvar org-varde-board--closed)
(defvar org-varde--listed)
(defvar org-varde--kept)
(defvar org-varde--drafts)
(defvar org-varde--rows)
(defvar org-varde--all)
(defvar org-varde--all-due)
(defvar org-varde--all-listing)
(defvar org-varde--starting)

;; No test may read the user's org tree or call the real varde, even one
;; that forgets the macro below or leaves a timer behind.
(setq org-directory (make-temp-file "org-varde-org-" t)
      org-agenda-files nil
      org-varde-varde-program "/nonexistent/varde-outside-a-test")
(setenv "VARDE_HOME" org-directory)
(add-hook 'kill-emacs-hook (lambda () (delete-directory (default-value 'org-directory) t)))

(defconst org-varde-test--fake-varde "#!/bin/sh
d=$(dirname \"$0\")
printf '%s\\n' \"$*\" >> \"$d/calls\"
cat > \"$d/stdin.$1\"
if [ -f \"$d/$1.once-err\" ]; then
  [ -f \"$d/$1.out\" ] && cat \"$d/$1.out\"
  cat \"$d/$1.once-err\" >&2; rm \"$d/$1.once-err\"; exit 1
fi
if [ -f \"$d/$1.once-out\" ]; then
  cat \"$d/$1.once-out\"; rm \"$d/$1.once-out\"; exit 0
fi
[ -f \"$d/$1.out\" ] && cat \"$d/$1.out\"
exit 0
"
  "A stand-in for varde: one line per call in `calls', stdin kept per
subcommand, output from `<subcommand>.out', and a one-time failure
from `<subcommand>.once-err', and a one-time answer from `<subcommand>.once-out'.")

(defvar org-varde-test-dir nil "The fake varde's directory, inside a test.")
(defvar org-varde-test-terminals nil "(DIRECTORY NAME) of each terminal opened.")
(defvar org-varde-test-sent nil "Each command typed into a terminal.")

(defmacro org-varde-test-with-varde (&rest body)
  "Run BODY against a fake varde, with terminals recorded, not opened."
  (declare (indent 0))
  `(let* ((org-varde-test-dir (make-temp-file "org-varde-test-" t))
          (fake (expand-file-name "varde" org-varde-test-dir))
          (org-varde-varde-program fake)
          (org-varde-workspaces nil)
          (org-varde-use-worktree 'ask)
          (org-varde-test-terminals nil)
          (org-varde-test-sent nil)
          (org-varde-terminal-function
           (lambda (dir name)
             (push (list dir name) org-varde-test-terminals)
             (get-buffer-create name)))
          (org-varde-send-function (lambda (cmd) (push cmd org-varde-test-sent)))
          (org-agenda-files nil)
          (org-directory org-varde-test-dir)
          (org-varde--agent nil)
          (org-varde-board-data nil)
          (org-varde--rows nil)
          (org-varde--all nil)
          (org-varde--all-due nil)
          (org-varde--all-listing nil)
          (org-varde--starting nil)
          (org-varde--too-new nil)
          (org-varde--due nil)
          (org-varde--last-needed nil)
          (org-varde--outside nil)
          (org-varde-board--closed nil)
          (org-varde--listed nil)
          (org-varde--kept nil)
          (org-varde--drafts nil)
          (process-environment (cons (concat "VARDE_HOME=" org-varde-test-dir)
                                     process-environment)))
     (with-temp-file fake (insert org-varde-test--fake-varde))
     (set-file-modes fake #o755)
     (unwind-protect
         ;; Under kqueue (macOS) a watch on the store the fake varde
         ;; writes into stops batch Emacs running process sentinels.
         (cl-letf (((symbol-function 'file-notify-add-watch) #'ignore))
           ,@body)
       ;; Nothing the test started may outlive it.
       (org-varde-test-wait)
       ;; `org-varde-start' turns the mode on, watching this test's store:
       ;; a watch on a deleted directory stops Emacs 29 and 30 running
       ;; the next test's sentinels.
       (when org-varde-mode (org-varde-mode -1))
       (when (timerp org-varde--timer) (cancel-timer org-varde--timer))
       (when (timerp org-varde--all-due) (cancel-timer org-varde--all-due))
       (dolist (b (buffer-list))
         (when (string-prefix-p "*varde" (buffer-name b))
           ;; A draft asks before it goes, from a hook of its own.
           (with-current-buffer b (set-buffer-modified-p nil))
           (let ((kill-buffer-query-functions nil)) (kill-buffer b))))
       (delete-directory org-varde-test-dir t))))

(defun org-varde-test-reply (subcommand text)
  "Make the fake varde answer SUBCOMMAND with TEXT."
  (with-temp-file (expand-file-name (concat subcommand ".out") org-varde-test-dir)
    (insert text)))

(defun org-varde-test-reply-once (subcommand text)
  "Make the fake varde answer only its next SUBCOMMAND with TEXT."
  (with-temp-file (expand-file-name (concat subcommand ".once-out") org-varde-test-dir)
    (insert text)))

(defun org-varde-test-fail-once (subcommand message)
  "Make the fake varde's next SUBCOMMAND fail with MESSAGE."
  (with-temp-file (expand-file-name (concat subcommand ".once-err") org-varde-test-dir)
    (insert message)))

(defun org-varde-test-wait ()
  "Wait for every varde call still running, and its callback."
  ;; A call is over once its sentinel has run, which kills its output
  ;; buffer: a process can be dead well before that.
  (while (seq-some (lambda (p) (and (stringp (car (process-command p)))
                                    (string-prefix-p org-varde-test-dir (car (process-command p)))
                                    (or (process-live-p p) (buffer-live-p (process-buffer p)))))
                   (process-list))
    (accept-process-output nil 0.02)))

(defvar org-varde-test-offered nil
  "The directories the project picker last offered, best first, and
`another directory…' last.")

(defun org-varde-test-pick (&optional answer)
  "A `completing-read' that answers the project picker: with ANSWER, a
directory typed or `org-varde--other-directory', else with RET, its
first choice.  What it offers goes in `org-varde-test-offered'."
  (lambda (_prompt collection &rest _)
    (let ((choices (all-completions "" collection)))
      (setq org-varde-test-offered
            (mapcar (lambda (c) (or (cadr (split-string c "  +" t)) c)) choices))
      (or answer (car choices)))))

(defun org-varde-test-calls ()
  "Every call the fake varde received, oldest first, once all are done."
  (org-varde-test-wait)
  (let ((f (expand-file-name "calls" org-varde-test-dir)))
    (and (file-exists-p f)
         (split-string (with-temp-buffer (insert-file-contents f) (buffer-string)) "\n" t))))

(defun org-varde-test-stdin (subcommand)
  "What the last SUBCOMMAND call read on stdin."
  (with-temp-buffer
    (insert-file-contents (expand-file-name (concat "stdin." subcommand) org-varde-test-dir))
    (buffer-string)))

(defmacro org-varde-test-with-org (text &rest body)
  "Visit TEXT as an org file, named FILE, then run BODY."
  (declare (indent 1))
  `(let ((file (make-temp-file "org-varde-test-" nil ".org" ,text)))
     (unwind-protect
         (with-current-buffer (find-file-noselect file)
           (goto-char (point-min))
           ,@body)
       (let ((b (find-buffer-visiting file)))
         (when b (with-current-buffer b (set-buffer-modified-p nil)) (kill-buffer b)))
       (delete-file file))))

(provide 'test-helper)
;;; test-helper.el ends here
