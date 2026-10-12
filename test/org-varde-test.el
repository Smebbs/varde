;;; org-varde-test.el --- Tests for the varde client -*- lexical-binding: t; -*-

;;; Code:

(require 'ert)
(require 'test-helper)
(require 'org-varde)

(defconst org-varde-test--id "01K5ZZZZZZZZZZZZZZZZXYZ123")

(defconst org-varde-test--proj
  (let ((dir (expand-file-name "proj" (default-value 'org-directory))))
    (make-directory dir t)
    dir)
  "A project directory that exists, as a heading's VARDE_PROJECT must.")

(defconst org-varde-test--org (concat "#+PROPERTY: VARDE_PROJECT " org-varde-test--proj "
* Area
Everything about logging in.
** TODO Fix the login bug
:PROPERTIES:
:CUSTOM: x
:END:
It fails on CI only.
*** A child note
"))

(defconst org-varde-test--linked
  (replace-regexp-in-string ":CUSTOM: x" (concat ":VARDE_TASK: " org-varde-test--id) org-varde-test--org)
  "`org-varde-test--org' with its task heading linked.")

(defun org-varde-test--saved (file)
  "What FILE says on disk."
  (with-temp-buffer (insert-file-contents file) (buffer-string)))

(defmacro org-varde-test--with-todo-hook (&rest body)
  "Run BODY with the todo hook on, and deferred calls made at once."
  (declare (indent 0))
  `(progn
     (add-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change)
     (unwind-protect
         (cl-letf (((symbol-function 'run-at-time) (lambda (_ _ f &rest args) (apply f args))))
           ,@body)
       (remove-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change))))

(defun org-varde-test--goto (title)
  (goto-char (point-min))
  (re-search-forward (concat "^\\*+ .*" (regexp-quote title))))

;;; --- Linking ---------------------------------------------------------------

(ert-deftest org-varde-a-heading-becomes-a-task-once ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" (format "{\"v\":1,\"id\":\"%s\",\"short\":\"xyz123\"}" org-varde-test--id))
    (org-varde-test-with-org org-varde-test--org
      (org-varde-test--goto "Fix the login bug")
      (should (equal (org-varde--ensure-task) org-varde-test--id))
      (should (equal (org-entry-get nil "VARDE_TASK") org-varde-test--id))
      (should (equal (car (org-varde-test-calls))
                     (concat "new --project " org-varde-test--proj " --external org --body - --json -- Fix the login bug")))
      ;; The body is the heading's own text: no drawer, no children.
      (should (equal (org-varde-test-stdin "new") "It fails on CI only."))
      ;; Linked: later starts refresh the text, and the project, which
      ;; varde compares while the board has not said where the task is.
      (org-varde--ensure-task)
      (should (equal (cadr (org-varde-test-calls))
                     (format "set %s --title=Fix the login bug --body - --project %s"
                             org-varde-test--id org-varde-test--proj))))))

(defconst org-varde-test--copied
  (concat org-varde-test--linked
          (replace-regexp-in-string "Fix the login bug" "Fix the logout bug"
                                    (substring org-varde-test--linked
                                               (string-search "** TODO" org-varde-test--linked))))
  "`org-varde-test--linked' with its task heading copied, link and all.")

(ert-deftest org-varde-a-copied-heading-is-asked-to-take-a-task-of-its-own ()
  "`org-copy-subtree' keeps the link: starting the copy must not retitle
the task of the heading it copies."
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"NEW111\"}")
    (org-varde-test-with-org org-varde-test--copied
      (org-varde-test--goto "Fix the logout bug")
      (let (asked)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (prompt) (setq asked prompt) t)))
          (should (equal (org-varde--ensure-task) "NEW111")))
        (should (string-match-p "xyz123 is linked from org-varde-test-.*\\.org:4 too" asked)))
      (should (equal (org-entry-get nil "VARDE_TASK") "NEW111"))
      (should (equal (car (org-varde-test-calls))
                     (concat "new --project " org-varde-test--proj " --external org --body - --json -- Fix the logout bug")))
      ;; The original keeps its task, and is started without asking.
      (org-varde-test--goto "Fix the login bug")
      (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked"))))
        (should (equal (org-varde--ensure-task) org-varde-test--id))))))

(ert-deftest org-varde-a-copied-heading-left-linked-changes-nothing ()
  (org-varde-test-with-varde
    (org-varde-test-with-org org-varde-test--copied
      (org-varde-test--goto "Fix the logout bug")
      (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) nil)))
        (should-error (org-varde--ensure-task) :type 'user-error))
      (should (equal (org-entry-get nil "VARDE_TASK") org-varde-test--id))
      (should-not (org-varde-test-calls)))))

(ert-deftest org-varde-a-file-found-by-two-names-is-one-file ()
  "A symlinked `org-directory' and an agenda file by its real name, say:
its headings are not each other's copies."
  (org-varde-test-with-varde
    (org-varde-test-with-org org-varde-test--linked
      (let ((link (expand-file-name "linked.org" org-varde-test-dir)))
        (make-symbolic-link file link)
        (push link org-agenda-files)
        (should (equal (car (org-varde--find-heading org-varde-test--id)) (file-truename file)))
        (org-varde-test--goto "Fix the login bug")
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked"))))
          (should (equal (org-varde--ensure-task) org-varde-test--id)))))))

(ert-deftest org-varde-a-heading-in-an-indirect-buffer-is-its-own ()
  "As `org-tree-to-indirect-buffer' shows it: no file of its own."
  (org-varde-test-with-varde
    (org-varde-test-with-org org-varde-test--linked
      (let ((pos (progn (org-varde-test--goto "Fix the login bug") (point))))
        (with-current-buffer (clone-indirect-buffer nil nil)
          (unwind-protect
              (progn
                (goto-char pos)
                (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked"))))
                  (should (equal (org-varde--ensure-task) org-varde-test--id))))
            (kill-buffer)))))))

(ert-deftest org-varde-a-task-with-two-headings-is-not-acted-on-through-either ()
  "Going to it, closing or parking it, and reconciling: either heading
may be the one meant."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" (format "{\"v\":1,\"groups\":[{\"key\":\"open\",\"label\":\"Open\",\"rows\":[{\"task\":\"%s\"}]}]}" org-varde-test--id))
    (org-varde-test-with-org (replace-regexp-in-string "TODO Fix the logout" "DONE Fix the logout"
                                                       org-varde-test--copied)
      (should (equal (cadr (should-error (org-varde--find-heading org-varde-test--id) :type 'user-error))
                     (format "xyz123 is linked from %s:4 and %s:10; remove VARDE_TASK from the copy"
                             (file-name-nondirectory file) (file-name-nondirectory file))))
      (save-buffer)
      (org-varde-reconcile)
      (org-varde-test-wait)
      (should-not (member (concat "done " org-varde-test--id) (org-varde-test-calls)))
      (should (string-match-p "more than one heading" (car org-varde--said))))))

(ert-deftest org-varde-a-task-without-a-heading-links-to-one ()
  (org-varde-test-with-varde
    (org-varde-test-reply "list" (format "{\"v\":2,\"groups\":[{\"key\":\"done\",\"label\":\"Done\",\"rows\":[{\"task\":\"T0000000000000000000DDD444\",\"short\":\"ddd444\",\"title\":\"finished earlier\"}]},{\"key\":\"inbox\",\"label\":\"Inbox\",\"rows\":[{\"task\":\"%s\",\"short\":\"xyz123\",\"title\":\"why does login fail\"}]}]}" org-varde-test--id))
    (org-varde-test-reply "show" "{\"v\":2,\"project\":null}")
    (org-varde-test-with-org org-varde-test--org
      (org-varde-test--goto "Fix the login bug")
      (let (offered)
        (cl-letf (((symbol-function 'completing-read)
                   (lambda (_prompt choices &rest _) (setq offered (mapcar #'cdr choices)) (caar choices))))
          (org-varde-link))
        ;; Inbox first, and done tasks too.
        (should (equal offered (list org-varde-test--id "T0000000000000000000DDD444"))))
      (should (equal (org-entry-get nil "VARDE_TASK") org-varde-test--id))
      ;; A link that is the buffer's only change is saved.
      (should-not (buffer-modified-p))
      (should (equal (cdr (org-varde-test-calls))
                     (list (format "link %s org" org-varde-test--id)
                           (format "show %s --json --no-git" org-varde-test--id)
                           (format "set %s --title=Fix the login bug --body - --project %s"
                                   org-varde-test--id org-varde-test--proj))))
      ;; A linked heading is not linked again.
      (should-error (org-varde-link) :type 'user-error)
      ;; A task that has a heading is not offered for another.
      (org-varde-test--goto "Area")
      (let (offered)
        (cl-letf (((symbol-function 'completing-read)
                   (lambda (_prompt choices &rest _) (setq offered (mapcar #'cdr choices)) (caar choices))))
          (org-varde-link))
        (should (equal offered '("T0000000000000000000DDD444")))))))

(ert-deftest org-varde-linking-a-heading-elsewhere-keeps-the-task-s-project ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":2,\"project\":\"/tmp/other\"}")
    (org-varde-test-with-org org-varde-test--org
      (org-varde-test--goto "Fix the login bug")
      (org-varde--link-here org-varde-test--id)
      (should (equal (org-entry-get nil "VARDE_PROJECT") "/tmp/other"))
      (should-not (buffer-modified-p))
      (should-not (seq-find (lambda (c) (string-match-p (regexp-quote org-varde-test--proj) c)) (org-varde-test-calls))))))

(ert-deftest org-varde-link-with-every-task-headed-changes-nothing ()
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"inbox\",\"label\":\"Inbox\",\"rows\":[]}]}")
    (org-varde-test-with-org org-varde-test--org
      (org-varde-test--goto "Fix the login bug")
      (should-error (org-varde-link) :type 'user-error)
      (should-not (org-entry-get nil "VARDE_TASK"))
      (should (equal (org-varde-test-calls) '("list --json --all"))))))

(ert-deftest org-varde-context-is-the-outline-and-parent-prose ()
  (org-varde-test-with-org org-varde-test--org
    (org-varde-test--goto "Fix the login bug")
    (let ((c (org-varde--context)))
      (should (string-match-p "org-mode file, /.*\\.org, under: Area\\." c))
      (should (string-match-p "Everything about logging in\\." c)))
    (let ((org-varde-context-words 3))
      (should (string-suffix-p "…" (org-varde--context))))))

(defconst org-varde-test--deferred "01K5ZZZZZZZZZZZZZZZZDEF456")

(defun org-varde-test--reply-deferred (origin)
  (org-varde-test-reply
   "show" (format "{\"v\":1,\"task\":\"%s\",\"title\":\"Retry the token refresh\",\"body\":\"Asked to leave it.\\n* not a heading\",\"origin\":%s}"
                  org-varde-test--deferred (if origin (format "\"%s\"" origin) "null"))))

(ert-deftest org-varde-a-deferred-task-goes-beside-its-origin ()
  (org-varde-test-with-varde
    (org-varde-test--reply-deferred org-varde-test--id)
    (org-varde-test-with-org (concat org-varde-test--linked "** Later sibling\n")
      (save-buffer)
      (let ((at (org-varde--place org-varde-test--deferred)))
        (goto-char at)
        (should (looking-at-p "\\*\\* TODO Retry the token refresh$"))
        (should (equal (org-entry-get nil "VARDE_TASK") org-varde-test--deferred))
        ;; After the origin's subtree, before its next sibling.
        (should (re-search-backward "^\\*\\*\\* A child note" nil t))
        (goto-char at)
        (should (re-search-forward "^ \\* not a heading\n\\*\\* Later sibling" nil t)))
      (should-not (buffer-modified-p))
      (should (equal (org-varde-test-calls)
                     (list (format "show %s --json --no-git" org-varde-test--deferred)
                           (format "link %s org" org-varde-test--deferred)))))))

(ert-deftest org-varde-a-task-without-a-live-origin-goes-where-you-choose ()
  (org-varde-test-with-varde
    (org-varde-test--reply-deferred nil)
    (org-varde-test-with-org org-varde-test--org
      (let ((area (save-excursion (org-varde-test--goto "Area") (line-beginning-position))))
        (cl-letf (((symbol-function 'org-refile-get-location)
                   (lambda (&rest _) (list "Area" file nil area))))
          (goto-char (org-varde--place org-varde-test--deferred))))
      (should (looking-at-p "\\*\\* TODO Retry the token refresh$"))
      ;; At the end of the chosen heading's subtree.
      (should (re-search-backward "^\\*\\*\\* A child note" nil t)))))

(ert-deftest org-varde-filing-a-task-elsewhere-keeps-its-project ()
  (org-varde-test-with-varde
    (org-varde-test-reply
     "show" (format "{\"v\":1,\"task\":\"%s\",\"title\":\"Retry\",\"project\":\"/tmp/other\",\"origin\":null}"
                    org-varde-test--deferred))
    (org-varde-test-with-org org-varde-test--org
      (let ((area (save-excursion (org-varde-test--goto "Area") (line-beginning-position))))
        (cl-letf (((symbol-function 'org-refile-get-location)
                   (lambda (&rest _) (list "Area" file nil area))))
          (goto-char (org-varde--place org-varde-test--deferred))))
      (should (equal (org-entry-get nil "VARDE_PROJECT") "/tmp/other"))
      ;; One already in the heading's project is not named again.
      (org-varde-test-reply
       "show" (format "{\"v\":1,\"task\":\"%s\",\"title\":\"Again\",\"project\":\"%s\",\"origin\":null}"
                      org-varde-test--deferred org-varde-test--proj))
      (let ((area (save-excursion (org-varde-test--goto "Area") (line-beginning-position))))
        (cl-letf (((symbol-function 'org-refile-get-location)
                   (lambda (&rest _) (list "Area" file nil area))))
          (goto-char (org-varde--place org-varde-test--deferred))))
      (should-not (org-entry-get nil "VARDE_PROJECT" nil)))))

;;; --- Starting ----------------------------------------------------------------

(defun org-varde-test--run-plan ()
  (org-varde-test-reply "start" "{\"v\":1,\"action\":\"run\",\"run\":\"R1\",\"cwd\":\"/tmp/proj\",\"command\":\"varde run R1\"}"))

(ert-deftest org-varde-start-opens-a-terminal-and-types-the-run ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test-with-org org-varde-test--linked
      (org-varde-test--goto "Fix the login bug")
      (org-varde-start)
      (org-varde-test-wait)
      (should (equal org-varde-test-terminals '(("/tmp/proj" "*varde xyz123*"))))
      (should (equal org-varde-test-sent '("varde run R1")))
      (should (equal (buffer-local-value 'org-varde--task (get-buffer "*varde xyz123*"))
                     org-varde-test--id))
      (let ((start (seq-find (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls))))
        (should (string-match-p "--context-file " start)))
      ;; C-u is a fresh conversation, C-u C-u a fork.
      (let ((org (find-buffer-visiting file)))
        (with-current-buffer org (org-varde-start '(4)))
        (with-current-buffer org (org-varde-start '(16))))
      (let ((starts (seq-filter (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls))))
        (should (string-match-p " --fresh" (nth 1 starts)))
        (should (string-match-p " --fork" (nth 2 starts)))))))

(ert-deftest org-varde-starting-stays-put-unless-asked-to-go ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (with-temp-buffer
      (let ((here (window-buffer)))
        (org-varde--start org-varde-test--id "t" nil nil)
        (org-varde-test-wait)
        (should (eq (window-buffer) here))
        (should (equal org-varde-test-sent '("varde run R1")))
        (let ((org-varde-start-goes t))
          (org-varde--start org-varde-test--id "t" nil nil)
          (org-varde-test-wait))
        (should (eq (window-buffer) (get-buffer "*varde xyz123*")))))))

;; The board's RET on a region: nothing goes to a terminal, even one
;; already running.
(ert-deftest org-varde-starting-in-the-background-goes-nowhere ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (with-temp-buffer
      (let ((here (window-buffer))
            (org-varde-start-goes t)
            (org-varde--background t))
        (org-varde--start org-varde-test--id "t" nil nil)
        (org-varde-test-wait)
        (should (eq (window-buffer) here))
        (should (equal org-varde-test-sent '("varde run R1")))
        (org-varde-test-reply "start" "{\"v\":1,\"action\":\"focus\",\"run\":\"R1\"}")
        (let* ((buffer (get-buffer "*varde xyz123*"))
               (proc (make-process :name "fake-shell" :buffer buffer :command '("cat") :noquery t)))
          (unwind-protect
              (progn
                (org-varde--start org-varde-test--id "t" nil nil)
                (org-varde-test-wait)
                (should (eq (window-buffer) here)))
            (delete-process proc)))))))

(ert-deftest org-varde-starting-fills-the-task-workspace-and-comes-back ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (let ((org-varde-workspaces t)
          (current "main")
          (spaces '("main"))
          (shown nil))
      (cl-letf (((symbol-function '+workspace-list-names) (lambda () spaces))
                ((symbol-function '+workspace-current-name) (lambda () current))
                ((symbol-function '+workspace-switch)
                 (lambda (name &optional _create)
                   (unless (member name spaces) (push name spaces))
                   (setq current name)))
                ((symbol-function 'pop-to-buffer-same-window)
                 (lambda (buffer) (push (cons current (buffer-name buffer)) shown))))
        (org-varde--start org-varde-test--id "t" nil nil)
        (org-varde-test-wait)
        (should (equal current "main"))
        (should (member "xyz123 t" spaces))
        (should (equal shown '(("xyz123 t" . "*varde xyz123*"))))))))

(ert-deftest org-varde-without-vterm-a-term-is-opened ()
  (skip-unless (not (locate-library "vterm")))
  (org-varde-test-with-varde
    (let (made)
      (cl-letf (((symbol-function 'make-term)
                 (lambda (name program) (setq made (list name program default-directory))
                   (get-buffer-create (concat "*" name "*"))))
                ((symbol-function 'term-char-mode) #'ignore))
        (should (equal (buffer-name (org-varde--open-shell "/tmp/proj" "*varde xyz123*")) "*varde xyz123*")))
      (should (equal (car made) "varde xyz123"))
      (should (equal (nth 2 made) "/tmp/proj/")))))

(ert-deftest org-varde-the-worktree-question-comes-from-varde ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test-reply-once "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"worktree\"}")
    (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t)))
      (org-varde--start org-varde-test--id "t" nil nil)
      (org-varde-test-wait))
    (should (string-suffix-p "--worktree always" (car (last (org-varde-test-calls)))))
    (should (equal org-varde-test-sent '("varde run R1")))))

(ert-deftest org-varde-a-done-heading-left-unanswered-stays-done ()
  "Quitting the worktree question changes neither heading nor task."
  (org-varde-test-with-varde
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"worktree\"}")
    (org-varde-test-with-org (replace-regexp-in-string "TODO Fix" "DONE Fix" org-varde-test--linked)
      (org-varde-test--goto "Fix the login bug")
      (let (why)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (signal 'quit nil))))
          (org-varde-start nil (lambda (w) (setq why w)))
          (org-varde-test-wait))
        (should (eq why 'quit)))
      (should (equal (org-get-todo-state) "DONE"))
      (should-not org-varde-test-sent))))

(ert-deftest org-varde-a-closed-heading-started-and-left-unanswered-keeps-its-task-closed ()
  "A task made for an unlinked DONE heading follows it when the start is quit."
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"NEW111\"}")
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"worktree\"}")
    (org-varde-test-with-org "* DONE Never linked\n:PROPERTIES:\n:VARDE_PROJECT: /tmp/proj\n:END:\n"
      (org-varde-test--goto "Never linked")
      (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (signal 'quit nil))))
        (org-varde-start)
        (org-varde-test-wait))
      (should (equal (org-get-todo-state) "DONE"))
      (should (member "done NEW111" (org-varde-test-calls))))))

(ert-deftest org-varde-a-second-session-is-not-started-in-a-busy-terminal ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test--with-terminal
      (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) t)))
        (should-error (org-varde--start org-varde-test--id "t" "--fresh" nil) :type 'user-error))
      (should-not (seq-find (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls)))
      (should-not org-varde-test-sent))))

(ert-deftest org-varde-the-project-question-comes-from-varde ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test-reply-once "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"project\",\"choices\":[\"/p/repo\"]}")
    (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) "/p/repo/")))
      (org-varde--start org-varde-test--id "t" nil nil)
      (org-varde-test-wait))
    (should (string-suffix-p "--project /p/repo/" (car (last (org-varde-test-calls)))))
    (should (equal org-varde-test-sent '("varde run R1")))))

;; The board's RET on a region: the worktree answer holds for every task
;; in it, and a task's project question offers the one before's answer.
(ert-deftest org-varde-a-region-is-asked-about-worktrees-once ()
  (let (calls prompts defaults (worktree-asked 0))
    (cl-letf (((symbol-function 'org-varde--varde-async)
               (lambda (args callback &rest _)
                 (push args calls)
                 (funcall callback
                          (cond ((not (member "--project" args))
                                 "{\"v\":1,\"action\":\"ask\",\"ask\":\"project\",\"choices\":[\"/p/a\"]}")
                                ((not (member "--worktree" args)) "{\"v\":1,\"action\":\"ask\",\"ask\":\"worktree\"}")
                                (t "{\"v\":1,\"action\":\"run\"}")))))
              ((symbol-function 'read-directory-name)
               (lambda (prompt _dir default)
                 (push prompt prompts)
                 (push default defaults)
                 (if (= (length prompts) 1) "/p/b/" default)))
              ((symbol-function 'y-or-n-p) (lambda (&rest _) (setq worktree-asked (1+ worktree-asked)))))
      (let ((org-varde--background t)
            (org-varde--answers nil))
        ;; As the board does: each starts once the one before is planned.
        (org-varde--plan-answering '("start" "T1" "--json") "One"
                                   (lambda (_) (org-varde--plan-answering '("start" "T2" "--json") "Two"
                                                                          #'ignore #'ignore))
                                   #'ignore))
      (should (= worktree-asked 1))
      (should (equal (nreverse prompts) '("Project directory for One: " "Project directory for Two: ")))
      (should (equal (nreverse defaults) '("/p/a" "/p/b/")))
      (should (equal (car calls) '("start" "T2" "--json" "--project" "/p/b/" "--worktree" "always")))
      ;; A start of its own keeps nothing.
      (should-not org-varde--answers)
      (org-varde--plan-answering '("start" "T3" "--json") "Three" #'ignore #'ignore)
      (should (= worktree-asked 2))
      (should (equal (car defaults) "/p/a")))))

(ert-deftest org-varde-a-question-asked-again-is-not-asked-forever ()
  (org-varde-test-with-varde
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"project\",\"choices\":[]}")
    (let ((asked 0) why)
      (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) (setq asked (1+ asked)) "/p/")))
        (org-varde--plan-answering '("start" "T1" "--json") nil #'ignore (lambda (w) (setq why w)))
        (org-varde-test-wait))
      (should (= asked 1))
      (should (equal why "Varde asked again about the project")))
    ;; A failure is shown, not answered.
    (org-varde-test-fail-once "start" "varde: git worktree add --worktree-ish failed")
    (let (why)
      (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked"))))
        (org-varde--plan-answering '("start" "T1" "--json") nil #'ignore (lambda (w) (setq why w)))
        (org-varde-test-wait))
      (should (stringp why)))))

(ert-deftest org-varde-doctor-shows-the-report-when-a-check-fails ()
  (org-varde-test-with-varde
    (org-varde-test-reply "doctor" "ok  store\n!!  hooks    not installed\n")
    (org-varde-test-fail-once "doctor" "varde: some checks failed")
    (save-window-excursion (org-varde-doctor))
    (with-current-buffer "*varde doctor*"
      (should (string-match-p "hooks    not installed" (buffer-string)))
      (should (string-match-p "some checks failed" (buffer-string))))))

(ert-deftest org-varde-a-running-task-is-focused-not-restarted ()
  (org-varde-test-with-varde
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"focus\",\"run\":\"R1\"}")
    (let* ((buffer (get-buffer-create "*varde xyz123*"))
           (proc (make-process :name "fake-shell" :buffer buffer :command '("cat") :noquery t)))
      (unwind-protect
          (progn
            (with-current-buffer buffer (setq org-varde--task org-varde-test--id))
            (org-varde--start org-varde-test--id "t" nil nil)
            (org-varde-test-wait)
            (should (eq (window-buffer) buffer))
            (should-not org-varde-test-sent))
        (delete-process proc)))))

(ert-deftest org-varde-a-session-typed-into-a-terminal-is-focused-there ()
  (org-varde-test-with-varde
    (let* ((buffer (get-buffer-create "*vterm*"))
           (proc (make-process :name "fake-vterm" :buffer buffer :command '("cat")
                               :connection-type 'pty :noquery t)))
      (unwind-protect
          (progn
            (org-varde-test-reply "start" (format "{\"v\":1,\"action\":\"focus\",\"run\":\"R1\",\"tty\":\"%s\"}"
                                                  (process-tty-name proc)))
            (org-varde--start org-varde-test--id "t" nil nil)
            (org-varde-test-wait)
            (should (eq (window-buffer) buffer))
            ;; It is left as it is: not taken over as the task's terminal.
            (should-not (buffer-local-value 'org-varde--task buffer))
            (should-not org-varde-test-terminals)
            (should-not org-varde-test-sent))
        (delete-process proc)
        (kill-buffer buffer)))))

(ert-deftest org-varde-a-session-on-an-unknown-terminal-is-not-duplicated ()
  (org-varde-test-with-varde
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"focus\",\"run\":\"R1\",\"tty\":\"/dev/pts/9999\"}")
    (org-varde--start org-varde-test--id "t" nil nil)
    (org-varde-test-wait)
    (should-not org-varde-test-terminals)
    (should-not org-varde-test-sent)))

(ert-deftest org-varde-varde-errors-read-as-varde-wrote-them ()
  (org-varde-test-with-varde
    (org-varde-test-fail-once "show" "varde: no task matches 'zzzz'")
    (should (equal (cadr (should-error (org-varde--varde "show" "zzzz") :type 'org-varde-varde-error))
                   "no task matches 'zzzz'"))))

(ert-deftest org-varde-varde-runs-from-a-buffer-whose-directory-is-gone ()
  "As from a task's terminal once `done' has removed its worktree."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":1,\"groups\":[]}")
    (let ((default-directory (file-name-as-directory (expand-file-name "gone" org-varde-test-dir)))
          (org-varde-board-data nil))
      (should (org-varde--varde "list" "--json"))
      (org-varde--list)
      (org-varde-test-wait)
      (should (equal org-varde-board-data '(:v 1 :groups nil))))))

;;; --- Finishing ---------------------------------------------------------------

(ert-deftest org-varde-marking-a-linked-heading-done-closes-its-task ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" (format "{\"v\":1,\"task\":\"%s\",\"worktree\":null}" org-varde-test--id))
    (org-varde-test-with-org org-varde-test--linked
      (org-varde-test--goto "Fix the login bug")
      (let ((org-varde-mode t))
        (org-varde-test--with-todo-hook
          (org-todo "DONE")))
      (should (member (format "done %s --json" org-varde-test--id) (org-varde-test-calls))))))

(ert-deftest org-varde-done-asks-nothing-about-a-kept-worktree ()
  "varde removes it once its work lands."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":{\"path\":\"/p/.worktrees/x\",\"branch\":\"varde/x\",\"base\":\"main\",\"ahead\":2,\"dirty\":false,\"action\":\"kept\"}}")
    (cl-letf (((symbol-function 'read-multiple-choice)
               (lambda (&rest _) (error "Asked about the worktree"))))
      (org-varde--finish org-varde-test--id "t"))
    (should (equal (seq-filter (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls))
                   (list (format "done %s --json" org-varde-test--id))))))

(defmacro org-varde-test--with-terminal (&rest body)
  "Run BODY with a live terminal for the test task, bound to `terminal'."
  (declare (indent 0))
  `(let* ((terminal (get-buffer-create "*varde xyz123*"))
          (proc (make-process :name "fake-shell" :buffer terminal :command '("cat") :noquery t)))
     (with-current-buffer terminal (setq org-varde--task org-varde-test--id))
     (unwind-protect (progn ,@body)
       (when (process-live-p proc) (delete-process proc)))))

(ert-deftest org-varde-a-busy-terminal-is-not-typed-into ()
  "A second start before the first session took over its terminal."
  (org-varde-test-with-varde
    (org-varde-test--with-terminal
      (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) t)))
        (org-varde--open-terminal org-varde-test--id "t" "/tmp/proj" "varde run R1"))
      (should-not org-varde-test-sent)
      (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) nil)))
        (org-varde--open-terminal org-varde-test--id "t" "/tmp/proj" "varde run R1"))
      (should (equal org-varde-test-sent '("varde run R1"))))))

(ert-deftest org-varde-done-stops-the-session-and-closes-its-terminal ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test--with-terminal
      (org-varde--finish org-varde-test--id "t")
      (should (member (format "done %s --json" org-varde-test--id) (org-varde-test-calls)))
      (should-not (buffer-live-p terminal)))))

(ert-deftest org-varde-declining-to-stop-a-busy-session-undoes-the-done ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-with-org org-varde-test--linked
      (org-varde-test--goto "Fix the login bug")
      (org-varde-test--with-terminal
        (let ((org-varde-mode t)
              (org-varde-board-data
               `(:groups ((:key "working" :rows ((:task ,org-varde-test--id :status "working"))))))
              (undo nil))
          (add-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change)
          (unwind-protect
              (cl-letf (((symbol-function 'run-at-time)
                         (lambda (_ _ f &rest args) (setq undo (lambda () (apply f args)))))
                        ((symbol-function 'y-or-n-p) (lambda (&rest _) nil)))
                (org-todo "DONE")
                (funcall undo))
            (remove-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change)))
        (should (equal (org-get-todo-state) "TODO"))
        (should-not (seq-find (lambda (c) (string-match-p "^\\(done\\|reopen\\)" c))
                              (org-varde-test-calls)))
        (should (buffer-live-p terminal))))))

(defun org-varde-test--look-at (terminal unread)
  "Show TERMINAL in the selected window of a focused frame, with the board
saying its task is UNREAD or not, and wait for any `varde seen'."
  (let ((org-varde-board-data
         `(:groups ((:key "needs_you" :rows ((:task ,org-varde-test--id :status "waiting" :unread ,unread)))))))
    (save-window-excursion
      (set-window-buffer (selected-window) terminal)
      (cl-letf (((symbol-function 'frame-focus-state) (lambda (&rest _) t)))
        (org-varde--mark-seen)
        (org-varde-test-wait)))))

(ert-deftest org-varde-looking-at-an-unread-terminal-marks-it-seen ()
  (org-varde-test-with-varde
    (org-varde-test--with-terminal
      (org-varde-test--look-at terminal nil)
      (should-not (org-varde-test-calls))
      (org-varde-test--look-at terminal t)
      (should (equal (org-varde-test-calls) (list (format "seen %s" org-varde-test--id)))))))

(ert-deftest org-varde-looking-at-a-task-view-marks-nothing-seen ()
  "Only the terminal shows what a session finished; a view of the task,
like `org-varde-show' or the cairn editor, does not."
  (org-varde-test-with-varde
    (with-temp-buffer
      (setq-local org-varde--task org-varde-test--id)
      (org-varde-test--look-at (current-buffer) t)
      (should-not (org-varde-test-calls)))))

(ert-deftest org-varde-a-task-already-done-is-only-put-away ()
  "Marking DONE a heading whose task varde already closed, say after the
DONE was lost, asks nothing again: no worktree choice, no second close."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"done\"}")
    (org-varde-test--with-terminal
      (cl-letf (((symbol-function 'read-multiple-choice)
                 (lambda (&rest _) (error "Asked about the worktree again"))))
        (org-varde--finish org-varde-test--id "t"))
      (should-not (seq-some (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls)))
      (should-not (buffer-live-p terminal)))))

(ert-deftest org-varde-a-session-kept-running-survives-a-second-done ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"done\"}")
    (org-varde-test--with-terminal
      (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) t)))
        (org-varde--finish org-varde-test--id "t"))
      (should-not (seq-some (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls)))
      (should (buffer-live-p terminal)))))

(ert-deftest org-varde-tasks-done-elsewhere-have-their-terminals-put-away ()
  (org-varde-test-with-varde
    (org-varde-test--with-terminal
      (let ((org-varde-board-data
             `(:groups ((:key "done" :rows ((:task ,org-varde-test--id :status "done")))))))
        ;; Still running a session kept with --keep-running: left open.
        (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) t)))
          (org-varde--put-away-done))
        (should (buffer-live-p terminal))
        (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) nil)))
          (org-varde--put-away-done))
        (should-not (buffer-live-p terminal))))))

(ert-deftest org-varde-starting-a-done-heading-reopens-it ()
  "Else the board would close the task again when it opens."
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test-with-org (replace-regexp-in-string "TODO Fix" "DONE Fix" org-varde-test--linked)
      (org-varde-test--goto "Fix the login bug")
      (org-varde-start)
      (org-varde-test-wait)
      (with-current-buffer (find-buffer-visiting file)
        (org-varde-test--goto "Fix the login bug")
        (should (equal (org-get-todo-state) "TODO"))))))

(ert-deftest org-varde-an-archived-heading-is-not-started ()
  (org-varde-test-with-varde
    (org-varde-test-with-org "* TODO Shelved :ARCHIVE:\n"
      (should-error (org-varde-start) :type 'user-error)
      (should-not (org-varde-test-calls)))))

(ert-deftest org-varde-show-comes-at-once-and-git-follows ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "abc123  A task")
    (save-window-excursion (org-varde-show org-varde-test--id))
    (should (equal (org-varde-test-calls)
                   (list (format "show %s --no-git" org-varde-test--id)
                         (format "show %s" org-varde-test--id))))
    (should (equal (with-current-buffer "*varde show*" (buffer-string)) "abc123  A task"))))

(ert-deftest org-varde-going-to-a-live-session-does-not-wait-for-its-text ()
  (org-varde-test-with-varde
    (org-varde-test-with-org org-varde-test--linked
      (org-varde-test--goto "Fix the login bug")
      (let ((org-varde-board-data
             `(:groups ((:key "needs_you" :rows ((:task ,org-varde-test--id :status "waiting"
                                                         :project ,org-varde-test--proj)))))))
        (cl-letf (((symbol-function 'org-varde--call) (lambda (&rest _) (error "Waited"))))
          (should (equal (org-varde--ensure-task) org-varde-test--id))))
      (should (equal (org-varde-test-calls)
                     (list (format "set %s --title=Fix the login bug --body -" org-varde-test--id))))
      (should (equal (org-varde-test-stdin "set") "It fails on CI only.")))))

(ert-deftest org-varde-a-heading-moved-to-another-project-moves-its-task ()
  "Its inherited VARDE_PROJECT goes with the title and body when the
board has the task elsewhere."
  (org-varde-test-with-varde
    (org-varde-test-with-org org-varde-test--linked
      (org-varde-test--goto "Fix the login bug")
      (let ((org-varde-board-data
             `(:groups ((:key "open" :rows ((:task ,org-varde-test--id :status nil :project "/tmp/old")))))))
        (org-varde-test-reply "set" "moved xyz123 to /tmp/proj")
        (org-varde--ensure-task)
        (should (equal (org-varde-test-calls)
                       (list (format "set %s --title=Fix the login bug --body - --project %s"
                                     org-varde-test--id org-varde-test--proj))))
        ;; Where the board has it already, only the text goes.
        (setf (plist-get (car (plist-get (car (plist-get org-varde-board-data :groups)) :rows)) :project)
              org-varde-test--proj)
        (org-varde--ensure-task)
        (should (equal (cadr (org-varde-test-calls))
                       (format "set %s --title=Fix the login bug --body -" org-varde-test--id)))))))

(ert-deftest org-varde-a-project-not-on-this-machine-moves-nothing ()
  "Such as one named in a file synced from another machine."
  (org-varde-test-with-varde
    (org-varde-test-with-org (replace-regexp-in-string (regexp-quote org-varde-test--proj)
                                                       "/nonexistent/proj" org-varde-test--linked t t)
      (org-varde-test--goto "Fix the login bug")
      (org-varde--ensure-task)
      (should (equal (org-varde-test-calls)
                     (list (format "set %s --title=Fix the login bug --body -" org-varde-test--id)))))))

(ert-deftest org-varde-a-refused-move-says-why-and-keeps-the-text-coming ()
  (org-varde-test-with-varde
    (org-varde-test-with-org org-varde-test--linked
      (org-varde-test--goto "Fix the login bug")
      (let ((org-varde-board-data
             `(:groups ((:key "open" :rows ((:task ,org-varde-test--id :status nil :project "/tmp/old"))))))
            (said nil))
        (org-varde-test-fail-once "set" "varde: xyz123 has an active worktree, /w, whose work belongs to /tmp/old")
        (cl-letf (((symbol-function 'message)
                   (lambda (fmt &rest args) (push (apply #'format-message fmt args) said))))
          (should (equal (org-varde--ensure-task) org-varde-test--id)))
        (should (equal (org-varde-test-calls)
                       (list (format "set %s --title=Fix the login bug --body - --project %s"
                                     org-varde-test--id org-varde-test--proj)
                             (format "set %s --title=Fix the login bug --body -" org-varde-test--id))))
        (should (equal (org-varde-test-stdin "set") "It fails on CI only."))
        (should (member "varde: xyz123 has an active worktree, /w, whose work belongs to /tmp/old" said))
        ;; With a live session, the same, without waiting.
        (setf (plist-get (car (plist-get (car (plist-get org-varde-board-data :groups)) :rows)) :status)
              "waiting")
        (org-varde-test-fail-once "set" "varde: xyz123 has a live session")
        (org-varde--ensure-task)
        (should (equal (nthcdr 2 (org-varde-test-calls))
                       (list (format "set %s --title=Fix the login bug --body - --project %s"
                                     org-varde-test--id org-varde-test--proj)
                             (format "set %s --title=Fix the login bug --body -" org-varde-test--id))))))))

(ert-deftest org-varde-show-keys-stay-in-the-show-buffer ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "abc123  A task")
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (should (eq major-mode 'org-varde-show-mode))
      (should (eq (key-binding "e") #'org-varde-edit-cairn)))
    ;; Not bound in every special-mode buffer.
    (should-not (lookup-key special-mode-map "e"))
    (kill-buffer "*varde show*")))

(ert-deftest org-varde-a-saved-cairn-shows-in-its-views ()
  "After C-c C-c the task and project views show the new cairn, and g
fetches them again."
  (org-varde-test-with-varde
    (let ((dir (file-name-as-directory org-varde-test-dir)))
      (org-varde-test-reply "show" "Old cairn")
      (save-window-excursion
        (org-varde-show org-varde-test--id)
        (org-varde-show-project dir))
      (org-varde-test-wait)
      (org-varde-test-reply "edit" "Open:\n")
      (save-window-excursion (org-varde-edit-cairn org-varde-test--id))
      (org-varde-test-reply "show" "New cairn")
      (with-current-buffer "*cairn xyz123*" (org-varde-cairn-save))
      (org-varde-test-wait)
      (should (equal (with-current-buffer "*varde show*" (buffer-string)) "New cairn"))
      (should (equal (with-current-buffer "*varde project*" (buffer-string)) "New cairn"))
      (org-varde-test-reply "show" "Newer still")
      (with-current-buffer "*varde project*" (revert-buffer))
      (should (equal (with-current-buffer "*varde project*" (buffer-string)) "Newer still")))))

(ert-deftest org-varde-a-recalled-task-opens-from-its-hit ()
  (org-varde-test-with-varde
    (org-varde-test-reply "recall" (concat "nothing in this project; the best from every project:\n"
                                           "xyz123  Fix the login bug (done)  (/p)\n"
                                           "    finding: the token expires\n"
                                           "project  /p\n"
                                           "    gotcha: run make first\n"))
    (org-varde-test-reply "show" "xyz123  Fix the login bug")
    (save-window-excursion (org-varde-recall "token"))
    (with-current-buffer "*varde recall*"
      (forward-line 2)
      (should (eq (key-binding (kbd "RET")) #'org-varde-show))
      (should (equal (org-varde--task-here) "xyz123"))
      (save-window-excursion (call-interactively #'org-varde-show))
      (should (member "show xyz123 --no-git" (org-varde-test-calls)))
      (forward-line 1)
      (should-error (org-varde--task-here) :type 'user-error)
      (forward-line -2)
      (org-varde-test-with-org org-varde-test--linked
        (save-window-excursion
          (with-current-buffer "*varde recall*" (org-varde-recall-visit))
          (should (equal (buffer-file-name) file))
          (should (looking-at "\\*\\* TODO Fix the login bug")))))))

;;; --- Finding headings --------------------------------------------------------

(ert-deftest org-varde-linked-headings-are-found-in-files-and-archives ()
  (let* ((dir (make-temp-file "org-varde-link-" t))
         (a (expand-file-name "a.org" dir))
         (org-agenda-files (list a))
         (org-todo-keywords '((sequence "TODO" "|" "DONE" "CANCELLED"))))
    (unwind-protect
        (progn
          (with-temp-file a
            (insert "* TODO One\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                    "* DONE Two\nCLOSED: [2026-09-23 Wed 10:00]\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"
                    "* Plain\n:VARDE_TASK: not-in-a-drawer\n"))
          (with-temp-file (concat a "_archive")
            (insert "* CANCELLED Three\n:PROPERTIES:\n:ARCHIVE_TIME: x\n:VARDE_TASK: CCC333\n:END:\n"))
          (should (equal (mapcar (lambda (l) (list (car l) (nth 3 l) (nth 4 l))) (org-varde--linked))
                         '(("AAA111" nil nil) ("BBB222" t nil) ("CCC333" t t))))
          (should (equal (org-varde--find-heading "BBB222") (cons a 51))))
      (delete-directory dir t))))

(ert-deftest org-varde-an-archive-is-known-through-a-symlinked-directory ()
  "Agenda files named through a symlink, as a synced org directory often
is, and an archive without the _archive suffix."
  (let* ((dir (file-truename (make-temp-file "org-varde-real-" t)))
         (link (concat dir "-link"))
         (org-directory nil)
         (org-agenda-files (list (expand-file-name "a.org" link)))
         (org-archive-location "archive.org::"))
    (unwind-protect
        (progn
          (make-symbolic-link dir link)
          (with-temp-file (expand-file-name "a.org" dir) (insert "* TODO One\n"))
          (with-temp-file (expand-file-name "archive.org" dir)
            (insert "* DONE Two\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"))
          (should (equal (mapcar (lambda (l) (list (car l) (nth 4 l))) (org-varde--linked))
                         '(("BBB222" t)))))
      (delete-file link)
      (delete-directory dir t))))

(ert-deftest org-varde-archives-kept-elsewhere-are-searched ()
  "Archives mirrored into their own tree, as `org-archive-location' can be
set up to do, hold linked headings too."
  (let* ((org-directory (make-temp-file "org-varde-dir-" t))
         (a (expand-file-name "projects/p.org" org-directory))
         (archived (expand-file-name "archive/projects/p.org_archive" org-directory))
         (org-agenda-files (list a)))
    (unwind-protect
        (progn
          (make-directory (file-name-directory a) t)
          (make-directory (file-name-directory archived) t)
          (make-directory (expand-file-name "archive/.worktrees/x" org-directory) t)
          (with-temp-file a (insert "* TODO Live\n"))
          (with-temp-file archived
            (insert "* DONE Old\n:PROPERTIES:\n:ARCHIVE_FILE: p.org\n:VARDE_TASK: OLD111\n:END:\n"))
          (with-temp-file (expand-file-name "archive/.worktrees/x/q.org_archive" org-directory)
            (insert "* DONE Copy\n:PROPERTIES:\n:VARDE_TASK: NOPE00\n:END:\n"))
          (should (equal (org-varde--find-heading "OLD111") (cons archived 1)))
          (should-not (org-varde--find-heading "NOPE00"))
          ;; A shared archive file named by the global location is searched too.
          (let ((org-archive-location "../all-archive.org::"))
            (with-temp-file (expand-file-name "all-archive.org" org-directory)
              (insert "* DONE Shared\n:PROPERTIES:\n:VARDE_TASK: SHR222\n:END:\n"))
            (should (org-varde--find-heading "SHR222"))))
      (delete-directory org-directory t))))

(ert-deftest org-varde-an-unsaved-link-is-found ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" (format "{\"v\":1,\"id\":\"%s\"}" org-varde-test--id))
    (org-varde-test-with-org org-varde-test--org
      (goto-char (point-max))
      (insert "an edit of my own")
      (org-varde-test--goto "Fix the login bug")
      (org-varde--ensure-task)
      ;; The user's edit is theirs to save.
      (should (buffer-modified-p))
      (should-not (string-match-p org-varde-test--id (org-varde-test--saved file)))
      (should (org-varde--find-heading org-varde-test--id)))))

(ert-deftest org-varde-a-link-in-a-clean-buffer-is-saved ()
  "A revert, say after the file is written outside Emacs, keeps the link."
  (org-varde-test-with-varde
    (org-varde-test-reply "new" (format "{\"v\":1,\"id\":\"%s\"}" org-varde-test--id))
    (org-varde-test-with-org org-varde-test--org
      (org-varde-test--goto "Fix the login bug")
      (org-varde--ensure-task)
      (should-not (buffer-modified-p))
      (revert-buffer t t)
      (should (org-varde--find-heading org-varde-test--id)))))

(ert-deftest org-varde-archived-headings-close-their-tasks ()
  "Archiving puts a task away even if it was never marked DONE: whether
moved to an archive file or tagged ARCHIVE where it is."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":1,\"groups\":[{\"key\":\"open\",\"label\":\"Open\",\"rows\":[{\"task\":\"AAA111\"},{\"task\":\"TAG222\"},{\"task\":\"LIVE33\"}]}]}")
    (let* ((org-directory org-varde-test-dir)
           (live (expand-file-name "p.org" org-directory))
           (org-agenda-files (list live)))
      (with-temp-file live
        (insert "* TODO Shelved here :ARCHIVE:\n:PROPERTIES:\n:VARDE_TASK: TAG222\n:END:\n"
                "* TODO Still going\n:PROPERTIES:\n:VARDE_TASK: LIVE33\n:END:\n"))
      (with-temp-file (concat live "_archive")
        (insert "* TODO Moved away\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"))
      (org-varde-reconcile)
      (should (member "list --json --all" (org-varde-test-calls)))
      (should (member "done AAA111" (org-varde-test-calls)))
      (should (member "done TAG222" (org-varde-test-calls)))
      (should-not (member "done LIVE33" (org-varde-test-calls))))))

(ert-deftest org-varde-headings-done-elsewhere-close-their-tasks ()
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":1,\"groups\":[{\"key\":\"open\",\"label\":\"Open\",\"rows\":[{\"task\":\"BBB222\"}]},{\"key\":\"done\",\"label\":\"Done today\",\"rows\":[]}]}")
    (org-varde-test-with-org "* DONE Two\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n* TODO One\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-reconcile)
      (should (member "done BBB222" (org-varde-test-calls)))
      (should-not (member "done AAA111" (org-varde-test-calls))))))

;; Headings outside the agenda: under `org-directory', or in an open buffer.
(ert-deftest org-varde-headings-outside-the-agenda-are-found ()
  (org-varde-test-with-varde
    (let ((notes (expand-file-name "notes/side.org" org-directory))
          (elsewhere (make-temp-file "org-varde-elsewhere-" nil ".org"
                                     "* TODO Open here\n:PROPERTIES:\n:VARDE_TASK: OPEN22\n:END:\n")))
      (make-directory (file-name-directory notes) t)
      (with-temp-file notes
        (insert "* DONE Side\nCLOSED: [2026-09-23 Wed 10:00]\n:PROPERTIES:\n:VARDE_TASK: SIDE11\n:END:\n"))
      (unwind-protect
          (progn
            (should (equal (org-varde--find-heading "SIDE11") (cons notes 1)))
            (should (equal (nth 6 (assoc "SIDE11" (org-varde--linked)))
                           (* 1000 (truncate (float-time (encode-time '(0 0 10 23 9 2026 nil -1 nil)))))))
            (should-not (org-varde--find-heading "OPEN22"))
            (with-current-buffer (find-file-noselect elsewhere)
              (should (org-varde--find-heading "OPEN22"))
              (kill-buffer)))
        (delete-file elsewhere)))))

(ert-deftest org-varde-lookups-read-a-file-again-only-once-it-changes ()
  (org-varde-test-with-varde
    (let ((f (expand-file-name "a.org" org-directory))
          (reads 0))
      (with-temp-file f (insert "* TODO A\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"))
      (should (org-varde--find-heading "AAA111"))
      (cl-letf* ((real (symbol-function 'insert-file-contents))
                 ((symbol-function 'insert-file-contents)
                  (lambda (&rest args) (setq reads (1+ reads)) (apply real args))))
        (org-varde--find-heading "AAA111")
        (should (= reads 0))
        (with-temp-file f (insert "* TODO B\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"))
        (set-file-times f (time-add nil 10))
        (should (org-varde--find-heading "BBB222"))
        (should (= reads 1))))))

;; The more recent change wins, so neither side undoes the other.
(ert-deftest org-varde-a-task-reopened-after-its-heading-closed-is-left-running ()
  "B2: `varde start' in a terminal on a task whose heading is DONE."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"AAA111\",\"status\":\"waiting\",\"last\":1790000000000}]},{\"key\":\"working\",\"rows\":[{\"task\":\"BBB222\",\"status\":\"working\",\"last\":1}]}]}")
    (org-varde-test-with-org (concat "* DONE Reopened from the CLI\nCLOSED: [2026-09-01 Tue 10:00]\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                                     "* DONE Busy\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n")
      (save-buffer)
      (org-varde-reconcile)
      (should (equal (org-varde-test-calls) '("list --json --all")))
      ;; varde changed last: the heading follows it, and is saved.
      (org-varde-test--goto "Reopened")
      (should (equal (org-get-todo-state) "TODO"))
      (should-not (buffer-modified-p))
      ;; The heading is newer, but its session is at work: left alone.
      (org-varde-test--goto "Busy")
      (should (equal (org-get-todo-state) "DONE")))))

(ert-deftest org-varde-a-task-closed-elsewhere-closes-its-heading ()
  "An agent's `varde done' reaches the heading on the next refresh."
  (org-varde-test-with-varde
    (org-varde-test-with-org (concat "#+TODO: TODO SOMEDAY | DONE\n"
                                     "* TODO Closed by an agent\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                                     "* TODO Parked in a terminal\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"
                                     "* TODO Still open\n:PROPERTIES:\n:VARDE_TASK: CCC333\n:END:\n")
      (save-buffer)
      (let ((org-varde--closed nil)
            (org-varde-board-data
             (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"CCC333\",\"last\":1}]},{\"key\":\"done\",\"rows\":[{\"task\":\"AAA111\",\"status\":\"done\",\"last\":1}]},{\"key\":\"parked\",\"rows\":[{\"task\":\"BBB222\",\"status\":\"parked\",\"last\":1}]}]}")))
        (org-varde--follow-closed)
        (should-not (org-varde-test-calls))
        (should-not (buffer-modified-p))
        (should (string-match-p "^\\* DONE Closed by an agent\n\\(CLOSED: .*\n\\)?.*\n.*\n.*\n\\* SOMEDAY Parked in a terminal\n.*\n.*\n.*\n\\* TODO Still open"
                                (org-varde-test--saved file)))
        ;; Seen once: set back by hand, the heading is not closed again.
        (org-varde-test--goto "Closed by an agent")
        (org-varde--set-keyword "TODO")
        (org-varde--follow-closed)
        (should (equal (org-get-todo-state) "TODO"))
        ;; Reopened in a terminal: the parked heading follows.
        (setq org-varde-board-data
              (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"CCC333\",\"last\":1},{\"task\":\"BBB222\",\"state\":\"open\",\"last\":1,\"changed\":4102444800000}]},{\"key\":\"done\",\"rows\":[{\"task\":\"AAA111\",\"status\":\"done\",\"last\":1}]}]}"))
        (org-varde--follow-closed)
        (org-varde-test--goto "Parked in a terminal")
        (should (equal (org-get-todo-state) "TODO"))
        (should-not (org-varde-test-calls))))))

(ert-deftest org-varde-a-task-dropped-or-reopened-elsewhere-moves-its-heading ()
  "A `varde drop' closes the heading; a plain `varde reopen' after the
heading closed reopens it, by `changed', not by the last activity."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"BBB222\",\"state\":\"open\",\"status\":\"exited\",\"last\":1,\"changed\":4102444800000}]},{\"key\":\"dropped\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"dropped\",\"last\":1,\"changed\":4102444800000}]}]}")
    (org-varde-test-with-org (concat "* TODO Thrown away\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                                     "* DONE Reopened\nCLOSED: [2026-09-01 Tue 10:00]\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n")
      (save-buffer)
      (org-varde-reconcile)
      (should (equal (org-varde-test-calls) '("list --json --all")))
      (org-varde-test--goto "Thrown away")
      (should (equal (org-get-todo-state) "DONE"))
      (org-varde-test--goto "Reopened")
      (should (equal (org-get-todo-state) "TODO")))))

(ert-deftest org-varde-a-reopen-in-the-minute-its-heading-closed-wins ()
  "Org keeps no seconds: a task reopened at 10:00:30 is newer than a
CLOSED of 10:00, and its session is not stopped again."
  (org-varde-test-with-varde
    (let ((changed (* 1000 (+ 30 (truncate (float-time (encode-time '(0 0 10 1 9 2026 nil -1 nil))))))))
      (org-varde-test-reply "list" (format "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"open\",\"status\":\"waiting\",\"last\":1,\"changed\":%d}]}]}" changed)))
    (org-varde-test-with-org "* DONE Closed then reopened\nCLOSED: [2026-09-01 Tue 10:00]\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (save-buffer)
      (org-varde-reconcile)
      (should (equal (org-varde-test-calls) '("list --json --all")))
      (org-varde-test--goto "Closed then reopened")
      (should (equal (org-get-todo-state) "TODO")))))

(ert-deftest org-varde-a-save-elsewhere-does-not-close-a-waiting-session ()
  "A heading dated only by its file's save may be older than it looks:
it closes or parks no task whose session is waiting.  One closed with a
time does."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"open\",\"status\":\"waiting\",\"last\":1,\"changed\":1}]},{\"key\":\"needs_you\",\"rows\":[{\"task\":\"BBB222\",\"state\":\"open\",\"status\":\"waiting\",\"last\":1,\"changed\":1}]},{\"key\":\"needs_you\",\"rows\":[{\"task\":\"CCC333\",\"state\":\"open\",\"status\":\"waiting\",\"last\":1,\"changed\":1}]}]}")
    (org-varde-test-with-org (concat "#+TODO: TODO SOMEDAY | DONE\n"
                                     "* DONE Reopened in a terminal\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                                     "* SOMEDAY Also reopened\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"
                                     "* DONE Closed with a time\nCLOSED: [2026-09-01 Tue 10:00]\n:PROPERTIES:\n:VARDE_TASK: CCC333\n:END:\n"
                                     "* Notes\nan unrelated edit\n")
      (save-buffer)
      (org-varde-reconcile)
      (should (equal (org-varde-test-calls) '("list --json --all" "done CCC333"))))))

(ert-deftest org-varde-an-undated-done-beside-a-waiting-session-is-said ()
  "Neither side wins, so the task is left out of step, and you are told."
  (org-varde-test-with-varde
    (org-varde-test-with-org "* DONE Closed without a time\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (save-buffer)
      (should (equal (org-varde--reconcile
                      (plist-get (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"open\",\"status\":\"waiting\",\"last\":1,\"changed\":1}]}]}")
                                 :groups))
                     '("AAA111")))
      (should (string-match-p "heading of aaa111 is done, with no CLOSED time" (car org-varde--said)))
      (should-not (org-varde-test-calls))
      (should (equal (org-get-todo-state) "DONE")))))

;;; --- Parking -----------------------------------------------------------------

(defconst org-varde-test--parkable
  "#+TODO: TODO SOMEDAY | DONE\n* TODO Forge integration\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n")

(ert-deftest org-varde-someday-parks-and-todo-reopens ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "park" "{\"v\":1,\"task\":\"AAA111\",\"stopped\":[]}")
    (org-varde-test-with-org org-varde-test--parkable
      (org-varde-test--goto "Forge integration")
      (org-varde-test--with-todo-hook
        (org-todo "SOMEDAY")
        (should (member "park AAA111 --json" (org-varde-test-calls)))
        (should-not (seq-some (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls)))
        (org-todo "TODO")
        (should (member "reopen AAA111" (org-varde-test-calls)))))))

(ert-deftest org-varde-done-set-back-to-todo-reopens ()
  "A DONE set by mistake is undone by setting the heading back."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org org-varde-test--parkable
      (org-varde-test--goto "Forge integration")
      (org-varde-test--with-todo-hook
        (org-todo "DONE")
        (should (member "done AAA111 --json" (org-varde-test-calls)))
        (org-todo "TODO")
        (should (member "reopen AAA111" (org-varde-test-calls)))))))

(ert-deftest org-varde-done-on-a-copied-heading-changes-nothing ()
  "Either heading may be the one meant."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org (concat org-varde-test--linked
                                     "* TODO Copy\n:PROPERTIES:\n:VARDE_TASK: " org-varde-test--id "\n:END:\n")
      (save-buffer)
      (org-varde-test--goto "Copy")
      (let (said)
        (cl-letf (((symbol-function 'message) (lambda (&rest args) (setq said (apply #'format args)))))
          (org-varde-test--with-todo-hook
            (org-todo "DONE")))
        (should (string-match-p "linked from .*:4 and .*:10" said)))
      (should (equal (org-get-todo-state) "TODO"))
      (should-not (seq-some (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls))))))

(ert-deftest org-varde-parking-closes-the-terminal ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "park" "{\"v\":1,\"stopped\":[]}")
    (org-varde-test--with-terminal
      (org-varde--park org-varde-test--id "t")
      (should (member (format "park %s --json" org-varde-test--id) (org-varde-test-calls)))
      (should-not (buffer-live-p terminal)))))

(ert-deftest org-varde-board-park-sets-someday-and-saves-it ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "park" "{\"v\":1,\"stopped\":[]}")
    (org-varde-test-with-org org-varde-test--parkable
      (org-varde-test--with-todo-hook
        (org-varde-park-task "AAA111" "Forge integration"))
      (should (member "park AAA111 --json" (org-varde-test-calls)))
      (should-not (buffer-modified-p))
      (should (string-match-p "^\\* SOMEDAY Forge"
                              (org-varde-test--saved file))))))

(ert-deftest org-varde-board-park-without-the-keyword-leaves-the-heading ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "park" "{\"v\":1,\"stopped\":[]}")
    (org-varde-test-with-org "* TODO Plain\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-park-task "AAA111" "Plain")
      (should (member "park AAA111 --json" (org-varde-test-calls)))
      (goto-char (point-min))
      (should (equal (org-get-todo-state) "TODO")))))

(ert-deftest org-varde-starting-a-someday-heading-makes-it-todo ()
  "Else the board would park the task again when it opens."
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test-with-org (replace-regexp-in-string "TODO Forge" "SOMEDAY Forge"
                                                       org-varde-test--parkable)
      (org-varde-test--goto "Forge integration")
      (org-varde-start)
      (org-varde-test-wait)
      (with-current-buffer (find-buffer-visiting file)
        (org-varde-test--goto "Forge integration")
        (should (equal (org-get-todo-state) "TODO"))))))

(ert-deftest org-varde-someday-headings-park-their-tasks ()
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":1,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"AAA111\"},{\"task\":\"BBB222\"}]},{\"key\":\"parked\",\"collapsed\":true,\"rows\":[{\"task\":\"CCC333\"},{\"task\":\"DDD444\"}]},{\"key\":\"done\",\"rows\":[{\"task\":\"EEE555\"}]}]}")
    (org-varde-test-with-org
        (concat "#+TODO: TODO SOMEDAY | DONE\n"
                "* SOMEDAY Open one\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                "* TODO Still going\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"
                "* SOMEDAY Already parked\n:PROPERTIES:\n:VARDE_TASK: CCC333\n:END:\n"
                "* DONE Parked then done\n:PROPERTIES:\n:VARDE_TASK: DDD444\n:END:\n"
                "* SOMEDAY Done by mistake\n:PROPERTIES:\n:VARDE_TASK: EEE555\n:END:\n")
      (org-varde-reconcile)
      (should (member "park AAA111" (org-varde-test-calls)))
      (should (member "park EEE555" (org-varde-test-calls)))
      (should-not (seq-some (lambda (c) (string-match-p "BBB222\\|CCC333" c)) (org-varde-test-calls)))
      (should (member "done DDD444" (org-varde-test-calls))))))

;;; --- Settling keyword changes ----------------------------------------------

(defmacro org-varde-test--with-held-hook (&rest body)
  "Run BODY with the todo hook on and keyword changes held until
`org-varde-test--settle' lets them settle."
  (declare (indent 0))
  `(progn
     (add-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change)
     (unwind-protect
         (cl-letf (((symbol-function 'run-at-time)
                    (lambda (_ _ f &rest args)
                      (unless (eq f #'org-varde--settle) (apply f args))
                      (timer-create))))
           ,@body)
       (setq org-varde--pending nil)
       (remove-hook 'org-after-todo-state-change-hook #'org-varde--on-todo-change))))

(defun org-varde-test--settle ()
  (dolist (p org-varde--pending) (org-varde--settle (car p))))

(defun org-varde-test--acts ()
  "The calls that change a task."
  (seq-filter (lambda (c) (string-match-p "^\\(done\\|park\\|reopen\\)" c)) (org-varde-test-calls)))

(ert-deftest org-varde-cycling-keywords-acts-once-where-it-ends ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org org-varde-test--parkable
      (org-varde-test--goto "Forge integration")
      (org-varde-test--with-held-hook
        ;; Through every keyword and back: nothing happened.
        (org-todo "SOMEDAY") (org-todo "DONE") (org-todo "TODO")
        (org-varde-test--settle)
        (should-not (org-varde-test--acts))
        ;; Past SOMEDAY to DONE: closed, never parked.
        (org-todo "SOMEDAY") (org-todo "DONE")
        (org-varde-test--settle)
        (should (equal (org-varde-test--acts) '("done AAA111 --json")))))))

(ert-deftest org-varde-a-repeating-heading-does-not-close-its-task ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-with-org "* TODO Weekly review\nSCHEDULED: <2026-10-12 Mon +1w>\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-test--with-held-hook
        ;; Org logs the repeat from `post-command-hook', which no command
        ;; loop here would run: left there, it fires in a later test.
        (let ((post-command-hook nil))
          (org-todo 'done))
        (should (equal (org-get-todo-state) "TODO"))
        (org-varde-test--settle)
        (should-not (org-varde-test--acts))))))

(ert-deftest org-varde-board-done-closes-the-task-of-a-repeating-heading ()
  "Asked for on the board, the task closes; the heading repeats."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* TODO Weekly review\nSCHEDULED: <2026-10-12 Mon +1w>\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-test--with-held-hook
        (org-varde-done-task "AAA111" "Weekly review")
        (org-varde-test--settle))
      (should (equal (org-varde-test--acts) '("done AAA111 --json")))
      (goto-char (point-min))
      (should (equal (org-get-todo-state) "TODO")))))

(ert-deftest org-varde-board-done-asks-about-a-busy-session-at-once ()
  "Asked as you press the key, before the heading changes, so keys typed
after it cannot answer it; declining leaves the file as it was."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* TODO Busy one\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (let ((org-varde-board-data
             '(:groups ((:key "working" :rows ((:task "AAA111" :status "working"))))))
            (asked nil))
        (org-varde-test--with-held-hook
          (cl-letf (((symbol-function 'y-or-n-p)
                     (lambda (&rest _) (setq asked (org-get-todo-state)) nil)))
            (org-varde-done-task "AAA111" "Busy one"))
          (should (equal asked "TODO"))
          (should-not org-varde--pending)
          (should-not (org-varde-test--acts))
          (should-not (buffer-modified-p))
          (should (string-match-p "^\\* TODO Busy one" (org-varde-test--saved file)))
          ;; Agreed to, it closes now, and once.
          (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t)))
            (org-varde-done-task "AAA111" "Busy one"))
          (should-not org-varde--pending)
          (should (equal (org-varde-test--acts) '("done AAA111 --json")))
          (should (string-match-p "^\\* DONE Busy one" (org-varde-test--saved file))))))))

(ert-deftest org-varde-a-failed-close-sets-the-heading-back ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-fail-once "done" "varde: the store is locked")
    (org-varde-test-with-org "* TODO Close me\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-test--with-held-hook
        (org-todo "DONE")
        (org-varde-test--settle)
        (org-varde-test-wait))
      (should (member "done AAA111 --json" (org-varde-test-calls)))
      (should (equal (org-get-todo-state) "TODO")))))

;;; --- Review findings -------------------------------------------------------

(ert-deftest org-varde-board-done-on-a-done-heading-still-closes-the-task ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* DONE Already done\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-done-task "AAA111" "Already done")
      (should (member "done AAA111 --json" (org-varde-test-calls))))))

(ert-deftest org-varde-board-done-leaves-an-archived-heading-as-it-is ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* TODO Old :ARCHIVE:\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-done-task "AAA111" "Old")
      (should (member "done AAA111 --json" (org-varde-test-calls)))
      (should (equal (org-get-todo-state) "TODO"))
      (should (string-match-p "^\\* TODO Old" (org-varde-test--saved file))))))

(ert-deftest org-varde-linking-in-a-narrowed-buffer-links-the-heading-picked ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\",\"title\":\"t\"}")
    (org-varde-test-with-org "* TODO A\n* TODO B\n"
      (let ((b (save-excursion (re-search-forward "^\\* TODO B") (line-beginning-position))))
        (org-varde-test--goto "A")
        (org-narrow-to-subtree)
        (cl-letf (((symbol-function 'org-varde--pick-heading)
                   (lambda (_) (list "B" file nil b))))
          (org-varde-link-task "AAA111"))
        (widen)
        (should-not (org-entry-get (point-min) "VARDE_TASK"))
        (goto-char b)
        (should (equal (org-entry-get nil "VARDE_TASK") "AAA111"))))))

(ert-deftest org-varde-board-done-asks-for-no-note ()
  "A board key acts on a row, away from the heading: with `org-log-done'
set to note, the time is logged and no note is asked for."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* TODO Noted\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (let ((org-log-done 'note)
            (org-log-into-drawer nil))
        ;; Org asks for a note from `post-command-hook'.
        (let ((post-command-hook nil))
          (org-varde-done-task "AAA111" "Noted")
          (should-not (memq 'org-add-log-note post-command-hook)))
        (org-varde-test--goto "Noted")
        (should (equal (org-get-todo-state) "DONE"))
        (should (org-entry-get nil "CLOSED"))))))

(ert-deftest org-varde-board-done-saves-the-done-heading ()
  "Closed from the board, a heading's DONE is saved: the buffer is out of
sight, and a restart would lose it."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* TODO Close me\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-done-task "AAA111" "Close me")
      (should-not (buffer-modified-p))
      (should (string-match-p "^\\* DONE Close me"
                              (org-varde-test--saved file))))))

(ert-deftest org-varde-board-done-leaves-your-own-edits-unsaved ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test-with-org "* TODO Close me\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (goto-char (point-max))
      (insert "an edit of my own")
      (org-varde-done-task "AAA111" "Close me")
      (should (buffer-modified-p))
      (should (string-match-p "^\\* TODO Close me"
                              (org-varde-test--saved file))))))

(ert-deftest org-varde-archived-means-the-archive-tag-not-the-word ()
  (let* ((dir (make-temp-file "org-varde-arch-" t))
         (f (expand-file-name "p.org" dir))
         (org-directory dir)
         (org-agenda-files (list f "/nonexistent/missing.org")))
    (unwind-protect
        (progn
          (with-temp-file f
            (insert "#+TODO: TODO | FINISHED\n"
                    "* TODO Fix :archive: parser\n:PROPERTIES:\n:VARDE_TASK: WORD11\n:END:\n"
                    "* Old stuff :ARCHIVE:\n** TODO Child\n:PROPERTIES:\n:VARDE_TASK: CHILD2\n:END:\n"
                    "* FINISHED Custom done\n:PROPERTIES:\n:VARDE_TASK: DONE33\n:END:\n"))
          ;; A missing agenda file is skipped, not an error.
          (should (equal (mapcar (lambda (l) (list (car l) (nth 3 l) (nth 4 l))) (org-varde--linked))
                         '(("WORD11" nil nil) ("CHILD2" nil t) ("DONE33" t nil)))))
      (delete-directory dir t))))

(ert-deftest org-varde-json-reads-the-same-without-native-json ()
  "Emacs 29 can be built without libjansson."
  (let ((text "{\"v\":2,\"a\":[1,null,false],\"b\":{\"c\":true,\"d\":\"x\"}}"))
    (should (equal (org-varde--json text) '(:v 2 :a (1 nil nil) :b (:c t :d "x"))))
    (cl-letf (((symbol-function 'json-available-p) #'ignore))
      (should (equal (org-varde--json text) '(:v 2 :a (1 nil nil) :b (:c t :d "x")))))))

(ert-deftest org-varde-watches-the-store-varde-uses ()
  (let ((process-environment (append '("VARDE_HOME" "XDG_STATE_HOME=relative/state") process-environment)))
    (should (equal (org-varde--home) (expand-file-name "~/.local/state/varde/")))
    (setenv "XDG_STATE_HOME" "/xdg")
    (should (equal (org-varde--home) "/xdg/varde/"))
    (setenv "VARDE_HOME" "/v")
    (should (equal (org-varde--home) "/v/"))))

(ert-deftest org-varde-only-store-writes-refresh-the-board ()
  (let (refreshed)
    (cl-letf (((symbol-function 'org-varde-refresh) (lambda () (setq refreshed t))))
      ;; What every `varde list' does to the directory: no refresh.
      (dolist (e '((1 created "/h/varde.db-wal") (1 created "/h/varde.db-shm")
                   (1 changed "/h/varde.db-shm") (1 deleted "/h/varde.db-wal")
                   (1 changed "/h/pending.jsonl")))
        (org-varde--on-store-change e))
      (should-not refreshed)
      (org-varde--on-store-change '(1 changed "/h/varde.db-wal"))
      (should refreshed))))

;;; --- Reconciling in the background ------------------------------------------

(defconst org-varde-test--three
  (concat "* TODO First\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
          "* TODO Not linked\n"
          "* TODO Second\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"
          "* TODO Third\n:PROPERTIES:\n:VARDE_TASK: CCC333\n:END:\n")
  "Three linked headings and an unlinked one, in one file.")

(defun org-varde-test--states ()
  "Each heading's keyword, top down."
  (org-map-entries #'org-get-todo-state))

(defun org-varde-test--rows (key state &rest tasks)
  "A board with TASKS in group KEY, in STATE, changed in 2100."
  (format "{\"v\":2,\"groups\":[{\"key\":\"%s\",\"rows\":[%s]}]}" key
          (mapconcat (lambda (task)
                       (format "{\"task\":\"%s\",\"state\":\"%s\",\"status\":\"%s\",\"last\":1,\"changed\":4102444800000}"
                               task state state))
                     tasks ",")))

(ert-deftest org-varde-reconciling-headings-that-move-sets-each-one ()
  "Setting one heading adds or removes its CLOSED line, which moves the
headings below it: each is still the one set, and nothing else is."
  (org-varde-test-with-varde
    (let ((org-log-done 'time))
      (org-varde-test-reply "list" (org-varde-test--rows "done" "done" "AAA111" "BBB222" "CCC333"))
      (org-varde-test-with-org org-varde-test--three
        (save-buffer)
        (org-varde-reconcile)
        (org-varde-test-calls)
        (should (equal (org-varde-test--states) '("DONE" "TODO" "DONE" "DONE")))
        (should-not (buffer-modified-p))
        ;; And back: each loses its CLOSED line.
        (org-varde-test-reply "list" (org-varde-test--rows "open" "open" "AAA111" "BBB222" "CCC333"))
        (org-varde-reconcile)
        (org-varde-test-calls)
        (should (equal (org-varde-test--states) '("TODO" "TODO" "TODO" "TODO")))
        (should-not (string-match-p "CLOSED" (buffer-string)))))))

(ert-deftest org-varde-headings-closed-elsewhere-in-one-file-all-follow ()
  (org-varde-test-with-varde
    (let ((org-log-done 'time))
      (org-varde-test-with-org org-varde-test--three
        (save-buffer)
        (let ((org-varde-board-data
               (org-varde--json (org-varde-test--rows "done" "done" "AAA111" "BBB222" "CCC333"))))
          (org-varde--follow-closed)
          (should (equal (org-varde-test--states) '("DONE" "TODO" "DONE" "DONE"))))))))

(ert-deftest org-varde-an-unsaved-edit-elsewhere-does-not-close-a-reopened-task ()
  "Typing in the file makes no heading in it newer than the task."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"open\",\"status\":\"waiting\",\"last\":1,\"changed\":1790000000000}]}]}")
    (org-varde-test-with-org "* DONE Finished once\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n* Notes\n"
      (save-buffer)
      (set-file-times file (seconds-to-time 1780000000))
      (goto-char (point-max))
      (insert "typing\n")
      (org-varde-reconcile)
      (should (equal (org-varde-test-calls) '("list --json --all")))
      (goto-char (point-min))
      (should (equal (org-get-todo-state) "TODO")))))

(defun org-varde-test--kill-visiting (files)
  "Kill the buffers visiting FILES, unsaved edits and all."
  (dolist (f files)
    (let ((b (find-buffer-visiting f)))
      (when b (with-current-buffer b (set-buffer-modified-p nil)) (kill-buffer b)))))

(ert-deftest org-varde-a-heading-that-cannot-be-written-leaves-the-rest ()
  "One read-only buffer is said once, the other headings still follow,
and its task is tried again on the next refresh."
  (org-varde-test-with-varde
    (let* ((locked (expand-file-name "a.org" org-directory))
           (open (expand-file-name "b.org" org-directory))
           (said nil))
      (with-temp-file locked (insert "* TODO Locked\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"))
      (with-temp-file open (insert "* TODO Open\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"))
      (with-current-buffer (find-file-noselect locked) (read-only-mode 1))
      (unwind-protect
          (progn
            (cl-letf (((symbol-function 'message)
                       (lambda (fmt &rest args) (push (apply #'format fmt args) said))))
              (let ((org-varde-board-data
                     (org-varde--json (org-varde-test--rows "done" "done" "AAA111" "BBB222"))))
                (org-varde--follow-closed)
                (should (equal org-varde--closed '("BBB222")))
                (org-varde--follow-closed)))
            (should (equal (seq-filter (lambda (m) (string-match-p "could not set" m)) said)
                           (list (format "org-varde: could not set the heading of aaa111 in %s: Buffer is read-only: #<buffer a.org>"
                                         locked))))
            (with-current-buffer (find-buffer-visiting open)
              (goto-char (point-min))
              (should (equal (org-get-todo-state) "DONE"))))
        (org-varde-test--kill-visiting (list locked open))))))

(ert-deftest org-varde-a-file-a-sync-changed-is-reread-without-asking ()
  "A buffer without edits is reverted; one with edits is left, said once."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" (org-varde-test--rows "done" "done" "AAA111" "BBB222"))
    (let ((clean (expand-file-name "a.org" org-directory))
          (edited (expand-file-name "b.org" org-directory))
          (said nil))
      (with-temp-file clean (insert "* TODO Synced\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"))
      (with-temp-file edited (insert "* TODO Edited\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n"))
      (find-file-noselect clean)
      (with-current-buffer (find-file-noselect edited) (goto-char (point-max)) (insert "mine\n"))
      (unwind-protect
          (progn
            ;; Another machine writes both files.
            (dolist (f (list clean edited))
              (let ((text (with-temp-buffer (insert-file-contents f) (buffer-string))))
                (with-temp-file f (insert "* TODO Laptop\n" text)))
              (set-file-times f (time-add nil 10)))
            (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) (error "Asked")))
                      ((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked")))
                      ((symbol-function 'ask-user-about-supersession-threat) (lambda (&rest _) (error "Asked")))
                      ((symbol-function 'message) (lambda (fmt &rest args) (push (apply #'format fmt args) said))))
              (org-varde-reconcile)
              (org-varde-test-calls)
              (org-varde--reconcile (plist-get (org-varde--json (org-varde-test--rows "done" "done" "BBB222"))
                                               :groups)))
            (should (string-match-p "\\`\\* TODO Laptop\n\\* DONE Synced" (org-varde-test--saved clean)))
            (should (equal (with-current-buffer (find-buffer-visiting edited) (org-varde-test--states)) '("TODO")))
            (should (= 1 (seq-count (lambda (m) (string-match-p "b.org changed on disk and has unsaved edits" m))
                                    said))))
        (org-varde-test--kill-visiting (list clean edited))))))

(ert-deftest org-varde-quitting-the-stop-question-sets-the-heading-back ()
  "C-g at the question counts as no, so no later reconcile closes the task."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-with-org "* TODO Busy one\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (save-buffer)
      (let ((org-varde-board-data '(:groups ((:key "working" :rows ((:task "AAA111" :status "working")))))))
        (org-varde-test--with-held-hook
          (org-todo "DONE")
          (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) (signal 'quit nil))))
            (should (eq 'quit (condition-case nil (org-varde-test--settle) (quit 'quit)))))))
      (should (equal (org-get-todo-state) "TODO"))
      (should-not (org-varde-test--acts)))))

(ert-deftest org-varde-board-done-writes-the-log-line-before-it-saves ()
  "A keyword that logs, or a repeat, logs its time at once, so the save
has it; no note is asked for."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (let ((org-log-done 'time) (org-log-repeat 'note) (org-log-into-drawer nil))
      (org-varde-test-with-org (concat "#+TODO: TODO(t) | DONE(d@)\n"
                                       "* TODO Noted\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
                                       "* TODO Weekly\nSCHEDULED: <2026-10-12 Mon +1w>\n:PROPERTIES:\n:VARDE_TASK: BBB222\n:END:\n")
        (org-mode)
        (save-buffer)
        (let ((post-command-hook nil))
          (org-varde-done-task "AAA111" "Noted")
          (org-varde-done-task "BBB222" "Weekly")
          (should-not (memq 'org-add-log-note post-command-hook)))
        (should-not (buffer-modified-p))
        (let ((saved (org-varde-test--saved file)))
          (should (string-match-p "^\\* DONE Noted\nCLOSED: .*\n\\(?:.*\n\\)*?- State \"DONE\" +from \"TODO\" +\\[" saved))
          (should (string-match-p "^\\* TODO Weekly\n\\(?:.*\n\\)*?- State \"DONE\" +from \"TODO\" +\\[" saved)))))))

(ert-deftest org-varde-a-keyword-set-quietly-is-not-followed-by-a-local-hook ()
  "A buffer-local todo hook still runs the global one."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" (org-varde-test--rows "done" "done" "AAA111"))
    (org-varde-test-with-org "* TODO X\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (save-buffer)
      (add-hook 'org-after-todo-state-change-hook #'ignore nil t)
      (org-varde-test--with-held-hook
        (org-varde-reconcile)
        (org-varde-test-calls)
        (should (equal (org-get-todo-state) "DONE"))
        (should-not org-varde--pending)))))

(ert-deftest org-varde-steady-store-writes-still-refresh ()
  "Refreshes asked for every 0.1 s fetch the board at least once a second."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[]}")
    (let ((start (float-time)))
      (while (< (- (float-time) start) 1.5)
        (org-varde--on-store-change (list nil 'changed (expand-file-name "varde.db-wal" org-varde-test-dir)))
        (accept-process-output nil 0.1))
      (should (seq-find (lambda (c) (string-prefix-p "list" c)) (org-varde-test-calls))))))

(ert-deftest org-varde-a-newer-board-is-said-once-and-not-reconciled ()
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":3,\"groups\":[{\"key\":\"done\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"done\"}]}]}")
    (let ((said 0))
      (cl-letf (((symbol-function 'message)
                 (lambda (fmt &rest args)
                   (when (string-match-p "newer" (apply #'format fmt args)) (setq said (1+ said))))))
        (dotimes (_ 3) (org-varde--list) (org-varde-test-wait)))
      (should (= said 1))
      (should (equal org-varde--too-new 3)))
    (org-varde-test-with-org "* TODO X\n:PROPERTIES:\n:VARDE_TASK: AAA111\n:END:\n"
      (org-varde-reconcile)
      (org-varde-test-calls)
      (should (equal (org-get-todo-state) "TODO"))
      (should-error (org-varde--unlinked) :type 'user-error))))

;;; --- Memory ----------------------------------------------------------------

(ert-deftest org-varde-an-edited-cairn-goes-back-on-stdin ()
  (org-varde-test-with-varde
    (org-varde-test-reply "edit" "Status: \nFindings:\nDecisions:\nOpen:\n")
    (org-varde-edit-cairn org-varde-test--id)
    (should (member (format "edit %s --print" org-varde-test--id) (org-varde-test-calls)))
    (with-current-buffer "*cairn xyz123*"
      (goto-char (point-max))
      (insert "- My own line\n")
      (org-varde-cairn-save))
    (should (member (format "edit %s --stdin" org-varde-test--id) (org-varde-test-calls)))
    (should (string-suffix-p "Open:\n- My own line\n" (org-varde-test-stdin "edit")))))

(ert-deftest org-varde-a-project-cairn-is-shown-and-edited ()
  (org-varde-test-with-varde
    (let ((dir (file-name-as-directory org-varde-test-dir)))
      (org-varde-test-reply "show" "project cairn  /p\n")
      (save-window-excursion (org-varde-show-project dir))
      (should (member (format "show --project %s" dir) (org-varde-test-calls)))
      (with-current-buffer "*varde project*"
        (should (eq (key-binding "e") #'org-varde-edit-project-cairn))
        (org-varde-test-reply "edit" "Conventions:\nGotchas:\nHow-to:\n")
        (save-window-excursion (org-varde-edit-project-cairn)))
      (should (member (format "edit --project %s --print" dir) (org-varde-test-calls)))
      (with-current-buffer (format "*project cairn %s*" (file-name-nondirectory (directory-file-name dir)))
        (goto-char (point-max))
        (insert "- `make` builds it\n")
        (org-varde-cairn-save))
      (should (member (format "edit --project %s --stdin" dir) (org-varde-test-calls)))
      (should (string-suffix-p "How-to:\n- `make` builds it\n" (org-varde-test-stdin "edit"))))))

(provide 'org-varde-test)
;;; org-varde-test.el ends here
