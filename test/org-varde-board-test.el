;;; org-varde-board-test.el --- Tests for the board -*- lexical-binding: t; -*-

;;; Code:

(require 'ert)
(require 'test-helper)
(require 'org-varde-board)

(defun org-varde-board-test--data (rows-by-group)
  "A board as `varde list --json' returns it, from ROWS-BY-GROUP."
  (org-varde--json
   (json-encode
    `((v . 2)
      (groups . ,(vconcat
                  (mapcar (lambda (g)
                            `((key . ,(car g)) (label . ,(cadr g))
                              (rows . ,(vconcat
                                        (mapcar (lambda (r)
                                                  `((task . ,(car r)) (short . ,(downcase (substring (car r) -6)))
                                                    (title . ,(cadr r)) (status . ,(nth 2 r))
                                                    (project . "/home/u/proj") (inbox . :json-false)
                                                    (external . ,(nth 3 r)) (last . 0)
                                                    (agents . ,(or (nth 4 r) 0)) (shells . ,(or (nth 5 r) 0))
                                                    (git . ,(nth 6 r)) (unread . ,(nth 7 r))))
                                                (cddr g))))))
                          rows-by-group)))))))

(defun org-varde-board-test--first-row ()
  "Move to the board's first line under its first group heading."
  (goto-char (point-min))
  (forward-line 1))

(defmacro org-varde-board-test--with (data &rest body)
  (declare (indent 1))
  `(let ((org-varde-board-data ,data))
     (unwind-protect
         (with-current-buffer (get-buffer-create org-varde-board-buffer)
           (org-varde-board-mode)
           (org-varde-board--render)
           ,@body)
       (kill-buffer org-varde-board-buffer))))

;; A session you have looked at and left reads as hollow, like one at rest.
(ert-deftest org-varde-board-what-you-have-not-seen-is-filled ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you"
          ("T0000000000000000000AAA111" "New" "waiting" nil 0 0 nil t)
          ("T0000000000000000000BBB222" "Seen" "waiting" nil 0 0 nil :json-false))))
    (let ((text (buffer-string)))
      (should (string-match-p "^  ● waiting .*New$" text))
      (should (string-match-p "^  ○ waiting .*Seen$" text)))))

(ert-deftest org-varde-board-renders-what-varde-grouped ()
  (org-varde-board-test--with
      (org-varde-board-test--inbox
       (org-varde-board-test--data
        '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "Fix login" "waiting" "org"))
          ("working" "Working")
          ("inbox" "Inbox" ("T0000000000000000000BBB222" "Ad hoc question" "exited" nil)))))
    (let ((text (buffer-string)))
      (should (string-match-p "^Needs you  1$" text))
      (should (string-match-p "^Working  0$" text))
      (should (string-match-p "^  ○ waiting +[0-9]+[smhd]  proj +Fix login$" text))
      ;; Only what helps you choose is shown: no ids, no markers.
      (should (string-match-p "  proj +Ad hoc question$" text))
      (should-not (string-match-p "aaa111" text)))
    (org-varde-board-test--first-row)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    (should (eq (get-text-property (point) 'face) nil))))

(ert-deftest org-varde-board-shows-where-the-git-work-stands ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("open" "Open"
          ("T0000000000000000000AAA111" "Review" "open" "org" 0 0
           ((stage . "pr") (label . "PR #12 ✗") (checks . "failing")))
          ("T0000000000000000000BBB222" "Quiet" "open" "org" 0 0 nil))))
    (let ((text (buffer-string)))
      (should (string-match-p "^  ○ open      PR #12 ✗  +[0-9]+[smhd]  proj +Review$" text))
      (should (string-match-p "^  ○ open      +[0-9]+[smhd]  proj +Quiet$" text)))
    (goto-char (point-min))
    (search-forward "PR #12")
    (should (eq (get-text-property (point) 'face) 'org-varde-board-blocked))))

(ert-deftest org-varde-board-throws-work-away-only-when-you-confirm ()
  (org-varde-test-with-varde
    (org-varde-test-reply "drop" "{\"v\":1,\"worktree\":{\"action\":\"discarded\",\"saved\":\"refs/varde/discarded/x\"}}")
    (org-varde-test-reply "show" "{\"v\":2,\"state\":\"dropped\",\"worktree\":{\"state\":\"active\"}}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Drop me" "open" nil))))
      (org-varde-board-test--first-row)
      ;; Work is at stake, so the question takes a full yes.
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) nil))
                ((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked too lightly"))))
        (org-varde-board-throw-away))
      (should-not (member "drop T0000000000000000000AAA111 --json" (org-varde-test-calls)))
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) t)))
        (org-varde-board-throw-away))
      (should (member "drop T0000000000000000000AAA111 --json" (org-varde-test-calls)))
      ;; It is put away, not closed again as done.
      (should-not (seq-find (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls))))))

(ert-deftest org-varde-board-new-asks-where-offering-the-task-at-point ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (org-varde-board-test--first-row)
      (let (offered)
        (cl-letf (((symbol-function 'read-directory-name)
                   (lambda (_prompt dir &rest _) (setq offered dir) "/home/u/other/")))
          (call-interactively #'org-varde-board-new))
        (should (equal offered "/home/u/proj/"))
        (should (equal (car org-varde-test-terminals) '("/home/u/other/" "*claude other*")))))))

;; Write NOTE in the *varde new* buffer that `n' opened, and create it
;; there, answering the project question with DIR.  Return what was offered.
(defun org-varde-board-test--write (note dir)
  (let (offered)
    (with-current-buffer "*varde new*"
      (should (derived-mode-p 'org-varde-new-mode))
      (insert note)
      (cl-letf (((symbol-function 'read-directory-name)
                 (lambda (_prompt offer &rest _) (setq offered offer) (or dir offer))))
        (call-interactively #'org-varde-new-create)))
    offered))

(ert-deftest org-varde-board-create-writes-the-task-in-a-buffer ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (org-varde-board-test--first-row)
      ;; The project is asked for even on a task, offering its project.
      (call-interactively #'org-varde-board-create)
      (should (equal (org-varde-board-test--write "\nCache by locale\n\nStart in cache.rs\n  then the tests\n" nil)
                     "/home/u/proj/"))
      (should-not (get-buffer "*varde new*"))
      (should (member "new --json --project /home/u/proj/ --body - -- Cache by locale" (org-varde-test-calls)))
      (should (equal (org-varde-test-stdin "new") "Start in cache.rs\n  then the tests"))
      ;; No context, no body.  No task at point: nothing is offered.
      (with-current-buffer org-varde-board-buffer
        (goto-char (point-min))
        (call-interactively #'org-varde-board-create))
      (should-not (org-varde-board-test--write "Bare" "/home/u/other/"))
      (should (member "new --json --project /home/u/other/ -- Bare" (org-varde-test-calls)))
      ;; A blank title is refused before the project is asked for, and
      ;; what was written is kept.
      (call-interactively #'org-varde-board-create)
      (with-current-buffer "*varde new*"
        (insert " \n\n")
        (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) (error "Asked too soon"))))
          (should-error (call-interactively #'org-varde-new-create) :type 'user-error)))
      (call-interactively #'org-varde-board-create)
      (with-current-buffer "*varde new*"
        (should (equal (buffer-string) " \n\n"))
        (kill-buffer))
      (should-not org-varde-test-terminals))))

(ert-deftest org-varde-board-moves-the-task-at-point-to-another-project ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (org-varde-board-test--first-row)
      (org-varde-test-reply "set" "moved aaa111 to /home/u/other")
      (let (offered)
        (cl-letf (((symbol-function 'read-directory-name)
                   (lambda (_prompt dir &rest _) (setq offered dir) "/home/u/other/")))
          (call-interactively #'org-varde-board-move))
        (should (equal offered "/home/u/proj/")))
      (should (member "set T0000000000000000000AAA111 --project /home/u/other" (org-varde-test-calls)))
      ;; varde's refusal is the message.
      (org-varde-test-fail-once "set" "varde: aaa111 has a live session, which belongs to /home/u/proj: end it first")
      (should (equal (cadr (should-error (org-varde-board-move "/home/u/other") :type 'user-error))
                     "varde: aaa111 has a live session, which belongs to /home/u/proj: end it first")))))

(ert-deftest org-varde-board-a-moved-task-takes-its-heading-along ()
  "A heading that names a project gets the new one, so its next start
does not move the task back."
  (org-varde-test-with-varde
    (org-varde-test-with-org (concat "* TODO Own\n:PROPERTIES:\n:VARDE_TASK: T0000000000000000000AAA111\n"
                                     ":VARDE_PROJECT: /srv/u/proj\n:END:\n"
                                     "* TODO Plain\n:PROPERTIES:\n:VARDE_TASK: T0000000000000000000BBB222\n:END:\n")
      (save-buffer)
      (org-varde-board-test--with
          (org-varde-board-test--data '(("open" "Open"
                                         ("T0000000000000000000AAA111" "Own" "open" "org")
                                         ("T0000000000000000000BBB222" "Plain" "open" "org"))))
        (org-varde-board-test--first-row)
        (org-varde-board-move "/srv/u/other/")
        (forward-line 1)
        (org-varde-board-move "/srv/u/other/"))
      (goto-char (point-min))
      (should (equal (org-entry-get nil "VARDE_PROJECT") "/srv/u/other"))
      (should-not (buffer-modified-p))
      (outline-next-heading)
      (should-not (org-entry-get nil "VARDE_PROJECT" t)))))

(ert-deftest org-varde-board-shows-what-a-session-left-running ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "Serve" "waiting" "org" 0 1))
         ("working" "Working" ("T0000000000000000000BBB222" "Fan out" "delegated" "org" 2 1))))
    (let ((text (buffer-string)))
      (should (string-match-p "^  ○ waiting .*Serve  1 shell$" text))
      (should (string-match-p "^  ● delegated .*Fan out  2 agents · 1 shell$" text)))))

(ert-deftest org-varde-board-keeps-point-on-its-task-as-rows-move ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil)
          ("T0000000000000000000BBB222" "Two" "waiting" nil))
         ("working" "Working")))
    (goto-char (point-min))
    (forward-line 2)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
    ;; Two starts working: it moves to another group.
    (setq org-varde-board-data
          (org-varde-board-test--data
           '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil))
             ("working" "Working" ("T0000000000000000000BBB222" "Two" "working" nil)))))
    (org-varde-board--render)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
    (should (string-match-p "Working" (save-excursion (forward-line -1)
                                                       (buffer-substring (point) (line-end-position)))))))

(ert-deftest org-varde-board-keeps-each-window-on-its-task ()
  "A board in a window you are not in must not jump to the top on refresh."
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil)
          ("T0000000000000000000BBB222" "Two" "waiting" nil)
          ("T0000000000000000000CCC333" "Three" "waiting" nil))))
    (let ((w (selected-window)))
      (set-window-buffer w (current-buffer))
      (goto-char (point-min))
      (forward-line 3)
      (set-window-point w (point))
      (with-temp-buffer (org-varde-board--render))
      (should (equal (get-text-property (window-point w) 'org-varde-task) "T0000000000000000000CCC333")))))

(ert-deftest org-varde-board-starts-the-task-on-the-line ()
  (org-varde-board-test--with
      (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil))))
    (org-varde-board-test--first-row)
    (let (called)
      (cl-letf (((symbol-function 'org-varde-start-task)
                 (lambda (task title arg) (setq called (list task title arg)))))
        (org-varde-board-start '(16)))
      (should (equal called '("T0000000000000000000AAA111" "One" (16))))
      (cl-letf (((symbol-function 'org-varde--show-text) (lambda (task _) (setq called task)))
                ((symbol-function 'org-varde--varde) #'ignore)
                ((symbol-function 'org-varde--varde-async) #'ignore)
                ((symbol-function 'pop-to-buffer) #'ignore))
        (call-interactively #'org-varde-show))
      (should (equal called "T0000000000000000000AAA111")))
    (goto-char (point-min))
    (should-error (org-varde-board-start) :type 'user-error)))

(ert-deftest org-varde-board-starts-every-task-in-the-region ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("open" "Open"
          ("T0000000000000000000AAA111" "One" "open" nil)
          ("T0000000000000000000BBB222" "Two" "open" nil)
          ("T0000000000000000000CCC333" "Three" "open" nil))))
    (org-varde-board-test--first-row)
    (transient-mark-mode 1)
    (push-mark (point) t t)
    (goto-char (point-max))
    (let (started said)
      (cl-letf (((symbol-function 'message) (lambda (&rest args) (setq said (apply #'format args))))
                ((symbol-function 'org-varde-start-task)
                 (lambda (task _title arg finished)
                   (should org-varde--background)
                   (push (list task arg) started)
                   ;; One that cannot start does not stop the rest.
                   (pcase task
                     ("T0000000000000000000BBB222" (user-error "Two is running in its terminal"))
                     ("T0000000000000000000CCC333" (funcall finished "no such worktree"))
                     (_ (funcall finished nil))))))
        (org-varde-board-start '(4)))
      (should (equal (nreverse started)
                     '(("T0000000000000000000AAA111" (4))
                       ("T0000000000000000000BBB222" (4))
                       ("T0000000000000000000CCC333" (4)))))
      (should (string-match-p "1 of 3.*Two is running.*no such worktree" said))
      (should-not (use-region-p)))))

;; Done is expanded by default: a region over it must not reopen its tasks.
(ert-deftest org-varde-board-region-start-skips-closed-tasks ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (let ((data (org-varde-board-test--data
                     '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil))
                       ("done" "Done" ("T0000000000000000000BBB222" "Two" "done" nil))
                       ("parked" "Parked" ("T0000000000000000000CCC333" "Three" "parked" nil))))))
          (dolist (g (plist-get data :groups) data)
            (dolist (r (plist-get g :rows))
              (plist-put r :state (plist-get g :key)))))
      (org-varde-board-test--first-row)
      (transient-mark-mode 1)
      (push-mark (point) t t)
      (goto-char (point-max))
      (let (started said)
        (cl-letf (((symbol-function 'message) (lambda (&rest args) (setq said (apply #'format args))))
                  ((symbol-function 'org-varde-start-task)
                   (lambda (task _title _arg finished) (push task started) (funcall finished nil))))
          (org-varde-board-start))
        (should (equal started '("T0000000000000000000AAA111")))
        (should (string-match-p "1 of 1; 2 closed or parked skipped" said))))))

(ert-deftest org-varde-board-moves-between-tasks-and-groups ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil))
         ("working" "Working")
         ("open" "Open" ("T0000000000000000000BBB222" "Two" "open" nil)
          ("T0000000000000000000CCC333" "Three" "open" nil))))
    (goto-char (point-min))
    (org-varde-board-next-task)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    ;; Skips the empty group and the blank lines between groups.
    (org-varde-board-next-task)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
    (org-varde-board-next-task 1)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000CCC333"))
    (should-error (org-varde-board-next-task) :type 'user-error)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000CCC333"))
    (org-varde-board-previous-task 2)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    (org-varde-board-next-group)
    (should (equal (get-text-property (point) 'org-varde-group) "working"))
    (org-varde-board-previous-group)
    (should (equal (get-text-property (point) 'org-varde-group) "needs_you"))
    (should (bobp))))

(ert-deftest org-varde-board-parked-opens-on-request ()
  (org-varde-board-test--with
      (org-varde--json
       "{\"v\":1,\"groups\":[{\"key\":\"open\",\"label\":\"Open\",\"collapsed\":false,\"rows\":[]},{\"key\":\"parked\",\"label\":\"Parked\",\"collapsed\":true,\"rows\":[{\"task\":\"T0000000000000000000AAA111\",\"short\":\"aaa111\",\"title\":\"Forge\",\"status\":\"parked\",\"project\":null,\"inbox\":false,\"external\":\"org\",\"last\":0}]}]}")
    (should (string-match-p "^Parked  1 …$" (buffer-string)))
    (should-not (string-match-p "Forge" (buffer-string)))
    (goto-char (point-min))
    (org-varde-board-next-group)
    (org-varde-board-toggle-group)
    (should (string-match-p "^  ‖ parked .*Forge$" (buffer-string)))
    ;; A refresh keeps it open.
    (org-varde-board--render)
    (should (string-match-p "Forge" (buffer-string)))
    (goto-char (point-min))
    (org-varde-board-next-group)
    (org-varde-board-toggle-group)
    (should-not (string-match-p "Forge" (buffer-string)))))

(ert-deftest org-varde-board-keeps-to-the-project-it-was-opened-from ()
  (let ((data (org-varde-board-test--data
               '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "Elsewhere" "waiting" nil))
                 ("open" "Open" ("T0000000000000000000BBB222" "Here" "open" nil))))))
    (plist-put (car (plist-get (car (plist-get data :groups)) :rows)) :project "/home/u/other")
    (org-varde-board-test--with data
      (let ((header (lambda () (substring-no-properties header-line-format))))
        (setq org-varde-board--project "/home/u/proj" org-varde-board--scoped t)
        (org-varde-board--render)
        (should (string-match-p "^Needs you  0$" (buffer-string)))
        (should-not (string-match-p "Elsewhere" (buffer-string)))
        (should (string-prefix-p " proj · 1 in other projects, 1 needs you (. all)   " (funcall header)))
        (should (eq (key-binding ".") #'org-varde-board-toggle-project))
        (org-varde-board-toggle-project)
        (should (string-match-p "Elsewhere" (buffer-string)))
        (should (string-prefix-p " every project (. only proj)" (funcall header)))
        ;; A project with no task on the board yet keeps to nothing.
        (setq org-varde-board--project "/home/u/new" org-varde-board--scoped t)
        (org-varde-board--render)
        (should (string-match-p "Elsewhere" (buffer-string)))
        (should (string-prefix-p " every project (none in new yet)" (funcall header)))
        (should-error (org-varde-board-toggle-project) :type 'user-error)
        (setq org-varde-board--project nil)
        (org-varde-board--render)
        (should-error (org-varde-board-toggle-project) :type 'user-error)))))

(ert-deftest org-varde-board-opens-on-the-project-of-the-buffer-it-is-called-from ()
  (org-varde-test-with-varde
    (let ((repo (expand-file-name "repo" org-varde-test-dir)))
      (make-directory (expand-file-name ".git" repo) t)
      (make-directory (expand-file-name "src" repo))
      (org-varde-test-reply "show" (format "{\"v\":1,\"project\":%S,\"version\":0}" repo))
      (cl-letf (((symbol-function 'org-varde-reconcile) #'ignore)
                ((symbol-function 'org-varde-refresh) #'ignore))
        (with-temp-buffer
          (setq default-directory (expand-file-name "src/" repo))
          (org-varde-board))
        (should (member (format "show --project %s/ --json" (expand-file-name "src" repo))
                        (org-varde-test-calls)))
        (with-current-buffer org-varde-board-buffer
          (should (equal org-varde-board--project repo))
          (should org-varde-board--scoped)
          ;; Called again from the board, it keeps what it keeps to.
          (setq org-varde-board--scoped nil)
          (org-varde-board)
          (should-not org-varde-board--scoped))
        (let ((org-varde-board-scope 'all))
          (with-temp-buffer
            (setq default-directory (expand-file-name "src/" repo))
            (org-varde-board)))
        (with-current-buffer org-varde-board-buffer
          (should (equal org-varde-board--project repo))
          (should-not org-varde-board--scoped))
        ;; Outside a repository there is no project to keep to.
        (org-varde-test-reply "show" (format "{\"v\":1,\"project\":%S,\"version\":0}" org-varde-test-dir))
        (with-temp-buffer
          (setq default-directory (file-name-as-directory org-varde-test-dir))
          (org-varde-board))
        (with-current-buffer org-varde-board-buffer
          (should-not org-varde-board--project))))))

(ert-deftest org-varde-board-renders-without-data ()
  (org-varde-board-test--with nil
    (should (string-match-p "Waiting for varde" (buffer-string)))))

(ert-deftest org-varde-board-marks-tasks-without-a-heading ()
  (org-varde-board-test--with
      (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Filed" "open" "org")
                                     ("T0000000000000000000BBB222" "Kept" "open" nil))))
    (should (string-match-p "  Filed$" (buffer-string)))
    (should (string-match-p " ◇ Kept$" (buffer-string))))
  ;; Nothing in the Inbox has a heading yet, so it carries no mark; an
  ;; Inbox task that needs you shows elsewhere, and is marked there.
  (org-varde-board-test--with
      (org-varde-board-test--inbox
       (org-varde-board-test--data '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "Asks" "waiting" nil))
                                     ("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil)))))
    (should (string-match-p " ◇ Asks$" (buffer-string)))
    (should (string-match-p "   Later$" (buffer-string)))))

(ert-deftest org-varde-board-says-when-varde-is-newer ()
  (let ((org-varde--too-new 3))
    (org-varde-board-test--with (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "A" "open" "org"))))
      (should (string-match-p "\\`This varde's board is v3, newer than org-varde reads" (buffer-string))))))

(defun org-varde-board-test--inbox (data &optional origin)
  "DATA with every row in the Inbox, deferred from ORIGIN when given."
  (dolist (g (plist-get data :groups) data)
    (dolist (r (plist-get g :rows))
      (plist-put r :inbox t)
      (when origin
        (plist-put r :origin (list :task origin :short (downcase (substring origin -6)) :title "Origin"))))))

(ert-deftest org-varde-board-names-where-deferred-work-came-from ()
  (org-varde-board-test--with
      (org-varde-board-test--inbox
       (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil))))
       "T0000000000000000000AAA111")
    (should (string-match-p "Later  ← aaa111$" (buffer-string)))))

(ert-deftest org-varde-board-files-only-a-task-without-a-heading ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--inbox
         (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil)
                                        ("T0000000000000000000AAA111" "Linked" "open" "org")))))
      (let (placed)
        (cl-letf (((symbol-function 'org-varde--place)
                   (lambda (task) (setq placed task) (with-current-buffer (get-buffer-create "*scratch*") (point-marker))))
                  ((symbol-function 'org-varde-refresh) #'ignore)
                  ((symbol-function 'org-varde--find-heading)
                   (lambda (task) (and (equal task "T0000000000000000000AAA111") '("x.org" . 1)))))
          (org-varde-board-test--first-row)
          (org-varde-board-file)
          (should (equal placed "T0000000000000000000BBB222"))
          (forward-line 1)
          (should-error (org-varde-board-file) :type 'user-error))))))

(ert-deftest org-varde-board-throws-away-a-task-without-work-on-a-y ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":2,\"state\":\"dropped\",\"worktree\":null}")
    (org-varde-test-reply "drop" "{\"v\":1,\"worktree\":null}")
    (org-varde-board-test--with
        (org-varde-board-test--inbox
         (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil)))))
      (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
        (org-varde-board-test--first-row)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) nil)))
          (org-varde-board-throw-away))
        (should-not (member "drop T0000000000000000000BBB222 --json" (org-varde-test-calls)))
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t)))
          (org-varde-board-throw-away))
        (should (member "drop T0000000000000000000BBB222 --json" (org-varde-test-calls)))))))

(ert-deftest org-varde-board-keeps-its-header-short ()
  (org-varde-board-test--with nil
    (let ((hints (split-string (substring-no-properties header-line-format) "  " t)))
      (should (<= (length hints) 6))
      (should (member "? more" (mapcar #'string-trim hints))))
    (should (eq (key-binding "?") #'org-varde-board-menu))))

(ert-deftest org-varde-board-starting-inbox-work-does-not-file-it ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--inbox
         (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil)))))
      (let (started)
        (cl-letf (((symbol-function 'org-varde--place) (lambda (_) (error "Filed")))
                  ((symbol-function 'org-varde-start-task) (lambda (task &rest _) (setq started task))))
          (org-varde-board-test--first-row)
          (org-varde-board-start)
          (should (equal started "T0000000000000000000BBB222")))))))

(ert-deftest org-varde-board-accepts-every-task-in-the-region ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--data
         '(("inbox" "Inbox"
            ("T0000000000000000000AAA111" "One" "exited" nil)
            ("T0000000000000000000BBB222" "Two" "exited" nil)
            ("T0000000000000000000CCC333" "Three" "exited" nil))))
      (org-varde-board-test--first-row)
      (org-varde-board-accept)
      (should (equal (org-varde-test-calls) '("accept T0000000000000000000AAA111")))
      ;; The region covers the first two rows, ending at the start of the third.
      (transient-mark-mode 1)
      (push-mark (point) t t)
      (forward-line 2)
      (org-varde-board-accept)
      (should (equal (cadr (org-varde-test-calls))
                     "accept T0000000000000000000AAA111 T0000000000000000000BBB222"))
      (should-not (use-region-p)))))

(ert-deftest org-varde-board-files-a-task-into-org ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":2,\"title\":\"why do indexers fail\",\"body\":null,\"origin\":null}")
    (org-varde-test-with-org "* Tasks\n** TODO Earlier\n* Other\n"
      (let ((file buffer-file-name))
        (save-buffer)
        (org-varde-board-test--with
            (org-varde-board-test--data
             '(("inbox" "Inbox" ("T0000000000000000000AAA111" "why do indexers fail" "exited" nil))))
          (org-varde-board-test--first-row)
          (cl-letf (((symbol-function 'org-refile-get-location)
                     (lambda (&rest _) (list "Tasks" file nil 1))))
            (org-varde-board-file)))
        (should (equal (buffer-string)
                       "* Tasks\n** TODO Earlier\n** TODO why do indexers fail\n:PROPERTIES:\n:VARDE_TASK: T0000000000000000000AAA111\n:END:\n* Other\n"))
        ;; The new heading was the file's only change, so it is saved.
        (should-not (buffer-modified-p))
        (should (equal (seq-take (org-varde-test-calls) 2)
                       '("show T0000000000000000000AAA111 --json --no-git"
                         "link T0000000000000000000AAA111 org")))
        ;; Now it has a heading, so it is not filed again.
        (org-varde-board-test--with
            (org-varde-board-test--data
             '(("open" "Open" ("T0000000000000000000AAA111" "why do indexers fail" "exited" "org"))))
          (org-varde-board-test--first-row)
          (should-error (org-varde-board-file) :type 'user-error))))))

(ert-deftest org-varde-board-links-a-task-to-an-existing-heading ()
  (org-varde-test-with-varde
    (org-varde-test-with-org "* Tasks\n** TODO Indexers\n** TODO Taken\n:PROPERTIES:\n:VARDE_TASK: T0000000000000000000ZZZ999\n:END:\n"
      (let ((file buffer-file-name))
        (save-buffer)
        (org-varde-board-test--with
            (org-varde-board-test--data
             '(("done" "Done" ("T0000000000000000000AAA111" "Indexers failing" "done" nil))))
          (org-varde-board-test--first-row)
          (cl-letf (((symbol-function 'org-refile-get-location)
                     (lambda (&rest _) (list "Taken" file nil 26))))
            (should-error (org-varde-board-link) :type 'user-error))
          (should-not (org-varde-test-calls))
          (cl-letf (((symbol-function 'org-refile-get-location)
                     (lambda (&rest _) (list "Indexers" file nil 9))))
            (org-varde-board-link)))
        (org-varde-test--goto "Indexers")
        (should (equal (org-entry-get nil "VARDE_TASK") "T0000000000000000000AAA111"))
        (should (equal (car (org-varde-test-calls)) "link T0000000000000000000AAA111 org"))))))

(ert-deftest org-varde-board-files-without-refile-targets ()
  "Vanilla Emacs has none: the agenda files' headings are offered."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":2,\"title\":\"New one\",\"body\":null,\"origin\":null}")
    (org-varde-test-with-org "* Tasks\n* Other\n"
      (save-buffer)
      (let ((org-refile-targets nil)
            ;; Batch frames are too narrow for file paths: plain heading paths.
            (org-refile-use-outline-path t)
            offered)
        (org-varde-board-test--with
            (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000AAA111" "New one" "exited" nil))))
          (org-varde-board-test--first-row)
          (cl-letf (((symbol-function 'completing-read)
                     (lambda (_prompt choices &rest _)
                       (setq offered (all-completions "" choices))
                       (seq-find (lambda (c) (string-match-p "^Tasks/" c)) offered))))
            (org-varde-board-file)))
        (should (seq-find (lambda (c) (string-match-p "^Other/" c)) offered))
        (should (string-match-p "^\\* Tasks\n\\*\\* TODO New one\n" (buffer-string)))))))

(ert-deftest org-varde-board-tab-on-a-row-folds-its-group ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil)
          ("T0000000000000000000BBB222" "Two" "waiting" nil))
         ("open" "Open" ("T0000000000000000000CCC333" "Three" "open" nil))))
    (goto-char (point-min))
    (forward-line 2)
    (org-varde-board-toggle-group)
    (should (string-match-p "^Needs you  2 …$" (buffer-string)))
    (should-not (string-match-p "One" (buffer-string)))
    (should (string-match-p "Three" (buffer-string)))
    (should (equal (get-text-property (point) 'org-varde-group) "needs_you"))
    (org-varde-board-toggle-group)
    (should (string-match-p "One" (buffer-string)))))

(ert-deftest org-varde-board-keys-follow-emacs-conventions ()
  "n makes a task, as on varde's own board; C-n and C-p move between tasks."
  (org-varde-board-test--with nil
    (should (eq (key-binding "n") #'org-varde-board-create))
    (should (eq (key-binding (kbd "C-n")) #'org-varde-board-next-task))
    (should (eq (key-binding (kbd "C-p")) #'org-varde-board-previous-task))
    (should (eq (key-binding "P") #'org-varde-board-park))
    (should (eq (key-binding "m") #'org-varde-board-move))
    (should (eq (key-binding "s") #'org-varde-show))
    (should (eq (key-binding "S") #'org-varde-show-project))
    (should (eq (key-binding "g") #'org-varde-refresh))
    (should-not (eq (key-binding (kbd "C-j")) #'org-varde-board-next-task))))

(ert-deftest org-varde-board-keys-under-evil ()
  "Run with evil on the load path to check this; skipped otherwise."
  (skip-unless (require 'evil nil t))
  (evil-mode 1)
  (unwind-protect
      (org-varde-board-test--with nil
        (evil-normal-state)
        (should (eq (key-binding "d") #'org-varde-board-done))
        (should (eq (key-binding "P") #'org-varde-board-park))
        (should (eq (key-binding (kbd "C-j")) #'org-varde-board-next-task))
        (should (eq (key-binding "gr") #'org-varde-refresh))
        (should (eq (key-binding "gk") #'org-varde-board-previous-group))
        (should (eq (key-binding "gg") #'evil-goto-first-line))
        (should (eq (key-binding "j") #'evil-next-line))
        (should (eq (key-binding "q") #'quit-window))
        (should (eq (key-binding "s") #'org-varde-show))
        (should (eq (key-binding "S") #'org-varde-show-project))
        ;; v and V are evil's selections, not the cairns.
        (should (eq (key-binding "v") #'evil-visual-char))
        (should (eq (key-binding "V") #'evil-visual-line)))
    (evil-mode -1)))

(ert-deftest org-varde-board-keys-under-evil-snipe ()
  "Run with evil-snipe on the load path to check this; skipped otherwise."
  (skip-unless (require 'evil-snipe nil t))
  (evil-mode 1)
  (evil-snipe-mode 1)
  (unwind-protect
      (org-varde-board-test--with nil
        (evil-normal-state)
        (should (eq (key-binding "s") #'org-varde-show))
        (should (eq (key-binding "S") #'org-varde-show-project)))
    (evil-snipe-mode -1)
    (evil-mode -1)))

(ert-deftest org-varde-board-selected-lines-under-evil ()
  "Run with evil on the load path to check this; skipped otherwise."
  (skip-unless (require 'evil nil t))
  (evil-mode 1)
  (unwind-protect
      (org-varde-board-test--with
          (org-varde-board-test--data
           '(("open" "Open"
              ("T0000000000000000000AAA111" "One" "open" nil)
              ("T0000000000000000000BBB222" "Two" "open" nil)
              ("T0000000000000000000CCC333" "Three" "open" nil))))
        (switch-to-buffer (current-buffer))
        (evil-normal-state)
        (org-varde-board-test--first-row)
        (let (started)
          (cl-letf (((symbol-function 'org-varde-start-task)
                     (lambda (task _title _arg finished) (push task started) (funcall finished nil))))
            (execute-kbd-macro (kbd "V j RET")))
          (should (equal (nreverse started)
                         '("T0000000000000000000AAA111" "T0000000000000000000BBB222")))
          (should (evil-normal-state-p))))
    (evil-mode -1)))

(provide 'org-varde-board-test)
;;; org-varde-board-test.el ends here
