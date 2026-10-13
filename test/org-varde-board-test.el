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
      ;; Needs you says it is your turn: its rows do not again.
      (should (string-match-p "^ ● New .*proj · " text))
      (should (string-match-p "^ ○ Seen .*proj · " text))
      (should-not (string-match-p "your turn" text)))))

(ert-deftest org-varde-board-renders-what-varde-grouped ()
  (org-varde-board-test--with
      (org-varde-board-test--inbox
       (org-varde-board-test--data
        '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "Fix login" "waiting" "org"))
          ("working" "Working")
          ("inbox" "Inbox" ("T0000000000000000000BBB222" "Ad hoc question" "exited" nil)))))
    (let ((text (buffer-string)))
      (should (string-match-p "^Needs you  1$" text))
      ;; An empty group is one quiet line, so nothing jumps as it fills.
      (should (string-match-p "^Needs you  1\n.*\nWorking  0\nInbox  1$" text))
      (should (eq (get-text-property (1+ (string-match "^Working" text)) 'face) 'org-varde-board-empty))
      ;; As the terminal board has it: the short id last, quiet.
      (should (string-match-p "^ ○ Fix login +proj · [0-9]+[smhd]  aaa111$" text))
      (should (string-match-p "^ ○ Ad hoc question +proj · [0-9]+[smhd]  bbb222$" text)))
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
      (should (string-match-p "^ ○ Review .*PR #12 ✗ · proj · [0-9]+[smhd]  aaa111$" text))
      (should (string-match-p "^ ○ Quiet .*proj · [0-9]+[smhd]  bbb222$" text)))
    (goto-char (point-min))
    (search-forward "PR #12")
    (should (eq (get-text-property (point) 'face) 'org-varde-board-blocked))))

(ert-deftest org-varde-board-drops-work-only-when-you-confirm ()
  (org-varde-test-with-varde
    (org-varde-test-reply "drop" "{\"v\":1,\"worktree\":{\"action\":\"discarded\",\"saved\":\"refs/varde/discarded/x\"}}")
    (org-varde-test-reply "show" "{\"v\":2,\"state\":\"dropped\",\"worktree\":{\"state\":\"active\"}}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Drop me" "open" nil))))
      (org-varde-board-test--first-row)
      ;; Work is at stake, so the question takes a full yes.
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) nil))
                ((symbol-function 'y-or-n-p) (lambda (&rest _) (error "Asked too lightly"))))
        (org-varde-board-drop))
      (should-not (member "drop T0000000000000000000AAA111 --json" (org-varde-test-calls)))
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) t)))
        (org-varde-board-drop))
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
      (should (equal (org-varde-board-test--write "Cache by locale\n\nStart in cache.rs\n  then the tests\n" nil)
                     "/home/u/proj/"))
      (should-not (get-buffer "*varde new*"))
      (should (member "new --json --project /home/u/proj/ --context - -- Cache by locale" (org-varde-test-calls)))
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
        ;; Put away, it comes back with n, however often; cleared, it is
        ;; dropped.
        (insert "Half a title")
        (org-varde-new-cancel)
        (should (get-buffer "*varde new*")))
      (call-interactively #'org-varde-board-create)
      (with-current-buffer "*varde new*"
        (should (equal (buffer-string) " \n\nHalf a title"))
        (org-varde-new-cancel))
      (call-interactively #'org-varde-board-create)
      (with-current-buffer "*varde new*"
        (should (equal (buffer-string) " \n\nHalf a title"))
        (erase-buffer)
        (org-varde-new-cancel))
      (should-not (get-buffer "*varde new*"))
      (should-not org-varde-test-terminals))))

(ert-deftest org-varde-new-here-makes-a-task-from-a-heading-and-links-it ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (let ((proj (expand-file-name "proj" org-varde-test-dir)))
      (make-directory proj)
      (org-varde-test-with-org (concat "#+PROPERTY: VARDE_PROJECT " proj "\n* Auth\n"
                                       "** TODO Fix the [[https://x/1][login]] [1/2] :work:\n"
                                       "SCHEDULED: <2026-10-12 Mon>\n:PROPERTIES:\n:ID: x\n:END:\n"
                                       "CLOCK: [2026-10-12 Mon 10:00]--[2026-10-12 Mon 11:00] =>  1:00\n"
                                       "It fails on CI only.\n*** A child\n")
        (re-search-forward "fails")
        (save-window-excursion (org-varde-new-here))
        (with-current-buffer "*varde new*"
          (should (string-prefix-p "Fix the login\n\nIt fails on CI only.\n\nFrom: [[file:" (buffer-string)))
          (should (string-match-p (concat "::\\*Fix the .*\\]\\[" (regexp-quote (file-name-nondirectory file))
                                          " › Auth › Fix the login.*\\]\\]\\'")
                                  (buffer-string)))
          ;; Its project is the heading's: nothing is asked.
          (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) (error "Asked"))))
            (call-interactively #'org-varde-new-create)))
        (should (member (concat "new --json --project " proj " --context - -- Fix the login") (org-varde-test-calls)))
        (should (string-match-p "\\`It fails on CI only\\.\n\nFrom: \\[\\[file:" (org-varde-test-stdin "new")))
        ;; Linked in the heading's text, below org's lines; nothing saved.
        (should (string-match-p "=>  1:00\n\\[\\[varde:new111\\]\\[Fix the login\\]\\]\nIt fails" (buffer-string)))
        (should (buffer-modified-p))
        (should-not (string-match-p "varde:" (with-temp-buffer (insert-file-contents file) (buffer-string))))))))

(ert-deftest org-varde-new-here-takes-a-region-and-outside-org-the-kill-ring ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (let* ((repo (expand-file-name "repo" org-varde-test-dir))
           (file (expand-file-name "notes.md" repo))
           (org-varde-board-data (org-varde--json (format "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"T1\",\"project\":%S}]}]}" repo))))
      (make-directory (expand-file-name ".git" repo) t)
      (org-varde-test-reply "show" (format "{\"v\":1,\"project\":%S}" repo))
      (with-temp-file file (insert "# Notes\nRetry the token refresh\nafter a 401.\nMore.\n"))
      (with-current-buffer (find-file-noselect file)
        (unwind-protect
            (progn
              (transient-mark-mode 1)
              (goto-char (point-min))
              (forward-line 1)
              (push-mark (point) t t)
              (forward-line 2)
              (save-window-excursion (org-varde-new-here))
              (with-current-buffer "*varde new*"
                (should (equal (buffer-string)
                               (format "Retry the token refresh\nafter a 401.\n\nFrom: [[file:%s::2][notes.md:2]]" file)))
                ;; Its repository has tasks: it goes there.
                (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) (error "Asked"))))
                  (call-interactively #'org-varde-new-create)))
              (should (member (concat "new --json --project " repo " --context - -- Retry the token refresh")
                              (org-varde-test-calls)))
              ;; Written as this buffer writes a link: plain text, bare.
              (should (equal (current-kill 0) "varde:new111"))
              (should-not (buffer-modified-p)))
          (kill-buffer))))
    ;; In org, the link goes below the region.
    (org-varde-test-with-org "* Notes\nRetry the token refresh\nafter a 401.\nMore.\n"
      (transient-mark-mode 1)
      (forward-line 1)
      (push-mark (point) t t)
      (forward-line 1)
      (end-of-line)
      (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
        (save-window-excursion (org-varde-new-here)))
      (org-varde-board-test--write "" "/tmp/elsewhere/")
      (should (equal (buffer-string)
                     "* Notes\nRetry the token refresh\nafter a 401.\n[[varde:new111][Retry the token refresh]]\nMore.\n")))))

;; A region's first line is a title: without a list item's bullet,
;; number or checkbox, and a heading's as org shows it.
(ert-deftest org-varde-new-here-titles-a-region-as-org-shows-it ()
  (org-varde-test-with-varde
    (dolist (case '(("- [ ] Retry the token refresh\n  after a 401.\n" "Retry the token refresh\n  after a 401.")
                    ("3. Retry it\n" "Retry it")
                    ("+ [X] Retry it\n" "Retry it")
                    ("** TODO [#A] Retry [[https://x.org][the token]] [1/2] :net:\nAfter a 401.\n"
                     "Retry the token\nAfter a 401.")
                    ;; From within a heading's own words: those marked.
                    ("** TODO Retry it now :net:\n" "it now :net:")))
      (org-varde-test-with-org (concat "* Notes\n" (car case))
        (transient-mark-mode 1)
        (forward-line 1)
        (when (string-suffix-p ":net:" (cadr case)) (search-forward "Retry "))
        (push-mark (point) t t)
        (goto-char (point-max))
        (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
          (save-window-excursion (org-varde-new-here)))
        (with-current-buffer "*varde new*"
          (should (string-prefix-p (concat (cadr case) "\n\nFrom: ") (buffer-string)))
          (set-buffer-modified-p nil)
          (kill-buffer))))))

(ert-deftest org-varde-new-here-keeps-a-draft-and-a-heading-s-old-link ()
  (org-varde-test-with-varde
    (save-window-excursion
      (org-varde--new nil nil)
      (with-current-buffer "*varde new*" (insert "Half a thought")))
    (org-varde-test-with-org "* Fix it\n"
      (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
        (save-window-excursion (org-varde-new-here)))
      (should (equal (current-kill 0) "Half a thought"))
      (with-current-buffer "*varde new*"
        (should (string-prefix-p "Fix it\n\nFrom: " (buffer-string)))))
    ;; A heading an earlier org-varde linked says how to convert it.
    (org-varde-test-with-org "* Fix it\n:PROPERTIES:\n:VARDE_TASK: 01K5ZZZZZZZZZZZZZZZZXYZ123\n:END:\n"
      (should (string-match-p "linked to xyz123 already.*org-varde-convert-links"
                              (cadr (should-error (org-varde-new-here) :type 'user-error)))))))

(defun org-varde-board-test--link-region (text from to)
  "Make a task from the region of org TEXT between the ends of the
searches FROM and TO, and return the text with its link."
  (org-varde-test-with-org text
    (transient-mark-mode 1)
    (search-forward from)
    (push-mark (point) t t)
    (search-forward to)
    (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
      (save-window-excursion (org-varde-new-here)))
    (org-varde-board-test--write "" "/tmp/elsewhere/")
    (buffer-string)))

(ert-deftest org-varde-new-here-never-puts-a-link-inside-org-s-structure ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    ;; Ending on a heading: below its planning and drawers.
    (should (equal (org-varde-board-test--link-region
                    "* TODO Fix the SSO thing\nSCHEDULED: <2026-10-14 Wed>\n:PROPERTIES:\n:ID: x\n:END:\nbody\n"
                    "Fix" "SSO")
                   "* TODO Fix the SSO thing\nSCHEDULED: <2026-10-14 Wed>\n:PROPERTIES:\n:ID: x\n:END:\n[[varde:new111][the SSO]]\nbody\n"))
    ;; Ending among them, the same.
    (should (string-match-p ":END:\n\\[\\[varde:new111\\]"
                            (org-varde-board-test--link-region
                             "* Fix\nSCHEDULED: <2026-10-14 Wed>\n:PROPERTIES:\n:ID: x\n:END:\nbody\n" "Fix" "<2026")))
    ;; In a table, a block, a drawer or a list: below it.
    (should (string-suffix-p "| b |\n[[varde:new111][| a]]\n\nMore.\n"
                             (org-varde-board-test--link-region "* N\n| a |\n| b |\n\nMore.\n" "* N\n" "| a")))
    (should (string-suffix-p "#+end_src\n[[varde:new111][x]]\n"
                             (org-varde-board-test--link-region "* N\nx\n#+begin_src sh\nls\n#+end_src\n" "* N\n" "ls")))
    (should (string-suffix-p ":END:\n[[varde:new111][Look]]\nAfter.\n"
                             (org-varde-board-test--link-region "* N\nLook\n:NOTES:\nin it\n:END:\nAfter.\n" "* N\n" "in it")))
    (should (string-suffix-p "- two\n[[varde:new111][one]]\n"
                             (org-varde-board-test--link-region "* N\n- one\n- two\n" "* N\n" "- one")))
    ;; The same with org's element cache off: parents are parsed then too.
    (let ((org-element-use-cache nil))
      (should (string-suffix-p "- two\n  more\n[[varde:new111][one]]\n"
                               (org-varde-board-test--link-region "* N\n- one\n- two\n  more\n" "* N\n" "more"))))
    ;; Whole lines: below the last of them.
    (should (equal (org-varde-board-test--link-region "* N\nOne\nTwo\nThree\n" "* N\n" "One\n")
                   "* N\nOne\n[[varde:new111][One]]\nTwo\nThree\n"))))

(ert-deftest org-varde-new-here-offers-the-task-a-heading-links ()
  (org-varde-test-with-varde
    (setq org-varde-board-data (org-varde-test--board '("aaa111" "waiting" "open") '("ddd444" "done" "done")))
    (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
      (let (asked went (answer t))
        (cl-letf (((symbol-function 'org-varde-go-task) (lambda (task &rest _) (setq went task)))
                  ((symbol-function 'y-or-n-p) (lambda (prompt) (setq asked prompt) answer)))
          (org-varde-test-with-org "* Fix it\n[[varde:ddd444]] [[varde:aaa111][Fix]]\n"
            (org-varde-new-here)
            (should (equal asked "This heading links \"Task aaa111\": go to it? (n makes a new task) "))
            (should (equal went "T00000000000000000000AAA111"))
            (should-not (get-buffer "*varde new*"))
            ;; n: a new task after all.
            (setq went nil answer nil)
            (save-window-excursion (org-varde-new-here))
            (should-not went)
            (should (get-buffer "*varde new*"))
            ;; The old command goes there without asking.
            (setq asked nil went nil)
            (with-suppressed-warnings ((obsolete org-varde-start)) (org-varde-start))
            (should-not asked)
            (should (equal went "T00000000000000000000AAA111")))
          ;; With no open task linked, it makes one.
          (org-varde-test-with-org "* Other\n[[varde:ddd444]]\n"
            (setq asked nil)
            (with-current-buffer "*varde new*" (set-buffer-modified-p nil))
            (save-window-excursion (with-suppressed-warnings ((obsolete org-varde-start)) (org-varde-start)))
            (should-not asked)
            (with-current-buffer "*varde new*" (should (string-prefix-p "Other\n" (buffer-string))))))))
    (should (eq (indirect-function 'org-varde-link) (indirect-function 'org-varde-insert-link)))))

(ert-deftest org-varde-new-here-drops-an-untouched-form-and-links-no-draft ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
      (org-varde-test-with-org "* Fix it\n"
        ;; Untouched, C-c C-k leaves nothing behind.
        (save-window-excursion (org-varde-new-here))
        (with-current-buffer "*varde new*" (org-varde-new-cancel))
        (should-not (get-buffer "*varde new*"))
        ;; Changed, it is kept as a draft, which n brings back as the
        ;; board's: made there, it links nothing here.
        (save-window-excursion (org-varde-new-here))
        (with-current-buffer "*varde new*" (insert " soon") (org-varde-new-cancel))
        (save-window-excursion (org-varde-board-create))
        (with-current-buffer "*varde new*"
          (should (string-prefix-p "Fix it soon\n" (buffer-string)))
          (should-not org-varde--new-then))
        (org-varde-board-test--write "" "/tmp/elsewhere/")
        (should (equal (buffer-string) "* Fix it\n"))))))

(ert-deftest org-varde-new-here-works-in-a-buffer-without-a-file ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (cl-letf (((symbol-function 'org-varde-board--project-of) #'ignore))
      (with-temp-buffer
        (transient-mark-mode 1)
        (insert "Retry the token refresh\nafter a 401.\n")
        (push-mark (point-min) t t)
        (save-window-excursion (org-varde-new-here))
        (with-current-buffer "*varde new*"
          ;; No From line: there is nowhere to go back to.
          (should (equal (buffer-string) "Retry the token refresh\nafter a 401.")))
        (org-varde-board-test--write "" "/tmp/elsewhere/")
        (should (equal (current-kill 0) "varde:new111"))))))

(ert-deftest org-varde-board-create-with-only-context-lets-varde-make-the-title ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (should (string-match-p "\\` C-c C-c create  C-c C-k later   line 1: title, then context\\'"
                              (progn (call-interactively #'org-varde-board-create)
                                     (with-current-buffer "*varde new*" (eval (cadr header-line-format) t)))))
      ;; A varde that wants a title says so, and the draft stays.
      (org-varde-test-fail-once "new" "varde: the following required arguments were not provided: <TITLE>...")
      (with-current-buffer "*varde new*"
        (insert "\nThe cache misses on every locale switch.")
        (let ((err (should-error (org-varde-new-create "/home/u/proj/") :type 'user-error)))
          (should (string-match-p "needs a title" (cadr err)))))
      (should (get-buffer "*varde new*"))
      ;; No title sent: the context makes one.
      (with-current-buffer "*varde new*" (org-varde-new-create "/home/u/proj/"))
      (should-not (get-buffer "*varde new*"))
      (should (equal (car (last (org-varde-test-calls))) "new --json --project /home/u/proj/ --context -"))
      (should (equal (org-varde-test-stdin "new") "The cache misses on every locale switch.")))))

(ert-deftest org-varde-board-create-on-a-board-kept-to-a-project-asks-nothing ()
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (setq org-varde-board--project "/home/u/proj" org-varde-board--scoped t)
      (call-interactively #'org-varde-board-create)
      (with-current-buffer "*varde new*"
        (insert "Straight in")
        (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) (error "Asked"))))
          (call-interactively #'org-varde-new-create)))
      (should (member "new --json --project /home/u/proj -- Straight in" (org-varde-test-calls)))
      ;; The board puts point on it once it shows it.
      (let ((org-varde-board-data (org-varde-board-test--data
                                   '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil)
                                      ("T0000000000000000000NEW111" "Straight in" "open" nil))))))
        (with-current-buffer org-varde-board-buffer
          (org-varde-board--render)
          (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000NEW111"))
          (should-not org-varde-board--select))))))

(ert-deftest org-varde-board-create-asks-where-in-a-project-without-tasks ()
  "A repository with no task, such as an org directory under git, is not
where a new task goes unasked."
  (org-varde-test-with-varde
    (org-varde-test-reply "new" "{\"v\":1,\"id\":\"T0000000000000000000NEW111\",\"short\":\"new111\"}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (setq org-varde-board--project "/home/u/notes" org-varde-board--scoped t)
      (call-interactively #'org-varde-board-create)
      (with-current-buffer "*varde new*"
        (insert "Somewhere")
        (cl-letf (((symbol-function 'read-directory-name) (lambda (&rest _) "/home/u/proj/")))
          (call-interactively #'org-varde-new-create)))
      (should (member "new --json --project /home/u/proj/ -- Somewhere" (org-varde-test-calls))))))

(ert-deftest org-varde-board-returns-to-its-own-workspace ()
  (org-varde-test-with-varde
    (let ((org-varde-workspaces t)
          (org-varde-board--home nil)
          (current "main")
          (spaces '("main" "xyz123 t")))
      (cl-letf (((symbol-function '+workspace-list-names) (lambda () spaces))
                ((symbol-function '+workspace-current-name) (lambda () current))
                ((symbol-function '+workspace-switch) (lambda (name &rest _) (setq current name)))
                ((symbol-function 'org-varde-board--project-of) #'ignore))
        (save-window-excursion (org-varde-board))
        (should (equal org-varde-board--home "main"))
        ;; From a task's workspace, back to its own.
        (setq current "xyz123 t")
        (save-window-excursion (org-varde-board))
        (should (equal current "main"))))))

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

(ert-deftest org-varde-board-shows-what-a-session-left-running ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "Serve" "waiting" "org" 0 1))
         ("working" "Working" ("T0000000000000000000BBB222" "Fan out" "delegated" "org" 2 1))))
    (let ((text (buffer-string)))
      (should (string-match-p "^ ○ Serve .*1 shell · " text))
      (should (string-match-p "^ ● Fan out .*working · 2 agents · 1 shell · " text)))))

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
      ;; RET goes to the session; s starts it and stays.
      (cl-letf (((symbol-function 'org-varde-start-task)
                 (lambda (task title arg)
                   (setq called (list task title arg org-varde--background)))))
        (org-varde-board-start '(16))
        (should (equal called '("T0000000000000000000AAA111" "One" (16) nil)))
        (org-varde-board-start-background)
        (should (equal called '("T0000000000000000000AAA111" "One" nil t))))
      (cl-letf (((symbol-function 'org-varde--show-fill) (lambda (task &rest _) (setq called task)))
                ((symbol-function 'pop-to-buffer) #'ignore))
        (call-interactively #'org-varde-show))
      (should (equal called "T0000000000000000000AAA111")))
    ;; On a group's heading, RET folds it.
    (goto-char (point-min))
    (org-varde-board-start)
    (should (string-match-p "^Open  1 ▸$" (buffer-string)))))

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
    ;; Skips the empty group.
    (org-varde-board-next-task)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
    (org-varde-board-next-task 1)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000CCC333"))
    (should-error (org-varde-board-next-task) :type 'user-error)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000CCC333"))
    (org-varde-board-previous-task 2)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    ;; The empty Working group has its line, but nothing to stop at.
    (should (string-match-p "^Working  0$" (buffer-string)))
    (org-varde-board-next-group)
    (should (equal (get-text-property (point) 'org-varde-group) "open"))
    (org-varde-board-previous-group)
    (should (equal (get-text-property (point) 'org-varde-group) "needs_you"))
    (should (bobp))
    ;; Nor anything to fold, and TAB on it does nothing.
    (search-forward "Working")
    (should-error (org-varde-board-toggle-group) :type 'user-error)
    (let ((before (buffer-string)))
      (org-varde-board-tab)
      (should (equal (buffer-string) before)))
    (org-varde-board-last-task)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000CCC333"))
    (org-varde-board-first-task)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))))

(ert-deftest org-varde-board-tab-goes-round-what-needs-you ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil)
          ("T0000000000000000000BBB222" "Two" "blocked" nil))
         ("open" "Open" ("T0000000000000000000CCC333" "Three" "open" nil))))
    (goto-char (point-max))
    (forward-line -1)
    (org-varde-board-tab)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    (org-varde-board-tab)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
    (org-varde-board-tab)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    (org-varde-board-previous-needs-you)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
    ;; On a group's heading, TAB folds it, and the folded heading is a stop.
    (goto-char (point-min))
    (org-varde-board-tab)
    (should (string-match-p "^Needs you  2 ▸$" (buffer-string)))
    (org-varde-board-last-task)
    (org-varde-board-previous-task)
    (should (equal (get-text-property (point) 'org-varde-group) "needs_you"))
    ;; Going to what needs you opens it again: S-TAB from elsewhere goes to the last.
    (org-varde-board-previous-needs-you)
    (should (string-match-p "^Needs you  2$" (buffer-string)))
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))))

(ert-deftest org-varde-board-a-folded-group-that-empties-opens-as-varde-has-it ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil))))
    (org-varde-board-toggle-group)
    (should (string-match-p "^Needs you  1 ▸$" (buffer-string)))
    (let ((org-varde-board-data (org-varde-board-test--data '(("needs_you" "Needs you")))))
      (org-varde-board--render))
    (org-varde-board--render)
    (should (string-match-p "^ ○ One" (buffer-string)))))

(ert-deftest org-varde-board-keeps-its-place-when-a-task-leaves ()
  "As the terminal board: after d, point stays on the line, now the next task."
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil)
          ("T0000000000000000000BBB222" "Two" "open" nil))
         ("done" "Done")))
    (switch-to-buffer (current-buffer))
    (org-varde-board-test--first-row)
    (cl-letf (((symbol-function 'org-varde--finish) #'ignore))
      (org-varde-board-done))
    ;; Before varde has it, the board stays on it.
    (org-varde-board--render)
    (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000AAA111"))
    (let ((org-varde-board-data (org-varde-board-test--data
                                 '(("open" "Open" ("T0000000000000000000BBB222" "Two" "open" nil))
                                   ("done" "Done" ("T0000000000000000000AAA111" "One" "done" nil))))))
      (org-varde-board--render)
      (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))
      ;; Moving on, the next render follows the task at point.
      (org-varde-board--render)
      (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222")))))

(ert-deftest org-varde-board-keeps-its-highlight-when-redrawn ()
  (org-varde-board-test--with
      (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil)
                                     ("T0000000000000000000BBB222" "Two" "open" nil))))
    (switch-to-buffer (current-buffer))
    (org-varde-board-next-task)
    (org-varde-board--render)
    (should (overlayp hl-line-overlay))
    (should (= (overlay-start hl-line-overlay) (line-beginning-position)))
    (should (> (overlay-end hl-line-overlay) (overlay-start hl-line-overlay)))))

(ert-deftest org-varde-board-draws-only-when-in-sight-and-changed ()
  (org-varde-board-test--with
      (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil))))
    (let ((drawn 0))
      (advice-add 'erase-buffer :before (lambda () (setq drawn (1+ drawn))) '((name . count)))
      (unwind-protect
          (progn
            (save-window-excursion
              (switch-to-buffer (current-buffer))
              (org-varde-board--refreshed)
              (should (= drawn 1))
              ;; The same data again: nothing to draw.
              (org-varde-board--refreshed)
              (should (= drawn 1)))
            ;; Out of sight, nothing is drawn.
            (setq org-varde-board-data (org-varde-board-test--data '(("open" "Open"))))
            (org-varde-board--refreshed)
            (should (= drawn 1)))
        (advice-remove 'erase-buffer 'count)))))

(ert-deftest org-varde-board-e-edits-the-title-and-context ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":2,\"title\":\"Old title\",\"body\":\"Old context\"}")
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Old title" "open" nil))))
      (org-varde-board-test--first-row)
      (save-window-excursion (org-varde-board-edit))
      (with-current-buffer "*varde edit aaa111*"
        (should (equal (buffer-string) "Old title\n\nOld context"))
        (erase-buffer)
        (insert "New title\n\nNew context\n")
        (call-interactively #'org-varde-new-create))
      (should-not (get-buffer "*varde edit aaa111*"))
      (should (member "set T0000000000000000000AAA111 --title=New title --body -" (org-varde-test-calls)))
      (should (equal (org-varde-test-stdin "set") "New context")))))

(ert-deftest org-varde-board-parked-opens-on-request ()
  (org-varde-board-test--with
      (org-varde--json
       "{\"v\":1,\"groups\":[{\"key\":\"open\",\"label\":\"Open\",\"collapsed\":false,\"rows\":[]},{\"key\":\"parked\",\"label\":\"Parked\",\"collapsed\":true,\"rows\":[{\"task\":\"T0000000000000000000AAA111\",\"short\":\"aaa111\",\"title\":\"Forge\",\"status\":\"parked\",\"project\":null,\"inbox\":false,\"external\":\"org\",\"last\":0}]}]}")
    (should (string-match-p "^Parked  1 ▸$" (buffer-string)))
    (should-not (string-match-p "Forge" (buffer-string)))
    (org-varde-board-first-task)
    (should (equal (get-text-property (point) 'org-varde-group) "parked"))
    (org-varde-board-toggle-group)
    (should (string-match-p "^ ‖ Forge" (buffer-string)))
    ;; A refresh keeps it open.
    (org-varde-board--render)
    (should (string-match-p "Forge" (buffer-string)))
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
        ;; The keys first, so `? help' is never cut; what it keeps to after.
        (should (string-match-p "\\` RET go  TAB needs you  v view  n new  d done  \\? help   proj · " (funcall header)))
        (should (equal (substring-no-properties (org-varde-board--scope-line t 1 1 80))
                       "proj · 1 in other projects, 1 needs you (. all)"))
        ;; Shorter where it must be.
        (should (equal (substring-no-properties (org-varde-board--scope-line t 1 1 30))
                       "proj · 1 needs you elsewhere"))
        (should (equal (org-varde-board--scope-line t 1 1 2) ""))
        ;; TAB keeps to it too, and says where the rest are.
        (let (said)
          (cl-letf (((symbol-function 'message) (lambda (f &rest args) (setq said (apply #'format f args)))))
            (org-varde-board-next-needs-you))
          (should (equal said "Nothing here needs you; 1 in other projects: . shows them")))
        (should (equal org-varde--kept "/home/u/proj"))
        (should (eq (key-binding ".") #'org-varde-board-toggle-project))
        (org-varde-board-toggle-project)
        (should (string-match-p "Elsewhere" (buffer-string)))
        (should (string-suffix-p "   every project (. only proj)" (funcall header)))
        ;; A project with no task on the board yet keeps to nothing.
        (setq org-varde-board--project "/home/u/new" org-varde-board--scoped t)
        (org-varde-board--render)
        (should (string-match-p "Elsewhere" (buffer-string)))
        (should (string-suffix-p "   every project (none in new yet)" (funcall header)))
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
      (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
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
          (should-not org-varde-board--project))
        ;; From an org note elsewhere: the VARDE_PROJECT of the entry at
        ;; point, inherited, decides.
        (org-varde-test-reply "show" (format "{\"v\":1,\"project\":%S,\"version\":0}" repo))
        (org-varde-test-with-org (format "#+PROPERTY: VARDE_PROJECT %s/src\n* Plan\n** Step\n" repo)
          (search-forward "Step")
          (org-varde-board))
        (should (equal (car (last (org-varde-test-calls)))
                       (format "show --project %s --json" (expand-file-name "src" repo))))
        (with-current-buffer org-varde-board-buffer
          (should (equal org-varde-board--project repo)))))))

(ert-deftest org-varde-board-renders-without-data ()
  (org-varde-board-test--with nil
    (should (string-match-p "Waiting for varde" (buffer-string)))))

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
    (should (string-match-p "^ ○ Later .*← aaa111 · " (buffer-string)))))

(ert-deftest org-varde-board-drops-a-task-without-work-on-a-y ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":2,\"state\":\"dropped\",\"worktree\":null}")
    (org-varde-test-reply "drop" "{\"v\":1,\"worktree\":null}")
    (org-varde-board-test--with
        (org-varde-board-test--inbox
         (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil)))))
      (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
        (org-varde-board-test--first-row)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) nil)))
          (org-varde-board-drop))
        (should-not (member "drop T0000000000000000000BBB222 --json" (org-varde-test-calls)))
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t)))
          (org-varde-board-drop))
        (should (member "drop T0000000000000000000BBB222 --json" (org-varde-test-calls)))))))

(ert-deftest org-varde-board-shows-a-task-s-changes-where-its-work-is ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "Here" "open" nil))))
      (let ((dir (make-temp-file "org-varde-wt-" t))
            shown)
        (unwind-protect
            ;; No magit here: vc-dir.
            (cl-letf (((symbol-function 'vc-dir) (lambda (d &rest _) (push d shown))))
              (org-varde-board-test--first-row)
              (should (eq (key-binding "=") #'org-varde-board-diff))
              ;; Its worktree, while it has one.
              (org-varde-test-reply "show" (format "{\"v\":1,\"project\":\"/nowhere\",\"worktree\":{\"state\":\"active\",\"path\":\"%s\"}}" dir))
              (org-varde-board-diff)
              (should (equal shown (list dir)))
              ;; Else its project.
              (org-varde-test-reply "show" (format "{\"v\":1,\"project\":\"%s\",\"worktree\":{\"state\":\"removed\",\"path\":\"/gone\"}}" dir))
              (org-varde-board-diff)
              (should (equal shown (list dir dir))))
          (delete-directory dir t))))))

(ert-deftest org-varde-board-keeps-its-header-short ()
  (org-varde-board-test--with nil
    (let ((hints (split-string (substring-no-properties header-line-format) "  " t)))
      (should (<= (length hints) 6))
      (should (member "? help" (mapcar #'string-trim hints))))
    (should (eq (key-binding "?") #'org-varde-board-menu))))

(ert-deftest org-varde-board-starting-inbox-work-does-not-file-it ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--inbox
         (org-varde-board-test--data '(("inbox" "Inbox" ("T0000000000000000000BBB222" "Later" "open" nil)))))
      (let (started)
        (cl-letf (((symbol-function 'org-varde-start-task) (lambda (task &rest _) (setq started task))))
          (org-varde-board-test--first-row)
          (org-varde-board-start)
          (should (equal started "T0000000000000000000BBB222")))))))

(ert-deftest org-varde-board-accepts-every-task-in-the-region ()
  (org-varde-test-with-varde
    (org-varde-board-test--with
        (org-varde-board-test--inbox
         (org-varde-board-test--data
          '(("inbox" "Inbox"
             ("T0000000000000000000AAA111" "One" "exited" nil)
             ("T0000000000000000000BBB222" "Two" "exited" nil)
             ("T0000000000000000000CCC333" "Three" "exited" nil)))))
      (org-varde-board-test--first-row)
      (org-varde-test-reply "accept" "accepted aaa111\n")
      (let (said)
        (cl-letf (((symbol-function 'message) (lambda (f &rest args) (setq said (apply #'format f args)))))
          (org-varde-board-accept))
        (should (equal said "Accepted \"One\"")))
      (should (equal (org-varde-test-calls) '("accept T0000000000000000000AAA111")))
      ;; The region covers the first two rows, ending at the start of the third.
      (transient-mark-mode 1)
      (push-mark (point) t t)
      (forward-line 2)
      (org-varde-board-accept)
      (should (equal (cadr (org-varde-test-calls))
                     "accept T0000000000000000000AAA111 T0000000000000000000BBB222"))
      (should-not (use-region-p)))))

(ert-deftest org-varde-board-accepts-only-what-is-in-the-inbox ()
  (org-varde-test-with-varde
    (let ((data (org-varde-board-test--data
                 '(("open" "Open" ("T0000000000000000000AAA111" "Mine" "open" nil))
                   ("inbox" "Inbox"
                    ("T0000000000000000000BBB222" "Two" "exited" nil)
                    ("T0000000000000000000CCC333" "Three" "exited" nil))))))
      (org-varde-board-test--inbox (list :groups (cdr (plist-get data :groups))))
      (org-varde-board-test--with data
        (org-varde-board-test--first-row)
        ;; Not in the Inbox: said plainly, and varde is not asked.
        (should (equal (cadr (should-error (org-varde-board-accept) :type 'user-error))
                       "\"Mine\" is not in the Inbox: a accepts Inbox tasks"))
        (should-not (org-varde-test-calls))
        ;; A region with some: those are accepted, and the rest counted.
        (transient-mark-mode 1)
        (push-mark (point) t t)
        (goto-char (point-max))
        (let ((said (car (org-varde-test--saying (org-varde-board-accept)))))
          (should (equal said "Accepted 2 tasks; 1 not in the Inbox is left as it is")))
        (should (equal (org-varde-test-calls)
                       '("accept T0000000000000000000BBB222 T0000000000000000000CCC333")))))))

;; x on a region asks once for them all, as the terminal does.
(ert-deftest org-varde-board-drops-every-task-in-the-region-asking-once ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\",\"worktree\":null}")
    (org-varde-test-reply "drop" "{\"v\":1,\"worktree\":null}")
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
      (let (asked)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (p) (push p asked) t)))
          (org-varde-board-drop)
          (org-varde-test-wait))
        (should (equal asked '("Drop 3 tasks? ")))
        (should (= 3 (seq-count (lambda (c) (string-prefix-p "drop " c)) (org-varde-test-calls))))
        (should (= 3 (length org-varde-board--closed)))
        ;; Said no, nothing is dropped.
        (setq asked nil)
        (push-mark (point-min) t t)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (p) (push p asked) nil)))
          (org-varde-board-drop))
        (should (= 1 (length asked)))
        (should (= 3 (seq-count (lambda (c) (string-prefix-p "drop " c)) (org-varde-test-calls))))))))

;; Coming back to the board from a task's terminal: as you left it, on
;; that task, and no project asked of varde.
(ert-deftest org-varde-board-comes-back-as-you-left-it ()
  (org-varde-test-with-varde
    (let ((org-varde-board-data (org-varde-board-test--data
                                 '(("open" "Open"
                                    ("T0000000000000000000AAA111" "One" "open" nil)
                                    ("T0000000000000000000BBB222" "Two" "open" nil))))))
      (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
        (save-window-excursion
          (with-temp-buffer
            (setq default-directory "/tmp/")
            (org-varde-board))
          (with-current-buffer org-varde-board-buffer
            (setq org-varde-board--project "/home/u/proj" org-varde-board--scoped nil))
          (let ((calls (length (org-varde-test-calls))))
            (with-temp-buffer
              (setq org-varde--task "T0000000000000000000BBB222")
              (switch-to-buffer (current-buffer))
              (org-varde-board))
            (should (= calls (length (org-varde-test-calls)))))
          (with-current-buffer org-varde-board-buffer
            (should (equal org-varde-board--project "/home/u/proj"))
            (should-not org-varde-board--scoped)
            (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222"))))))))

;; A board that fits its window shows from the top once it grows again.
(ert-deftest org-varde-board-that-fits-shows-from-its-top ()
  (org-varde-board-test--with
      (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil))))
    (save-window-excursion
      (delete-other-windows)
      (switch-to-buffer (current-buffer))
      (goto-char (point-max))
      (set-window-start (selected-window) (line-beginning-position 0))
      (org-varde-board--resized (selected-window))
      (should (= (window-start) 1)))))

(ert-deftest org-varde-board-tab-on-a-row-folds-its-group ()
  (org-varde-board-test--with
      (org-varde-board-test--data
       '(("needs_you" "Needs you" ("T0000000000000000000AAA111" "One" "waiting" nil)
          ("T0000000000000000000BBB222" "Two" "waiting" nil))
         ("open" "Open" ("T0000000000000000000CCC333" "Three" "open" nil))))
    (goto-char (point-min))
    (forward-line 2)
    (org-varde-board-toggle-group)
    (should (string-match-p "^Needs you  2 ▸$" (buffer-string)))
    (should-not (string-match-p "One" (buffer-string)))
    (should (string-match-p "Three" (buffer-string)))
    (should (equal (get-text-property (point) 'org-varde-group) "needs_you"))
    (org-varde-board-toggle-group)
    (should (string-match-p "One" (buffer-string)))))

(ert-deftest org-varde-board-keys-follow-emacs-conventions ()
  "n makes a task, as on varde's own board; j/k and C-n/C-p move between tasks."
  (org-varde-board-test--with nil
    (should (eq (key-binding "n") #'org-varde-board-create))
    (should (eq (key-binding "j") #'org-varde-board-next-task))
    (should (eq (key-binding (kbd "C-n")) #'org-varde-board-next-task))
    (should (eq (key-binding (kbd "C-p")) #'org-varde-board-previous-task))
    (should (eq (key-binding "}") #'org-varde-board-next-group))
    (should (eq (key-binding (kbd "M-<")) #'org-varde-board-first-task))
    (should (eq (key-binding (kbd "TAB")) #'org-varde-board-tab))
    (should (eq (key-binding (kbd "<backtab>")) #'org-varde-board-previous-needs-you))
    (should (eq (key-binding "z") #'org-varde-board-toggle-group))
    (should (eq (key-binding "P") #'org-varde-board-park))
    (should (eq (key-binding "m") #'org-varde-board-move))
    (should (eq (key-binding "s") #'org-varde-board-start-background))
    (should (eq (key-binding "v") #'org-varde-show))
    ;; The project's cairn is the view's other tab: ] there.
    (should (eq (key-binding "C") #'undefined))
    (should (eq (key-binding "e") #'org-varde-board-edit))
    (should (eq (key-binding "g") #'org-varde-refresh))))

(ert-deftest org-varde-board-keys-under-evil ()
  "Run with evil on the load path to check this; skipped otherwise."
  (skip-unless (require 'evil nil t))
  (evil-mode 1)
  (unwind-protect
      (org-varde-board-test--with nil
        (evil-normal-state)
        (should (eq (key-binding "d") #'org-varde-board-done))
        (should (eq (key-binding "P") #'org-varde-board-park))
        (should (eq (key-binding "j") #'org-varde-board-next-task))
        (should (eq (key-binding "}") #'org-varde-board-next-group))
        (should (eq (key-binding "gr") #'org-varde-refresh))
        (should (eq (key-binding "gg") #'org-varde-board-first-task))
        (should (eq (key-binding "G") #'org-varde-board-last-task))
        (should (eq (key-binding (kbd "TAB")) #'org-varde-board-tab))
        (should (eq (key-binding "za") #'org-varde-board-toggle-group))
        (should (eq (key-binding "q") #'quit-window))
        (should (eq (key-binding "s") #'org-varde-board-start-background))
        (should (eq (key-binding "K") #'org-varde-show))
        (should-not (eq (key-binding "C") #'org-varde-show-project))
        (should (string-match-p "K view" (org-varde-board--header)))
        ;; v and V are evil's selections; K views.
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
        (should (eq (key-binding "s") #'org-varde-board-start-background)))
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
          (should (evil-normal-state-p)))
        ;; x drops the selected lines, asked once; u reopens.
        (let (dropped)
          (cl-letf (((symbol-function 'y-or-n-p) #'always)
                    ((symbol-function 'org-varde--has-work-p) #'ignore)
                    ((symbol-function 'org-varde-drop-task)
                     (lambda (task _title asked) (push (list task asked) dropped))))
            (execute-kbd-macro (kbd "gg V j x")))
          (should (equal (nreverse dropped)
                         '(("T0000000000000000000AAA111" t) ("T0000000000000000000BBB222" t)))))
        (should (eq (key-binding "u") #'org-varde-board-reopen)))
    (evil-mode -1)))

(ert-deftest org-varde-board-parks-a-closed-task-once-you-confirm ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"done\"}")
    (org-varde-test-reply "park" "{\"v\":1}")
    (let ((data (org-varde-board-test--data '(("done" "Done" ("T0000000000000000000AAA111" "Finished" "done" nil))))))
      (plist-put (car (plist-get (car (plist-get data :groups)) :rows)) :state "done")
      (org-varde-board-test--with data
        (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
          (org-varde-board-test--first-row)
          (let (asked said)
            (cl-letf (((symbol-function 'y-or-n-p) (lambda (prompt) (setq asked prompt) nil)))
              (should-error (org-varde-board-park) :type 'user-error))
            (should (equal asked "\"Finished\" is done: park it instead? "))
            (should-not (member "park T0000000000000000000AAA111 --json" (org-varde-test-calls)))
            ;; Parked, it says so.
            (cl-letf (((symbol-function 'y-or-n-p) #'always)
                      ((symbol-function 'message) (lambda (f &rest a) (when f (setq said (apply #'format f a))))))
              (org-varde-board-park)
              (org-varde-test-wait))
            (should (member "park T0000000000000000000AAA111 --json" (org-varde-test-calls)))
            (should (equal said "Parked \"Finished\"; u reopens"))
            ;; u reopens it, and the board is on it.
            (org-varde-board-reopen)
            (should (member "reopen T0000000000000000000AAA111" (org-varde-test-calls)))
            (should (equal org-varde-board--select "T0000000000000000000AAA111"))
            (should-error (org-varde-board-reopen) :type 'user-error)))))))

(ert-deftest org-varde-board-opens-a-folded-group-a-view-steps-into ()
  (let ((data (org-varde-board-test--data '(("open" "Open" ("T0000000000000000000AAA111" "One" "open" nil))
                                            ("parked" "Parked" ("T0000000000000000000BBB222" "Shelved" "parked" nil))))))
    (plist-put (cadr (plist-get data :groups)) :collapsed t)
    (org-varde-board-test--with data
      (should-not (string-match-p "Shelved" (buffer-string)))
      ;; J and K in a view go through it too.
      (should (equal (org-varde--board-tasks) '("T0000000000000000000AAA111" "T0000000000000000000BBB222")))
      (org-varde--board-select "T0000000000000000000BBB222")
      (should (string-match-p "Shelved" (buffer-string)))
      (should (equal (get-text-property (point) 'org-varde-task) "T0000000000000000000BBB222")))))

(ert-deftest org-varde-board-menu-hides-what-does-nothing ()
  (let ((data (org-varde-board-test--data '(("done" "Done" ("T0000000000000000000AAA111" "Finished" "done" nil))))))
    (plist-put (car (plist-get (car (plist-get data :groups)) :rows)) :state "done")
    (org-varde-board-test--with data
      (org-varde-board-test--first-row)
      (should-not (org-varde--can-done-p))
      (should (org-varde--can-park-p)))))

(ert-deftest org-varde-board-menu-says-what-each-group-holds ()
  (let ((lines (split-string (org-varde-board--legend) "\n")))
    (should (equal (mapcan (lambda (l) (list (string-trim (substring l 0 11)) (string-trim (substring l 36 47))))
                           lines)
                   '("Needs you" "Inbox" "Working" "Done" "Open" "Parked")))
    ;; Each fits 80 columns.
    (should (seq-every-p (lambda (l) (<= (string-width l) 80)) lines))))

(provide 'org-varde-board-test)
;;; org-varde-board-test.el ends here
