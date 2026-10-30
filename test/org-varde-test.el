;;; org-varde-test.el --- Tests for the varde client -*- lexical-binding: t; -*-

;;; Code:

(require 'ert)
(require 'test-helper)
(require 'org-varde)

(defconst org-varde-test--id "01K5ZZZZZZZZZZZZZZZZXYZ123")

(defconst org-varde-test--shown
  (concat "{\"v\":2,\"id\":\"" org-varde-test--id "\",\"short\":\"xyz123\","
          "\"title\":\"Fix the login bug\",\"project\":\"/p/app\",\"state\":\"open\","
          "\"body\":\"It fails on CI only.\",\"notes\":[],\"runs\":[],\"worktree\":null,"
          "\"cairn\":{\"status\":\"Cause found.\","
          "\"findings\":[{\"id\":12,\"text\":\"The key omits the locale.\",\"mine\":false,\"stale\":true}],"
          "\"decisions\":[],\"open\":[{\"id\":14,\"text\":\"Add the locale.\",\"mine\":true}]}}")
  "A task as `varde show --json --no-git' gives it.")

;;; --- Starting ----------------------------------------------------------------

(defun org-varde-test--run-plan ()
  (org-varde-test-reply "start" "{\"v\":1,\"action\":\"run\",\"run\":\"R1\",\"cwd\":\"/tmp/proj\",\"command\":\"varde run R1\"}"))

(ert-deftest org-varde-start-opens-a-terminal-and-types-the-run ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (save-window-excursion
      (org-varde-go-task org-varde-test--id "t")
      (org-varde-test-wait)
      (should (equal org-varde-test-terminals '(("/tmp/proj" "*varde xyz123*"))))
      (should (equal org-varde-test-sent '("varde run R1")))
      (should (equal (buffer-local-value 'org-varde--task (get-buffer "*varde xyz123*"))
                     org-varde-test--id))
      ;; It goes there.
      (should (eq (window-buffer) (get-buffer "*varde xyz123*")))
      ;; C-u is a fresh conversation, C-u C-u a fork.
      (org-varde-start-task org-varde-test--id "t" '(4))
      (org-varde-test-wait)
      (org-varde-start-task org-varde-test--id "t" '(16))
      (let ((starts (seq-filter (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls))))
        (should (equal (car starts) (concat "start " org-varde-test--id " --json")))
        (should (string-match-p " --fresh" (nth 1 starts)))
        (should (string-match-p " --fork" (nth 2 starts)))))))

;; The board's RET on a region: nothing goes to a terminal, even one
;; already running.
(ert-deftest org-varde-starting-in-the-background-goes-nowhere ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (with-temp-buffer
      (let ((here (window-buffer))
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
          (org-varde--background t)
          (current "main")
          (spaces '("main"))
          (shown nil))
      (cl-letf (((symbol-function '+workspace-list-names) (lambda () spaces))
                ((symbol-function '+workspace-current-name) (lambda () current))
                ((symbol-function '+workspace-switch)
                 (lambda (name &optional _create)
                   (unless (member name spaces) (push name spaces))
                   (setq current name)))
                ((symbol-function '+workspace-contains-buffer-p) (lambda (&rest _) t))
                ((symbol-function 'pop-to-buffer-same-window)
                 (lambda (buffer) (push (cons current (buffer-name buffer)) shown))))
        (org-varde--start org-varde-test--id "t" nil nil)
        (org-varde-test-wait)
        (should (equal current "main"))
        (should (member "xyz123 t" spaces))
        (should (equal shown '(("xyz123 t" . "*varde xyz123*"))))))))

;; A workspace laid out with the terminal before the session printed
;; anything kept its window at the top: going there shows the cursor.
(ert-deftest org-varde-going-to-a-vterm-shows-its-cursor ()
  (org-varde-test-with-varde
    (let ((buffer (generate-new-buffer "*varde xyz123*"))
          reset)
      (unwind-protect
          (cl-letf (((symbol-function 'vterm-reset-cursor-point)
                     (lambda () (push (current-buffer) reset))))
            (with-current-buffer buffer (setq major-mode 'vterm-mode))
            (save-window-excursion (org-varde--go-to-buffer buffer))
            (should (equal reset (list buffer)))
            ;; Not while you are reading back in copy mode.
            (with-current-buffer buffer (setq-local vterm-copy-mode t))
            (save-window-excursion (org-varde--go-to-buffer buffer))
            (should (equal reset (list buffer))))
        (kill-buffer buffer)))))

(defvar ghostel-use-native-pty)
(defvar ghostel-buffer-name)
(defvar ghostel--input-mode)

(ert-deftest org-varde-going-to-a-ghostel-shows-its-cursor ()
  (org-varde-test-with-varde
    (let ((buffer (generate-new-buffer "*varde xyz123*")))
      (unwind-protect
          (cl-letf (((symbol-function 'ghostel-cursor-point) (lambda () 4)))
            (with-current-buffer buffer
              (insert "$ hello")
              (goto-char (point-min))
              (setq major-mode 'ghostel-mode))
            (save-window-excursion (org-varde--go-to-buffer buffer))
            (should (= (with-current-buffer buffer (point)) 4))
            ;; Not while you are reading back in copy mode.
            (with-current-buffer buffer
              (goto-char (point-min))
              (setq-local ghostel--input-mode 'copy))
            (save-window-excursion (org-varde--go-to-buffer buffer))
            (should (= (with-current-buffer buffer (point)) 1)))
        (kill-buffer buffer)))))

;; Through Emacs's PTY, so the buffer's process has a tty and a
;; foreground process to read; ghostel's native one is a pipe.
(ert-deftest org-varde-a-ghostel-is-opened-through-emacs-s-pty ()
  (org-varde-test-with-varde
    (let* ((org-varde-terminals '(ghostel))
           (dir (make-temp-file "ghostel" t))
           (load-path (cons dir load-path))
           (ghostel-use-native-pty t)
           (ghostel-buffer-name "*ghostel*")
           made)
      (with-temp-file (expand-file-name "ghostel.el" dir) (insert "(provide 'ghostel)\n"))
      (unwind-protect
          (cl-letf (((symbol-function 'ghostel)
                     (lambda (arg) (setq made (list arg ghostel-buffer-name ghostel-use-native-pty default-directory))
                       (get-buffer-create ghostel-buffer-name))))
            (should (equal (buffer-name (org-varde--open-shell "/tmp" "*varde xyz123*")) "*varde xyz123*")))
        (setq features (delq 'ghostel features))
        (delete-directory dir t))
      (should (equal made '(t "*varde xyz123*" nil "/tmp/"))))))

(ert-deftest org-varde-a-command-is-typed-into-a-ghostel ()
  (org-varde-test-with-varde
    (with-temp-buffer
      (setq major-mode 'ghostel-mode)
      (let (sent)
        (cl-letf (((symbol-function 'ghostel-send-string) (lambda (s) (push s sent))))
          (org-varde--send "varde run abc"))
        (should (equal sent '("varde run abc\r")))))))

;; The first terminal you prefer that is installed; term when none is.
(ert-deftest org-varde-the-terminal-is-the-first-preferred-one-installed ()
  (cl-letf (((symbol-function 'locate-library) (lambda (name) (equal name "vterm"))))
    (let ((org-varde-terminals '(ghostel vterm)))
      (should (eq (org-varde--terminal-kind) 'vterm)))
    (let ((org-varde-terminals '(term vterm)))
      (should (eq (org-varde--terminal-kind) 'term)))
    (let ((org-varde-terminals '(ghostel)))
      (should (eq (org-varde--terminal-kind) 'term)))))

(ert-deftest org-varde-with-no-terminal-installed-a-term-is-opened ()
  (org-varde-test-with-varde
    (let ((org-varde-terminals nil)
          made)
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

(ert-deftest org-varde-a-second-session-is-not-started-in-a-busy-terminal ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test--with-terminal
      (cl-letf (((symbol-function 'process-running-child-p) (lambda (&rest _) t)))
        (should-error (org-varde--start org-varde-test--id "t" "--fresh" nil) :type 'user-error))
      (should-not (seq-find (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls)))
      (should-not org-varde-test-sent))))

(ert-deftest org-varde-a-task-is-not-started-again-before-varde-answers ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (save-window-excursion
      (org-varde--start org-varde-test--id "t" nil nil)
      (should (equal org-varde--starting (list org-varde-test--id)))
      (should-error (org-varde--start org-varde-test--id "t" nil nil) :type 'user-error)
      (org-varde-test-wait)
      (should-not org-varde--starting)
      (should (= 1 (seq-count (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls))))
      ;; Once varde has answered, even with a failure, it can be asked again.
      (org-varde-test-fail-once "start" "varde: no such task")
      (org-varde--start org-varde-test--id "t" nil nil)
      (org-varde-test-wait)
      (should-not org-varde--starting)
      (should (= 2 (seq-count (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls)))))))

(ert-deftest org-varde-what-varde-says-on-a-call-that-worked-is-shown ()
  (org-varde-test-with-varde
    (let ((org-varde-varde-program (expand-file-name "warns" org-varde-test-dir))
          said got)
      (with-temp-file org-varde-varde-program
        (insert "#!/bin/sh\necho out\necho 'varde: the setup failed; the session starts anyway' >&2\n"))
      (set-file-modes org-varde-varde-program #o755)
      (cl-letf (((symbol-function 'message)
                 (lambda (format &rest args) (push (apply #'format-message format args) said))))
        (org-varde--varde-async '("start") (lambda (out) (setq got out)))
        (org-varde-test-wait))
      (should (equal got "out\n"))
      (should (member "varde: the setup failed; the session starts anyway" said)))))

(ert-deftest org-varde-the-project-question-comes-from-varde ()
  (org-varde-test-with-varde
    (org-varde-test--run-plan)
    (org-varde-test-reply-once "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"project\",\"choices\":[\"/p/repo\"]}")
    ;; RET takes the first of varde's choices.
    (cl-letf (((symbol-function 'completing-read) (org-varde-test-pick)))
      (org-varde--start org-varde-test--id "t" nil nil)
      (org-varde-test-wait))
    (should (equal org-varde-test-offered (list "/p/repo" org-varde--other-directory)))
    (should (string-suffix-p "--project /p/repo" (car (last (org-varde-test-calls)))))
    (should (equal org-varde-test-sent '("varde run R1")))))

;; The board's RET on a region: the worktree answer holds for every task
;; in it, and a task's project question offers the one before's answer.
(ert-deftest org-varde-a-region-is-asked-about-worktrees-once ()
  (org-varde-test-with-varde
    (let (calls prompts defaults (worktree-asked 0))
      (cl-letf (((symbol-function 'org-varde--varde-async)
                 (lambda (args callback &rest _)
                   (push args calls)
                   (funcall callback
                            (cond ((not (member "--project" args))
                                   "{\"v\":1,\"action\":\"ask\",\"ask\":\"project\",\"choices\":[\"/p/a\"]}")
                                  ((not (member "--worktree" args)) "{\"v\":1,\"action\":\"ask\",\"ask\":\"worktree\"}")
                                  (t "{\"v\":1,\"action\":\"run\"}")))))
                ((symbol-function 'completing-read)
                 (lambda (prompt collection &rest _)
                   (push prompt prompts)
                   (funcall (org-varde-test-pick (and (= (length prompts) 1) "/p/b/")) prompt collection)
                   (push (car org-varde-test-offered) defaults)
                   (if (= (length prompts) 1) "/p/b/" (car (all-completions "" collection)))))
                ((symbol-function 'y-or-n-p) (lambda (&rest _) (setq worktree-asked (1+ worktree-asked)))))
        (let ((org-varde--background t)
              (org-varde--answers (list :region t)))
          ;; As the board does: each starts once the one before is planned.
          (org-varde--plan-answering '("start" "T1" "--json") "One"
                                     (lambda (_) (org-varde--plan-answering '("start" "T2" "--json") "Two"
                                                                            #'ignore #'ignore))
                                     #'ignore))
        (should (= worktree-asked 1))
        (should (equal (nreverse prompts) '("Project for One (default a): " "Project for Two (default b): ")))
        (should (equal (nreverse defaults) '("/p/a" "/p/b")))
        (should (equal (car calls) '("start" "T2" "--json" "--project" "/p/b" "--worktree" "always")))
        ;; A start of its own keeps nothing.
        (should-not org-varde--answers)
        (org-varde--plan-answering '("start" "T3" "--json") "Three" #'ignore #'ignore)
        (should (= worktree-asked 2))
        (should (equal (car defaults) "/p/a"))
        ;; Nor does one started in the background, as s on the board does,
        ;; and it is asked about itself, not a region.
        (let (asked)
          (cl-letf (((symbol-function 'y-or-n-p) (lambda (prompt) (push prompt asked) t)))
            (let ((org-varde--background t))
              (org-varde--plan-answering '("start" "T4" "--json" "--project" "/p/app/") "Four" #'ignore #'ignore)))
          (should (equal asked '("Start \"Four\" in a worktree of its own (app)? ")))
          (should-not org-varde--answers))))))

(ert-deftest org-varde-a-question-asked-again-is-not-asked-forever ()
  (org-varde-test-with-varde
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"project\",\"choices\":[]}")
    (let ((asked 0) why)
      (cl-letf (((symbol-function 'completing-read) (lambda (&rest _) (setq asked (1+ asked)) "/p/")))
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

(ert-deftest org-varde-show-comes-at-once-and-git-follows ()
  (org-varde-test-with-varde
    (org-varde-test-reply-once "show" org-varde-test--shown)
    (org-varde-test-reply "show" (concat (string-remove-suffix "}" org-varde-test--shown)
                                         ",\"git\":{\"stage\":\"committed\",\"branch\":\"varde/fix\"}}"))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (should (string-match-p "\\`Fix the login bug  xyz123\n" (buffer-string)))
      (should (string-match-p "^Git\n  looking…$" (buffer-string))))
    (should (equal (org-varde-test-calls)
                   (list (format "show %s --json --no-git" org-varde-test--id)
                         (format "show %s --json" org-varde-test--id))))
    (with-current-buffer "*varde show*"
      (should (string-match-p "^  committed · on varde/fix$" (buffer-string))))))

(ert-deftest org-varde-show-lists-a-repeating-tasks-sessions-and-ret-opens-one ()
  (org-varde-test-with-varde
    (org-varde-test-reply
     "show" (concat (string-remove-suffix "}" org-varde-test--shown)
                    ",\"conversations\":["
                    "{\"run\":\"RUN3\",\"started\":1760000000000,\"status\":null,\"unread\":true,\"quiet\":false,\"launched\":true},"
                    "{\"run\":\"RUN2\",\"started\":1759900000000,\"status\":null,\"unread\":false,\"quiet\":true,\"launched\":true},"
                    "{\"run\":\"RUN1\",\"started\":1759800000000,\"status\":null,\"unread\":true,\"quiet\":false,\"launched\":true},"
                    "{\"run\":\"RUN0\",\"started\":1759700000000,\"status\":null,\"unread\":false,\"quiet\":false,\"launched\":true}]}"))
    (setq org-varde-board-data
          `(:groups ((:key "needs_you"
                      :rows ((:task ,org-varde-test--id
                              :schedule (:rule "every day 06:00" :label "↻ Fri 06:00" :repeats t)))))))
    (org-varde-test--run-plan)
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      ;; Every one, under its schedule: two unread, one that found nothing.
      (should (string-match-p "^Sessions · every day 06:00, next Fri 06:00$" (buffer-string)))
      (should (= 4 (how-many "^  [● ] [A-Z][a-z][a-z] " (point-min) (point-max))))
      (should (= 2 (how-many "^  ● " (point-min) (point-max))))
      (should (= 1 (how-many "  quiet$" (point-min) (point-max))))
      (goto-char (point-min))
      (re-search-forward "  quiet$")
      (save-window-excursion (org-varde-show-go)))
    (should (member (format "start %s --json --run RUN2" org-varde-test--id) (org-varde-test-calls)))))

(ert-deftest org-varde-show-folds-a-schedules-older-quiet-sessions ()
  (org-varde-test-with-varde
    (org-varde-test-reply
     "show" (concat (string-remove-suffix
                     "}" (string-replace "\"runs\":[]" "\"runs\":[{\"id\":\"RUN0\",\"background\":true}]"
                                         org-varde-test--shown))
                    ",\"conversations\":["
                    (mapconcat (lambda (i)
                                 (format "{\"run\":\"RUN%d\",\"started\":%d,\"status\":null,\"unread\":false,\"quiet\":%s,\"launched\":true}"
                                         i (- 1760000000000 (* i 1800000)) (if (= i 15) "false" "true")))
                               (number-sequence 1 30) ",")
                    "]}"))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      ;; The newest ten, and an older one that found something.
      (should (string-match-p "^Sessions · 19 earlier found nothing$" (buffer-string)))
      (should (= 11 (how-many "^  [● ] [A-Z][a-z][a-z] " (point-min) (point-max)))))))

(ert-deftest org-varde-show-lists-every-session-once-its-series-ended ()
  (org-varde-test-with-varde
    (org-varde-test-reply
     "show" (concat (string-remove-suffix
                     "}" (string-replace "\"runs\":[]" "\"runs\":[{\"id\":\"RUN0\",\"background\":true}]"
                                         org-varde-test--shown))
                    ",\"conversations\":["
                    "{\"run\":\"RUN3\",\"started\":1760000000000,\"status\":null,\"unread\":false,\"quiet\":true,\"launched\":true},"
                    "{\"run\":\"RUN2\",\"started\":1759900000000,\"status\":null,\"unread\":false,\"quiet\":true,\"launched\":true},"
                    "{\"run\":\"RUN1\",\"started\":1759800000000,\"status\":null,\"unread\":false,\"quiet\":true,\"launched\":true},"
                    "{\"run\":\"RUN0\",\"started\":1759700000000,\"status\":null,\"unread\":false,\"quiet\":false,\"launched\":true,"
                    "\"continued\":[1760100000000,\"resume\"]}]}"))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (should (string-match-p "^Sessions$" (buffer-string)))
      (should (= 1 (how-many "  resumed [A-Z][a-z][a-z] " (point-min) (point-max))))
      (should (= 4 (how-many "^  [● ] [A-Z][a-z][a-z] " (point-min) (point-max)))))))

(ert-deftest org-varde-done-on-a-repeating-task-ends-its-run ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" org-varde-test--shown)
    (org-varde-test-reply "done" (concat "{\"v\":1,\"task\":\"" org-varde-test--id
                                         "\",\"repeats\":\"every day 06:00\",\"stopped\":[],\"left\":[]}"))
    (let ((org-varde--on-board t))
      (org-varde--finish org-varde-test--id "Scan"))
    (org-varde-test-wait)
    (with-current-buffer "*Messages*"
      (should (string-match-p "Ended this run of .*; it repeats (every day 06:00)\n\\'" (buffer-string))))))

(ert-deftest org-varde-show-keys-stay-in-the-show-buffer ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" org-varde-test--shown)
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (should (eq major-mode 'org-varde-show-mode))
      (should (eq (key-binding "e") #'org-varde-cairn-edit))
      (should (eq (key-binding "E") #'org-varde-cairn-edit-all))
      (should (eq (key-binding (kbd "RET")) #'org-varde-show-go)))
    ;; Not bound in every special-mode buffer.
    (should-not (lookup-key special-mode-map "e"))
    (kill-buffer "*varde show*")))

(ert-deftest org-varde-a-saved-cairn-shows-in-its-views ()
  "After C-c C-c the view shows the new cairn, on both its tabs, and g
fetches them again."
  (org-varde-test-with-varde
    (progn
      (org-varde-test-reply "show" (org-varde-test--both "Old line"))
      (save-window-excursion (org-varde-show org-varde-test--id))
      (org-varde-test-wait)
      (org-varde-test-reply "edit" "Open:\n")
      (save-window-excursion (org-varde-edit-cairn org-varde-test--id))
      (org-varde-test-reply "show" (org-varde-test--both "New line"))
      (with-current-buffer "*cairn xyz123*" (org-varde-cairn-save))
      (org-varde-test-wait)
      (should (string-match-p "^  - New line$" (with-current-buffer "*varde show*" (buffer-string))))
      ;; Its Project tab shows the project's cairn, and g reads it again.
      (with-current-buffer "*varde show*"
        (org-varde-show-next-tab)
        (should (member (format "show --project %s --json" "/p/app") (org-varde-test-calls)))
        (should (string-match-p "^How-to\n  - New line$" (buffer-string)))
        (org-varde-test-reply "show" (org-varde-test--both "Newer still"))
        (revert-buffer)
        (org-varde-test-wait)
        (should (eq org-varde--tab 'project))
        (should (string-match-p "^How-to\n  - Newer still$" (buffer-string)))))))

(defun org-varde-test--both (line)
  "A task and a project cairn in one reply, each holding LINE."
  (format "{\"v\":2,\"id\":\"%s\",\"title\":\"T\",\"project\":\"/p/app\",\"runs\":[],\
\"cairn\":{\"status\":\"\",\"findings\":[],\"decisions\":[],\"open\":[{\"id\":1,\"text\":\"%s\"}],\
\"conventions\":[],\"gotchas\":[],\"howto\":[{\"id\":2,\"text\":\"%s\"}]}}"
          org-varde-test--id line line))

(ert-deftest org-varde-a-recalled-task-opens-from-its-hit ()
  (org-varde-test-with-varde
    (org-varde-test-reply "recall" (concat "{\"v\":1,\"everywhere\":true,\"hits\":["
                                           "{\"task\":\"" org-varde-test--id "\",\"short\":\"xyz123\","
                                           "\"title\":\"Fix the login bug\",\"project\":\"/p\",\"state\":\"done\","
                                           "\"kind\":\"findings\",\"snippet\":\"the token expires\",\"stale\":true},"
                                           "{\"task\":\"\",\"short\":\"project\",\"title\":\"project cairn\","
                                           "\"project\":\"/p\",\"kind\":\"gotchas\",\"snippet\":\"run make first\",\"more\":2}]}"))
    (org-varde-test-reply "show" org-varde-test--shown)
    (save-window-excursion (org-varde-recall "token"))
    (should (seq-find (lambda (c) (string-match-p "\\`recall --json --project .* token\\'" c))
                      (org-varde-test-calls)))
    (with-current-buffer "*varde recall*"
      (should (equal (buffer-string)
                     (concat "Recall: token  in every project\n\n"
                             "Nothing in this project; the best from every project:\n\n"
                             "xyz123  Fix the login bug (done)  p\n    finding: the token expires ?\n"
                             "p · project cairn\n    gotcha: run make first  (+2 more in this project cairn)\n")))
      ;; It opens on the first hit, which is the task's, by its full id.
      (should (eq (key-binding (kbd "RET")) #'org-varde-recall-show))
      ;; j and k go between hits, as n and p do and as in the terminal.
      (should (eq (key-binding "j") #'org-varde-recall-next))
      (should (eq (key-binding "k") #'org-varde-recall-previous))
      (should (eq (key-binding (kbd "C-n")) #'org-varde-recall-next))
      (should (equal (org-varde--task-here) org-varde-test--id))
      ;; n and p go between hits; s starts the task, staying here.
      (org-varde-recall-next)
      (should (looking-at-p "p · project cairn"))
      (should-error (org-varde-recall-start) :type 'user-error)
      (org-varde-recall-previous)
      (should (looking-at-p "xyz123"))
      (let (started)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t))
                  ((symbol-function 'org-varde-start-task)
                   (lambda (task &rest _) (push (list task org-varde--background) started))))
          (call-interactively #'org-varde-recall-start))
        (should (equal started (list (list org-varde-test--id t)))))
      ;; The view takes the hits' window, and q brings them back.
      (save-window-excursion
        (delete-other-windows)
        (switch-to-buffer "*varde recall*")
        (call-interactively #'org-varde-recall-show)
        (should (one-window-p))
        (should (equal (buffer-name) "*varde show*"))
        (quit-window)
        (should (equal (buffer-name) "*varde recall*")))
      (should (member (format "show %s --json --no-git" org-varde-test--id) (org-varde-test-calls)))
      ;; A project's hit shows its cairn.
      (forward-line 2)
      (should-error (org-varde--task-here) :type 'user-error)
      (save-window-excursion (call-interactively #'org-varde-recall-show))
      (should (member "show --project /p --json" (org-varde-test-calls))))))

(ert-deftest org-varde-recall-s-dot-looks-in-every-project-and-back ()
  "As the terminal's: . asks again in every project, and again here."
  (org-varde-test-with-varde
    (org-varde-test-reply "recall" (concat "{\"v\":1,\"everywhere\":false,\"hits\":[{\"task\":\"" org-varde-test--id
                                           "\",\"short\":\"xyz123\",\"title\":\"T\",\"project\":\"/p/app\","
                                           "\"state\":\"open\",\"kind\":\"findings\",\"snippet\":\"the token\"}]}"))
    (save-window-excursion (org-varde-recall "token" nil "/p/app"))
    (with-current-buffer "*varde recall*"
      (should (string-prefix-p "Recall: token  in this project  (. every project)\n" (buffer-string)))
      (should (eq (key-binding ".") #'org-varde-recall-toggle-scope))
      (save-window-excursion (org-varde-recall-toggle-scope))
      (should (string-prefix-p "Recall: token  in every project  (. this project first)\n" (buffer-string)))
      (save-window-excursion (org-varde-recall-toggle-scope))
      (should (string-prefix-p "Recall: token  in this project" (buffer-string)))
      ;; Found nowhere, it looked everywhere: no scope to switch to.
      (org-varde-test-reply "recall" "{\"v\":1,\"everywhere\":false,\"hits\":[]}")
      (save-window-excursion (org-varde-recall "nothing" nil "/p/app"))
      (should (equal (buffer-string) "Recall: nothing  in every project\n\nNothing found, here or in any other project.\n"))
      (should (equal (seq-filter (lambda (c) (string-prefix-p "recall" c)) (org-varde-test-calls))
                     '("recall --json --project /p/app token" "recall --json --all token"
                       "recall --json --project /p/app token" "recall --json --project /p/app nothing"))))))

;;; --- Parking -----------------------------------------------------------------

(ert-deftest org-varde-parking-closes-the-terminal ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "park" "{\"v\":1,\"stopped\":[]}")
    (org-varde-test--with-terminal
      (org-varde--park org-varde-test--id "t")
      (should (member (format "park %s --json" org-varde-test--id) (org-varde-test-calls)))
      (should-not (buffer-live-p terminal)))))

(defun org-varde-test--acts ()
  "The calls that change a task."
  (seq-filter (lambda (c) (string-match-p "^\\(done\\|park\\|reopen\\)" c)) (org-varde-test-calls)))

(ert-deftest org-varde-board-done-asks-about-a-busy-session-at-once ()
  "Asked as you press the key; Esc changes nothing."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (let ((org-varde-board-data
           '(:groups ((:key "working" :rows ((:task "AAA111" :status "working"))))))
          (asked nil))
      ;; Esc cancels.
      (cl-letf (((symbol-function 'read-char-choice)
                 (lambda (prompt &rest _) (setq asked prompt) ?\e)))
        (should (eq 'quit (condition-case nil (org-varde--finish "AAA111" "Busy one") (quit 'quit)))))
      (should (equal asked "Done \"Busy one\"? It is working. y stop, n leave running, Esc cancel "))
      (should-not (org-varde-test--acts))
      ;; Agreed to, it closes now, and once.
      (cl-letf (((symbol-function 'read-char-choice) (lambda (&rest _) ?y)))
        (org-varde--finish "AAA111" "Busy one"))
      (should (equal (org-varde-test--acts) '("done AAA111 --json"))))))

(ert-deftest org-varde-board-done-can-leave-a-busy-session-running ()
  "n marks the task done and leaves its session, and its terminal, running."
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
    (org-varde-test--with-terminal
      (let ((org-varde-board-data
             `(:groups ((:key "working" :rows ((:task ,org-varde-test--id :status "blocked" :busy t))))))
            asked)
        (cl-letf (((symbol-function 'read-char-choice) (lambda (prompt &rest _) (setq asked prompt) ?n))
                  ((symbol-function 'process-running-child-p) (lambda (&rest _) t)))
          (org-varde--finish org-varde-test--id "t")
          (org-varde-test-wait))
        (should (string-prefix-p "Done \"t\"? It is waiting for your answer." asked))
        (should (member (format "done %s --json --keep-running" org-varde-test--id) (org-varde-test-calls)))
        (should (buffer-live-p terminal))))))

(ert-deftest org-varde-busy-is-what-varde-says ()
  "A row's busy, when varde gives it, wins over its latest session's status."
  (org-varde-test-with-varde
    (let ((org-varde-board-data '(:groups ((:key "working" :rows ((:task "A" :status "waiting" :busy t)
                                                                  (:task "B" :status "working" :busy nil)
                                                                  (:task "C" :status "working")))))))
      (should (org-varde--busy-p "A"))
      (should-not (org-varde--busy-p "B"))
      (should (org-varde--busy-p "C")))))

;;; --- Review findings -------------------------------------------------------

(ert-deftest org-varde-json-reads-the-same-without-native-json ()
  "Emacs 29 can be built without libjansson."
  (org-varde-test-with-varde
    (let ((text "{\"v\":2,\"a\":[1,null,false],\"b\":{\"c\":true,\"d\":\"x\"}}"))
      (should (equal (org-varde--json text) '(:v 2 :a (1 nil nil) :b (:c t :d "x"))))
      (cl-letf (((symbol-function 'json-available-p) #'ignore))
        (should (equal (org-varde--json text) '(:v 2 :a (1 nil nil) :b (:c t :d "x"))))))))

(ert-deftest org-varde-watches-the-store-varde-uses ()
  (org-varde-test-with-varde
    (let ((process-environment (append '("VARDE_HOME" "XDG_STATE_HOME=relative/state") process-environment)))
      (should (equal (org-varde--home) (expand-file-name "~/.local/state/varde/")))
      (setenv "XDG_STATE_HOME" "/xdg")
      (should (equal (org-varde--home) "/xdg/varde/"))
      (setenv "VARDE_HOME" "/v")
      (should (equal (org-varde--home) "/v/")))))

(ert-deftest org-varde-only-store-writes-refresh-the-board ()
  (org-varde-test-with-varde
    (let (refreshed)
      (cl-letf (((symbol-function 'org-varde-refresh) (lambda () (setq refreshed t))))
        ;; What every `varde list' does to the directory: no refresh.
        (dolist (e '((1 created "/h/varde.db-wal") (1 created "/h/varde.db-shm")
                     (1 changed "/h/varde.db-shm") (1 deleted "/h/varde.db-wal")
                     (1 changed "/h/pending.jsonl")))
          (org-varde--on-store-change e))
        (should-not refreshed)
        (org-varde--on-store-change '(1 changed "/h/varde.db-wal"))
        (should refreshed)))))

(ert-deftest org-varde-watches-the-wal-itself ()
  "Under kqueue a directory's watch sees nothing written to a file in it:
not a hook's commit to the -wal a running session keeps open."
  (org-varde-test-with-varde
    (let ((wal (expand-file-name "varde.db-wal" org-varde-test-dir))
          watched refreshed)
      (cl-letf (((symbol-function 'file-notify-add-watch)
                 (lambda (file &rest _) (push (directory-file-name file) watched) file))
                ((symbol-function 'file-notify-rm-watch) #'ignore)
                ((symbol-function 'org-varde-refresh) (lambda () (setq refreshed t))))
        (write-region "" nil wal)
        (org-varde-mode 1)
        (should (member wal watched))
        (setq watched nil refreshed nil)
        ;; Made anew, by `varde list': watched anew, and nothing to show.
        (org-varde--on-store-change (list 1 'created wal))
        (should (equal watched (list wal)))
        (should-not refreshed)
        ;; Made anew and written before it was watched.
        (write-region "commit" nil wal)
        (org-varde--on-store-change (list 1 'created wal))
        (should refreshed)
        (org-varde-mode -1)))))

(ert-deftest org-varde-steady-store-writes-still-refresh ()
  "Refreshes asked for every 0.1 s fetch the board at least once a second."
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[]}")
    (let ((start (float-time)))
      (while (< (- (float-time) start) 1.5)
        (org-varde--on-store-change (list nil 'changed (expand-file-name "varde.db-wal" org-varde-test-dir)))
        (accept-process-output nil 0.1))
      (should (seq-find (lambda (c) (string-prefix-p "list" c)) (org-varde-test-calls))))))

(ert-deftest org-varde-a-newer-board-is-said-once ()
  (org-varde-test-with-varde
    (org-varde-test-reply "list" "{\"v\":3,\"groups\":[{\"key\":\"done\",\"rows\":[{\"task\":\"AAA111\",\"state\":\"done\"}]}]}")
    (let ((said 0))
      (cl-letf (((symbol-function 'message)
                 (lambda (fmt &rest args)
                   (when (string-match-p "newer" (apply #'format fmt args)) (setq said (1+ said))))))
        (dotimes (_ 3) (org-varde--list) (org-varde-test-wait)))
      (should (= said 1))
      (should (equal org-varde--too-new 3)))))

;;; --- Org as org users keep it ------------------------------------------------

(ert-deftest org-varde-a-heading-s-text-leaves-out-org-s-own-lines ()
  "State changes, notes and clocks logged under the heading are org's, not the task."
  (org-varde-test-with-varde
    (org-varde-test-with-org (concat "* TODO Fix [[https://x/issues/12][#12]] login [1/3]\n"
                                     "SCHEDULED: <2026-10-12 Mon>\n"
                                     "- State \"TODO\"       from \"DONE\"       [2026-10-12 Mon 10:00]\n"
                                     "- Note taken on [2026-10-11 Sun 09:00] \\\\\n"
                                     "  a note of my own\n"
                                     "CLOCK: [2026-10-11 Sun 09:00]--[2026-10-11 Sun 10:00] =>  1:00\n"
                                     "Real body.\n- a list of mine\n")
      (should (equal (org-varde--title) "Fix #12 login"))
      (should (equal (org-varde--body) "Real body.\n- a list of mine")))))

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

(ert-deftest org-varde-a-cairn-updated-while-edited-is-shown-and-the-edit-kept ()
  "A save refused because an update landed meanwhile loses nothing."
  (org-varde-test-with-varde
    (org-varde-test-reply "edit" "#: version 1\nStatus: \nFindings:\nDecisions:\nOpen:\n")
    (org-varde-edit-cairn org-varde-test--id)
    (with-current-buffer "*cairn xyz123*"
      (goto-char (point-max))
      (insert "- My own line\n")
      (org-varde-test-fail-once "edit" "varde: not saved: it was updated while you were editing: edit it again")
      (org-varde-test-reply "edit" "#: version 2\nStatus: New\nFindings:\nDecisions:\nOpen:\n")
      (should-error (org-varde-cairn-save) :type 'user-error)
      (should (string-prefix-p "#: version 2" (buffer-string)))
      (should-not (buffer-modified-p))
      (should (string-suffix-p "- My own line\n" (current-kill 0))))
    ;; Changes are not dropped by C-c C-k without a yes.
    (with-current-buffer "*cairn xyz123*"
      (insert "- Another\n")
      (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) nil)))
        (org-varde-cairn-cancel))
      (should (buffer-live-p (current-buffer))))))

(ert-deftest org-varde-a-project-cairn-is-shown-and-edited ()
  (org-varde-test-with-varde
    (let ((dir (file-name-as-directory org-varde-test-dir)))
      (org-varde-test-reply "show" "{\"v\":1,\"project\":\"/p/app\",\"version\":0,\"cairn\":null}")
      (save-window-excursion (org-varde-show-project dir))
      (should (member (format "show --project %s --json" dir) (org-varde-test-calls)))
      (with-current-buffer "*varde show*"
        ;; A project's cairn alone: its tab, and no task's.
        (should (eq org-varde--tab 'project))
        (should (string-match-p "\\`app\nproject cairn: what holds across app, handed to each new session\nNo project cairn yet"
                                (buffer-string)))
        ;; An empty section is its heading, dimmed.
        (should (string-match-p "^Gotchas$" (buffer-string)))
        (should (string-match-p "\\` Project    e edit" (substring-no-properties (org-varde--view-header))))
        (should-error (org-varde-show-next-tab) :type 'user-error)
        (should (eq (key-binding "E") #'org-varde-cairn-edit-all))
        (org-varde-test-reply "edit" "Conventions:\nGotchas:\nHow-to:\n")
        (save-window-excursion (org-varde-cairn-edit-all)))
      (should (member "edit --project /p/app --print" (org-varde-test-calls)))
      (with-current-buffer "*project cairn app*"
        (goto-char (point-max))
        (insert "- `make` builds it\n")
        (org-varde-cairn-save))
      (should (member "edit --project /p/app --stdin" (org-varde-test-calls)))
      (should (string-suffix-p "How-to:\n- `make` builds it\n" (org-varde-test-stdin "edit"))))))

(ert-deftest org-varde-a-view-has-a-task-tab-and-a-project-tab ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" (org-varde-test--both "Shared"))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (should (eq org-varde--tab 'task))
      (should (string-match-p "^Open\n  - Shared$" (buffer-string)))
      (should-not (string-match-p "^How-to" (buffer-string)))
      ;; ] and [ switch, each on its cairn's first line.
      (should (eq (key-binding "]") #'org-varde-show-next-tab))
      (should (eq (key-binding "[") #'org-varde-show-previous-tab))
      (org-varde-show-next-tab)
      (should (eq org-varde--tab 'project))
      (should (equal org-varde--editing '("--project" "/p/app")))
      (should (string-match-p "\\`app\nproject cairn: what holds across app" (buffer-string)))
      (should (string-match-p "^How-to\n  - Shared$" (buffer-string)))
      (should (equal (get-text-property (point) 'org-varde-item) '("howto" 2 "Shared")))
      (should (eq (get-text-property 0 'face (org-varde--view-header)) 'org-varde-quiet))
      ;; A project cairn has no Status; a line is added to its own sections.
      (should-error (org-varde-cairn-status) :type 'user-error)
      (cl-letf (((symbol-function 'read-multiple-choice)
                 (lambda (_prompt choices &rest _)
                   (should (equal (mapcar #'car choices) '(?c ?g ?h ?\r)))
                   (assq ?\r choices))))
        (should (equal (org-varde--pick-section) "howto")))
      (org-varde-show-previous-tab)
      (should (eq org-varde--tab 'task))
      (should (string-match-p "^Status" (buffer-string)))
      ;; v, as on the board, closes it.
      (should (eq (key-binding "v") #'quit-window)))))

(ert-deftest org-varde-a-view-steps-tasks-on-the-tab-it-is-on ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" (org-varde-test--both "Shared"))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (org-varde-show-next-tab)
      (org-varde--show-fill org-varde-test--id)
      (should (eq org-varde--tab 'project)))))

;;; --- Editing a cairn a line at a time ----------------------------------------

(defmacro org-varde-test--viewing (&rest body)
  "Show the task of `org-varde-test--shown', and run BODY in its view."
  (declare (indent 0))
  `(progn
     (org-varde-test-reply "show" org-varde-test--shown)
     (save-window-excursion (org-varde-show org-varde-test--id))
     (org-varde-test-wait)
     (with-current-buffer "*varde show*" ,@body)))

(ert-deftest org-varde-a-status-you-set-is-marked-as-yours ()
  (org-varde-test-with-varde
    (let ((org-varde-test--shown (string-replace "\"status\":\"Cause found.\","
                                                 "\"status\":\"Cause found.\",\"status_mine\":true,"
                                                 org-varde-test--shown)))
      (org-varde-test--viewing
        (should (string-match-p "^Status › Cause found.\n" (buffer-string)))))))

;; x on a Status you set hands it back to the updates; on one of theirs
;; it says so, and asks varde nothing.
(ert-deftest org-varde-a-status-you-set-is-handed-back ()
  (org-varde-test-with-varde
    (let ((org-varde-test--shown (string-replace "\"status\":\"Cause found.\","
                                                 "\"status\":\"Cause found.\",\"status_mine\":true,"
                                                 org-varde-test--shown)))
      (org-varde-test--viewing
        (goto-char (point-min))
        (search-forward "Status ›")
        (org-varde-test-reply "edit" (org-varde-test--cairn '((12 "x"))))
        (should (equal (car (org-varde-test--saying (org-varde-cairn-remove)))
                       "Status handed back: the next update may change it; u undoes"))
        (should (member "--rm status --json" (org-varde-test--edits)))
        (should (string-match-p "^Status  Cause found.\n" (buffer-string)))
        (goto-char (point-min))
        (search-forward "Status ")
        (should (equal (cadr (should-error (org-varde-cairn-remove) :type 'user-error))
                       "The Status is already the updates': e sets it as yours"))
        (should (= 1 (length (org-varde-test--edits))))
        ;; u makes it yours again, as it was.
        (should (equal (car (org-varde-test--saying (org-varde-cairn-undo))) "The Status is yours again"))
        (should (equal (car (last (org-varde-test--edits))) "--add status - --json"))
        (should (equal (org-varde-test-stdin "edit") "Cause found."))
        (should-error (org-varde-cairn-undo) :type 'user-error)))))

;; With nothing removed to bring back, u reopens the task the view
;; closed; a close there forgets a line removed before it.
(ert-deftest org-varde-u-in-a-view-reopens-the-task-it-closed ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
        (goto-char (point-min))
        (search-forward "omits")
        (org-varde-test-reply "edit" (org-varde-test--cairn nil))
        (org-varde-cairn-remove)
        (org-varde-board--note-closed (list (list :task org-varde-test--id :title "Fix the login bug")))
        (org-varde-cairn-undo)
        (should (member (concat "reopen " org-varde-test--id) (org-varde-test-calls)))
        (should-not (member "--restore 12 --json" (org-varde-test--edits)))
        (should-error (org-varde-cairn-undo) :type 'user-error)
        ;; Another task closed: nothing here to undo.
        (org-varde-board--note-closed (list (list :task "OTHER")))
        (should-error (org-varde-cairn-undo) :type 'user-error)))))

(defun org-varde-test--cairn (findings)
  "`varde edit --json' after a change: the cairn with FINDINGS, (ID TEXT)s."
  (concat "{\"v\":1,\"cairn\":{\"status\":\"Cause found.\",\"findings\":["
          (mapconcat (lambda (f) (format "{\"id\":%d,\"text\":\"%s\",\"mine\":true}" (car f) (cadr f)))
                     findings ",")
          "],\"decisions\":[],\"open\":[]}}"))

(defun org-varde-test--edits ()
  "The `varde edit' calls made, without the task."
  (mapcar (lambda (c) (string-remove-prefix (concat "edit " org-varde-test--id " ") c))
          (seq-filter (lambda (c) (string-prefix-p "edit " c)) (org-varde-test-calls))))

(ert-deftest org-varde-a-task-view-shows-its-cairn-first ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (let ((text (buffer-string)))
        ;; Its title, and where it stands on one line, without paths.
        (should (string-match-p "\\`Fix the login bug  xyz123\nopen · app +cairn: what its sessions learned, handed to the next one\n\nStatus  Cause found.\n" text))
        ;; A count only on a section full or nearly; marks as glyphs.
        (should (string-match-p "^Findings\n  - The key omits the locale. \\?\n" text))
        (should (string-match-p "^Decisions\nOpen\n  › Add the locale.\n" text))
        (should (< (string-match "^Findings" text) (string-match "^Context" text)))
        (should-not (string-match-p "/p/app" text)))
      ;; Its tabs head it, the one on view highlighted, then its keys.
      (let ((header (org-varde--view-header)))
        (should (string-match-p "\\` Task   Project    RET go  e edit  a add  x remove  \\[ \\] tabs  \\? help\\'"
                                (substring-no-properties header)))
        (should (eq (get-text-property 0 'face header) 'org-varde-tab)))
      ;; z folds the section at point, and keeps it folded on a refresh.
      (goto-char (point-min))
      (search-forward "Add the")
      (org-varde-show-toggle)
      (should (string-match-p "^Open ▸\n" (buffer-string)))
      (should-not (string-match-p "Add the locale" (buffer-string)))
      (should (equal (get-text-property (point) 'org-varde-section) "open"))
      (revert-buffer)
      (org-varde-test-wait)
      (should-not (string-match-p "Add the locale" (buffer-string)))
      ;; TAB on a heading folds it too, as in org.
      (should (equal (get-text-property (point) 'org-varde-section) "open"))
      (org-varde-show-tab)
      (should (string-match-p "Add the locale" (buffer-string))))))

(ert-deftest org-varde-a-view-says-how-its-cairn-is-used ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" (replace-regexp-in-string
                                  "\"cairn\":"
                                  (format "\"usage\":{\"updated\":%d,\"author\":\"job\",\"sessions\":3,\"handed\":1,\"stale\":1},\"cairn\":"
                                          (- (truncate (* 1000 (float-time))) 1200000))
                                  org-varde-test--shown t t))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (should (string-match-p "^open · app[^\n]*\nupdated 20m ago · handed to 3 sessions · 1 stale\n\nStatus"
                              (buffer-string))))))

(ert-deftest org-varde-a-line-of-the-cairn-is-edited-in-place ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (goto-char (point-min))
      (search-forward "omits")
      (org-varde-test-reply "edit" (org-varde-test--cairn '((12 "The key omits the locale and the zone."))))
      (let (offered asked)
        (cl-letf (((symbol-function 'read-string)
                   (lambda (prompt initial &rest _)
                     (setq offered initial asked prompt)
                     "The key omits the locale and the zone.")))
          (org-varde-cairn-edit))
        (should (equal asked "Edit line #12: "))
        (should (equal offered "The key omits the locale."))
        (should (equal (org-varde-test--edits) '("--set 12 - --json")))
        (should (equal (org-varde-test-stdin "edit") "The key omits the locale and the zone."))
        ;; The view shows what varde says the cairn now is, on the same line.
        (should (string-match-p "^  › The key omits the locale and the zone.$" (buffer-string)))
        (should (equal (cadr (get-text-property (point) 'org-varde-item)) 12))))))

(ert-deftest org-varde-a-line-is-added-to-the-section-you-pick ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (goto-char (point-min))
      (search-forward "Add the")
      (org-varde-test-reply "edit" (org-varde-test--cairn '((12 "x"))))
      (let (asked)
        (cl-letf (((symbol-function 'read-multiple-choice)
                   (lambda (prompt choices &rest _)
                     (should (equal prompt "Add to"))
                     ;; A key each, and RET for the section at point.
                     (should (equal (mapcar #'car choices) '(?f ?d ?o ?s ?\r)))
                     (should (equal (cadr (assq ?\r choices)) "Open"))
                     (assq ?d choices)))
                  ((symbol-function 'read-string)
                   (lambda (prompt &rest _) (setq asked prompt) "  Keep the old key for a release  ")))
          (call-interactively #'org-varde-cairn-add))
        (should (equal asked "Decisions · new line: "))
        (should (equal (org-varde-test--edits) '("--add decisions - --json")))
        (should (equal (org-varde-test-stdin "edit") "Keep the old key for a release")))
      ;; Off any section, RET is Findings.
      (goto-char (point-min))
      (cl-letf (((symbol-function 'read-multiple-choice) (lambda (_prompt choices &rest _) (assq ?\r choices))))
        (should (equal (org-varde--pick-section) "findings"))))))

(ert-deftest org-varde-a-refused-line-is-asked-again-with-what-you-wrote ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (org-varde-test-fail-once "edit" "varde: not saved: the line has 41 words; at most 40")
      (org-varde-test-reply "edit" (org-varde-test--cairn '((12 "x"))))
      (let (asked)
        (cl-letf (((symbol-function 'org-varde--read-line)
                   (lambda (prompt _cap initial note)
                     (push (list prompt initial note) asked)
                     (or initial "A long finding"))))
          (org-varde-cairn-add "findings"))
        (setq asked (nreverse asked))
        (should (equal (length asked) 2))
        ;; The same prompt, open with what you wrote, and why beside it.
        (should (equal (car (nth 1 asked)) "Findings · new line: "))
        (should (equal (nth 1 (nth 1 asked)) "A long finding"))
        (should (string-match-p "41 words" (nth 2 (nth 1 asked))))
        ;; Refused, the view was read again: it may have been behind.
        (should (= 3 (seq-count (lambda (c) (string-prefix-p "show " c)) (org-varde-test-calls))))))))

(ert-deftest org-varde-a-full-section-is-said-at-once ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" (replace-regexp-in-string
                                  "\"cairn\":" "\"limits\":{\"line\":20,\"sections\":{\"findings\":1}},\"cairn\":"
                                  org-varde-test--shown))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (org-varde-test-wait)
    (with-current-buffer "*varde show*"
      ;; Its caps are varde's, said where a section is full or nearly.
      (should (string-match-p "^Findings 1/1$" (buffer-string)))
      (should (string-match-p "^Open$" (buffer-string)))
      (cl-letf (((symbol-function 'read-string) (lambda (&rest _) (error "Asked"))))
        (should-error (org-varde-cairn-add "findings") :type 'user-error))
      ;; Full when varde says so, it is asked again saying why; quit,
      ;; what you wrote is kept.
      (org-varde-test-fail-once "edit" "varde: not saved: Open is full (5 of 5): change or remove one of its lines first")
      (let (asked cap)
        (cl-letf (((symbol-function 'org-varde--read-line)
                   (lambda (_prompt c initial note)
                     (setq cap c)
                     (push note asked)
                     (if (cdr asked)
                         (progn (setq org-varde--typed initial) (signal 'quit nil))
                       "One more question"))))
          (org-varde-cairn-add "open"))
        (should (= (length asked) 2))
        (should (string-match-p "Open is full" (car asked)))
        (should (= cap 20)))
      (should (equal (current-kill 0) "One more question")))))

(ert-deftest org-varde-a-line-being-written-is-never-lost ()
  "Quit, it is kept as a draft for what it was for, offered there next
time and yanked anywhere; cleared and quit, it is dropped."
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (let* ((offered nil)
             (typing (lambda (text)
                       (lambda (_prompt _cap initial _note)
                         (push initial offered)
                         (setq org-varde--typed text)
                         (signal 'quit nil)))))
        (cl-letf (((symbol-function 'org-varde--read-line) (funcall typing "Half a fin")))
          (org-varde-cairn-add "findings"))
        (should (equal (current-kill 0) "Half a fin"))
        (should-not (org-varde-test--edits))
        ;; Another line's draft is its own.
        (cl-letf (((symbol-function 'org-varde--read-line) (funcall typing "")))
          (org-varde-cairn-add "open"))
        (should (equal (car offered) nil))
        (cl-letf (((symbol-function 'org-varde--read-line) (funcall typing "")))
          (org-varde-cairn-add "findings"))
        (should (equal (car offered) "Half a fin"))
        ;; Cleared, it is dropped.
        (cl-letf (((symbol-function 'org-varde--read-line) (funcall typing "")))
          (org-varde-cairn-add "findings"))
        (should (equal (car offered) nil))
        ;; A line's draft, once the line is removed, is one to add.
        (goto-char (point-min))
        (search-forward "omits")
        (cl-letf (((symbol-function 'org-varde--read-line) (funcall typing "The key omits the zone")))
          (org-varde-cairn-edit))
        (org-varde-test-reply "edit" (org-varde-test--cairn nil))
        (org-varde-cairn-remove)
        (cl-letf (((symbol-function 'org-varde--read-line) (funcall typing "The key omits the zone")))
          (org-varde-cairn-add "findings"))
        (should (equal (car offered) "The key omits the zone"))))))

(ert-deftest org-varde-a-line-added-gets-point ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (goto-char (point-min))
      (org-varde-test-reply "edit" (org-varde-test--cairn '((12 "The key omits the locale.") (30 "A new one"))))
      (cl-letf (((symbol-function 'read-string) (lambda (&rest _) "A new one")))
        (org-varde-cairn-add "findings"))
      (should (equal (cadr (get-text-property (point) 'org-varde-item)) 30)))))

(defmacro org-varde-test--saying (&rest body)
  "Run BODY, and return what it said with `message', the latest first."
  (declare (indent 0))
  `(let (said)
     (cl-letf (((symbol-function 'message)
                (lambda (format &rest args) (when format (push (apply #'format-message format args) said)))))
       ,@body)
     said))

(ert-deftest org-varde-each-change-says-what-it-did-as-the-terminal ()
  "A change says what it did; a draft kept, how to take it up again."
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (org-varde-test-reply "edit" (org-varde-test--cairn '((12 "x"))))
      ;; The view opens on its first line, as the terminal's does.
      (should (equal (seq-take (get-text-property (point) 'org-varde-item) 2) '("findings" 12)))
      (cl-letf (((symbol-function 'read-string) (lambda (&rest _) "A new one")))
        (should (equal (car (org-varde-test--saying (org-varde-cairn-add "findings")))
                       "Added to Findings, as yours"))
        (should (equal (car (org-varde-test--saying (org-varde-cairn-status)))
                       "Status set, yours now: updates won't change it; x hands it back"))
        (goto-char (point-min))
        (search-forward "  › x")
        (should (equal (car (org-varde-test--saying (org-varde-cairn-edit))) "Changed line #12, as yours")))
      (let ((typing (lambda (&rest _) (setq org-varde--typed "Half") (signal 'quit nil))))
        (cl-letf (((symbol-function 'org-varde--read-line) typing))
          (should (equal (car (org-varde-test--saying (org-varde-cairn-add "decisions")))
                         "Draft kept for \"Fix the login bug\": a d continues it"))
          (should (equal (car (org-varde-test--saying (org-varde-cairn-status)))
                         "Draft kept for \"Fix the login bug\": e on the Status continues it"))
          (goto-char (point-min))
          (search-forward "  › x")
          (should (equal (car (org-varde-test--saying (org-varde-cairn-edit)))
                         "Draft kept for \"Fix the login bug\": e on the line continues it")))))))

(ert-deftest org-varde-an-empty-cairn-is-its-hint-without-a-status ()
  (org-varde-test-with-varde
    (let ((org-varde-test--shown (replace-regexp-in-string "\"cairn\":{.*}}\\'" "\"cairn\":null}"
                                                           org-varde-test--shown)))
      (org-varde-test--viewing
        (should (string-match-p "^No cairn yet: its first session's update writes one; a adds a line.\nFindings\n"
                                (buffer-string)))
        (should-not (string-match-p "^Status" (buffer-string)))))))

(ert-deftest org-varde-the-facts-line-is-the-row-s-meta ()
  (org-varde-test-with-varde
    (let ((org-varde-board-data
           `(:groups ((:key "needs_you"
                       :rows ((:task ,org-varde-test--id :status "waiting" :state "open" :agents 3 :shells 1
                               :git (:label "+12 −3" :stage "edited") :project "/p/app" :inbox t
                               :external "org"))))))
          (org-varde-test--shown (string-replace "\"worktree\":null"
                                                 "\"worktree\":{\"state\":\"active\",\"branch\":\"varde/fix\"}"
                                                 org-varde-test--shown)))
      (org-varde-test--viewing
        (should (string-match-p
                 "^your turn · 3 agents · 1 shell · \\+12 −3 · app · in the Inbox · worktree varde/fix · linked to org"
                 (buffer-string)))))))

(ert-deftest org-varde-a-removed-line-can-be-put-back ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (goto-char (point-min))
      (search-forward "omits")
      (org-varde-test-reply "edit" (org-varde-test--cairn nil))
      (org-varde-cairn-remove)
      (should-not (string-match-p "omits" (buffer-string)))
      (org-varde-test-reply "edit" (org-varde-test--cairn '((20 "The key omits the locale."))))
      (org-varde-cairn-undo)
      (should (string-match-p "omits" (buffer-string)))
      (should (equal (org-varde-test--edits)
                     '("--rm 12 --json" "--restore 12 --json")))
      (should-error (org-varde-cairn-undo) :type 'user-error)
      ;; The Status is set, not removed.
      (goto-char (point-min))
      (search-forward "Cause found")
      (should-error (org-varde-cairn-remove) :type 'user-error)
      (cl-letf (((symbol-function 'read-string)
                 (lambda (prompt initial &rest _)
                   (should (equal prompt "Status: "))
                   (should (equal initial "Cause found."))
                   "Fixed; tests pending")))
        (org-varde-cairn-edit))
      (should (equal (car (last (org-varde-test--edits))) "--add status - --json")))))

(ert-deftest org-varde-a-project-cairn-is-edited-a-line-at-a-time ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"project\":\"/p/app\",\"version\":3,\"cairn\":{\"gotchas\":[{\"id\":40,\"text\":\"Run make first.\"}]}}")
    (save-window-excursion (org-varde-show-project "/p/app/sub"))
    (with-current-buffer "*varde show*"
      (should (string-match-p "^Gotchas\n  - Run make first.\n" (buffer-string)))
      (should-error (org-varde-cairn-status) :type 'user-error)
      (goto-char (point-min))
      (search-forward "make")
      (org-varde-test-reply "edit" "{\"v\":1,\"cairn\":{\"gotchas\":[]}}")
      (org-varde-cairn-remove)
      (should (member "edit --project /p/app --rm 40 --json" (org-varde-test-calls))))))

;;; --- Moving between sessions ------------------------------------------------

(ert-deftest org-varde-next-needs-you-goes-round-the-tasks-that-need-you ()
  (org-varde-test-with-varde
    (setq org-varde-board-data
          (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"A1\",\"title\":\"One\"},{\"task\":\"B2\",\"title\":\"Two\"}]},{\"key\":\"open\",\"rows\":[{\"task\":\"C3\"}]}]}"))
    (let (went)
      (cl-letf (((symbol-function 'org-varde-go-task) (lambda (task &rest _) (push task went))))
        (with-temp-buffer (org-varde-next-needs-you))
        (with-temp-buffer (setq org-varde--task "A1") (org-varde-next-needs-you))
        (with-temp-buffer (setq org-varde--task "B2") (org-varde-next-needs-you))
        (with-temp-buffer (setq org-varde--task "C3") (org-varde-next-needs-you))
        ;; A link at point is not here: A1, unseen, is gone to.
        (org-varde-test-with-org "Note [[varde:A1][One]]\n"
          (search-forward "One")
          (org-varde-next-needs-you)))
      (should (equal (nreverse went) '("A1" "B2" "A1" "A1" "A1"))))
    (setq org-varde-board-data (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[]}]}"))
    (should-error (org-varde-next-needs-you) :type 'user-error)))

;; From a task's terminal, what you have not seen comes first; pressed
;; again it walks on from where it went, though it could not go there.
(ert-deftest org-varde-next-needs-you-walks-on-and-is-never-stuck ()
  (org-varde-test-with-varde
    (setq org-varde-board-data
          (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"A1\",\"last\":1},{\"task\":\"B2\",\"last\":2},{\"task\":\"C3\",\"last\":3,\"unread\":true}]}]}"))
    (let (went)
      (cl-letf (((symbol-function 'org-varde-go-task) (lambda (task &rest _) (push task went))))
        (with-temp-buffer
          (setq org-varde--task "A1")
          (org-varde-next-needs-you)
          ;; Again at once, from the same buffer: on from C3, round to A1,
          ;; which is here, so B2.
          (let ((last-command 'org-varde-next-needs-you))
            (org-varde-next-needs-you)
            (org-varde-next-needs-you)))
        (should (equal (nreverse went) '("C3" "B2" "C3")))
        ;; C3 runs outside Emacs: said once, then passed over.
        (setq went nil)
        (push '("C3" . 3) org-varde--outside)
        (with-temp-buffer (org-varde-next-needs-you))
        (should (equal went '("A1")))
        (with-temp-buffer
          (setq org-varde--task "A1")
          (org-varde-next-needs-you))
        (should (equal went '("B2" "A1")))
        ;; Once it moves on, it is tried again.
        (setq org-varde-board-data
              (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"C3\",\"last\":4,\"unread\":true}]}]}"))
        (with-temp-buffer (org-varde-next-needs-you))
        (should (equal (car went) "C3"))
        (setq org-varde-board-data
              (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"C3\",\"last\":3}]}]}"))
        (should (equal (cadr (should-error (with-temp-buffer (org-varde-next-needs-you)) :type 'user-error))
                       "Nothing else needs you that Emacs can go to: the rest run outside it"))))))

;; A running task is gone to, not started again; one already on screen
;; where it is, not shown twice.
(ert-deftest org-varde-going-to-a-terminal-in-sight-selects-its-window ()
  (org-varde-test-with-varde
    (org-varde-test-reply "start" "{\"v\":1,\"action\":\"focus\",\"run\":\"R1\"}")
    (org-varde-test--with-terminal
      (save-window-excursion
        (delete-other-windows)
        (let ((other (split-window)))
          (set-window-buffer other terminal)
          (org-varde-go-task org-varde-test--id "t")
          (org-varde-test-wait)
          (should (eq (selected-window) other))
          (should (= 1 (length (get-buffer-window-list terminal))))
          (should-not org-varde-test-sent))))))

(ert-deftest org-varde-a-session-of-your-own-runs-varde-s-agent ()
  (org-varde-test-with-varde
    (org-varde-test-reply "agent" "{\"v\":1,\"agent\":\"pi\",\"command\":[\"pi\",\"--model\",\"a b\"]}")
    (save-window-excursion (org-varde-new-session "/home/u/app/"))
    (should (equal (car org-varde-test-terminals) '("/home/u/app/" "*pi app*")))
    (should (equal org-varde-test-sent '("pi --model a\\ b")))
    ;; Asked once.
    (save-window-excursion (org-varde-new-session "/home/u/app/"))
    (should (= 1 (seq-count (lambda (c) (string-prefix-p "agent" c)) (org-varde-test-calls))))
    ;; Your own command wins.
    (let ((org-varde-agent-command "claude --model opus"))
      (save-window-excursion (org-varde-new-session "/home/u/app/")))
    (should (equal (car org-varde-test-sent) "claude --model opus"))
    (should (equal (cadr (car org-varde-test-terminals)) "*claude app*"))))

(ert-deftest org-varde-view-keys-under-evil ()
  "Run with evil on the load path to check this; skipped otherwise."
  (skip-unless (require 'evil nil t))
  (evil-mode 1)
  (unwind-protect
      (org-varde-test-with-varde
        (org-varde-test--viewing
          (switch-to-buffer (current-buffer))
          (evil-normal-state)
          (should (eq (key-binding "e") #'org-varde-cairn-edit))
          (should (eq (key-binding "a") #'org-varde-cairn-add))
          (should (eq (key-binding "x") #'org-varde-cairn-remove))
          (should (eq (key-binding "o") #'org-varde-cairn-add-here))
          (should (eq (key-binding "u") #'org-varde-cairn-undo))
          (should (eq (key-binding "d") #'org-varde-board-done))
          (should (eq (key-binding (kbd "TAB")) #'org-varde-show-tab))
          ;; K opened the view, and closes it; C-j and C-k step tasks.
          (should (eq (key-binding "K") #'quit-window))
          (should-not (eq (key-binding "J") #'org-varde-show-next-task))
          (should (eq (key-binding (kbd "C-j")) #'org-varde-show-next-task))
          (should (eq (key-binding (kbd "C-k")) #'org-varde-show-previous-task))
          (should (org-varde--evil-p))
          (should (eq (key-binding "za") #'org-varde-show-toggle))
          (should (eq (key-binding "gr") #'revert-buffer))
          (should (eq (key-binding "gg") #'org-varde-show-first))
          (should (eq (key-binding "G") #'org-varde-show-last))
          (should (eq (key-binding "j") #'org-varde-show-next))
          (should (eq (key-binding "k") #'org-varde-show-previous))
          (should (eq (key-binding (kbd "C-d")) #'evil-scroll-down))
          ;; Not evil's repeat, nor its motions.
          (should (eq (key-binding ".") #'org-varde-board-toggle-project))
          (should (eq (key-binding "X") #'org-varde-board-drop))
          (should (eq (key-binding "]") #'org-varde-show-next-tab))
          (should (eq (key-binding "[") #'org-varde-show-previous-tab))))
    (evil-mode -1)))

(ert-deftest org-varde-edit-buffers-under-evil ()
  "Run with evil on the load path to check this; skipped otherwise.
They start in insert state, and :w, ZZ and ZQ do what they say."
  (skip-unless (require 'evil nil t))
  (evil-mode 1)
  (unwind-protect
      (org-varde-test-with-varde
        (org-varde-test-reply "edit" "Open:\n")
        (save-window-excursion
          (org-varde-edit-cairn org-varde-test--id)
          (with-current-buffer "*cairn xyz123*"
            (should (evil-insert-state-p))
            (should (eq (evil-ex-binding "w") #'org-varde-cairn-save))
            (should (eq (evil-ex-binding "wq") #'org-varde-cairn-save))
            (should (eq (evil-ex-binding "q") #'org-varde-cairn-cancel))
            (evil-normal-state)
            (should (eq (key-binding "ZZ") #'org-varde-cairn-save))
            (should (eq (key-binding "ZQ") #'org-varde-cairn-cancel))
            ;; Its header says its keys as evil has them.
            (should (string-match-p ":w save  :q cancel" header-line-format))))
        (with-current-buffer (get-buffer-create org-varde-board-buffer)
          (org-varde-board-mode)
          (save-window-excursion
            (call-interactively #'org-varde-board-create)
            (with-current-buffer "*varde new*"
              (should (evil-insert-state-p))
              (should (eq (evil-ex-binding "w") #'org-varde-new-create))
              (should (eq (evil-ex-binding "q") #'org-varde-new-cancel))))))
    (evil-mode -1)))

(ert-deftest org-varde-view-keys-are-the-terminal-view-s ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (should (eq (key-binding "x") #'org-varde-cairn-remove))
      (should (eq (key-binding "o") #'org-varde-cairn-add-here))
      (should (eq (key-binding "J") #'org-varde-show-next-task))
      (should (eq (key-binding "K") #'org-varde-show-previous-task))
      (should (eq (key-binding (kbd "TAB")) #'org-varde-show-tab))
      (should (eq (key-binding (kbd "<backtab>")) #'org-varde-show-previous-needs-you))
      (should (eq (key-binding "z") #'org-varde-show-toggle))
      (should (eq (key-binding "=") #'org-varde-board-diff))
      (should (eq (key-binding "j") #'org-varde-show-next))
      (should (eq (key-binding (kbd "M-<")) #'org-varde-show-first))
      ;; Every key its menu lists.
      (should (eq (key-binding "X") #'org-varde-board-drop))
      (should (eq (key-binding ".") #'org-varde-board-toggle-project))
      (should (eq (key-binding "v") #'quit-window))
      ;; The board's keys act on the task shown.
      (should (eq (key-binding "d") #'org-varde-board-done))
      (should (eq (key-binding "s") #'org-varde-board-start-background))
      (should (equal (org-varde--view-row)
                     (list :task org-varde-test--id :short "xyz123" :title "Fix the login bug"
                           :project "/p/app" :state "open")))
      ;; o on a line adds to its section.
      (goto-char (point-min))
      (search-forward "Add the")
      (org-varde-test-reply "edit" (org-varde-test--cairn nil))
      (cl-letf (((symbol-function 'read-string) (lambda (&rest _) "Ask about zones")))
        (org-varde-cairn-add-here))
      (should (member "--add open - --json" (org-varde-test--edits))))))

(ert-deftest org-varde-a-view-steps-through-the-tasks ()
  (org-varde-test-with-varde
    (setq org-varde-board-data
          (org-varde--json (format "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"A1\"},{\"task\":\"%s\"}]},{\"key\":\"open\",\"rows\":[{\"task\":\"C3\"}]}]}" org-varde-test--id)))
    (org-varde-test--viewing
      (let (shown)
        (cl-letf (((symbol-function 'org-varde--show-fill) (lambda (task) (push task shown))))
          (org-varde-show-next-task 1)
          (org-varde-show-previous-task 1)
          (org-varde-show-next-needs-you 1)
          (org-varde-show-previous-needs-you 1)
          ;; TAB off a section's heading, as the key gives it, with no count.
          (org-varde-show-tab))
        (should (equal (nreverse shown) '("C3" "A1" "A1" "A1" "A1")))
        (cl-letf (((symbol-function 'org-varde--show-fill) #'ignore))
          (setq org-varde--task "C3")
          (should-error (org-varde-show-next-task 1) :type 'user-error))))))

(ert-deftest org-varde-a-view-opened-by-short-id-keeps-the-full-one ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" org-varde-test--shown)
    (save-window-excursion (org-varde-show "xyz123"))
    (org-varde-test-wait)
    (with-current-buffer "*varde show*"
      (should (equal org-varde--task org-varde-test--id))
      ;; So git, which follows, is shown.
      (should (plist-member org-varde--shown :git)))))

(ert-deftest org-varde-a-view-in-sight-follows-varde ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (switch-to-buffer (current-buffer))
      (org-varde-test-reply "show" (replace-regexp-in-string "Cause found" "Fixed" org-varde-test--shown))
      (org-varde--refresh-views)
      (org-varde-test-wait)
      (should (string-match-p "Status  Fixed\\." (buffer-string)))
      (should (member (format "show %s --json --no-git" org-varde-test--id) (org-varde-test-calls))))))

(ert-deftest org-varde-going-to-a-closed-task-asks-first ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" (replace-regexp-in-string "\"open\"," "\"done\"," org-varde-test--shown))
    (save-window-excursion (org-varde-show org-varde-test--id))
    (with-current-buffer "*varde show*"
      (let (asked went)
        (cl-letf (((symbol-function 'y-or-n-p) (lambda (prompt) (setq asked prompt) nil))
                  ((symbol-function 'org-varde-go-task) (lambda (&rest _) (setq went t))))
          (should-error (org-varde-show-go) :type 'user-error)
          (should (equal asked "\"Fix the login bug\" is done: reopen and start it? "))
          (should-not went))))))

(ert-deftest org-varde-a-session-at-work-in-the-background-is-taken-over-when-asked ()
  "A session its schedule started has no terminal: going to it asks
whether to take it over, and starts it again with that answer."
  (org-varde-test-with-varde
    (org-varde-test-reply-once "start" "{\"v\":1,\"action\":\"ask\",\"ask\":\"takeover\",\"run\":\"R1\"}")
    (org-varde-test--run-plan)
    (let (asked)
      (cl-letf (((symbol-function 'y-or-n-p) (lambda (q) (setq asked q) t)))
        (save-window-excursion
          (org-varde-go-task org-varde-test--id "Triage")
          (org-varde-test-wait)))
      (should (string-match-p "is working in the background" asked)))
    (should (seq-find (lambda (c) (string-suffix-p "--take-over" c)) (org-varde-test-calls)))
    (should (equal org-varde-test-sent '("varde run R1")))))

(ert-deftest org-varde-a-varde-that-exits-early-is-answered-once ()
  (org-varde-test-with-varde
    (let ((org-varde-varde-program "true") (n 0))
      (org-varde--varde-async '("x") (lambda (_) (setq n (1+ n))) (lambda () (setq n (1+ n)))
                              (make-string 2000000 ?x))
      (let ((until (+ (float-time) 3)))
        (while (and (= n 0) (< (float-time) until)) (accept-process-output nil 0.05)))
      ;; A second answer would come right after the first.
      (accept-process-output nil 0.3)
      (should (= n 1)))))

(ert-deftest org-varde-closing-a-task-workspace-keeps-your-buffers ()
  (org-varde-test-with-varde
    (let ((org-varde-workspaces t) killed)
      (cl-letf (((symbol-function '+workspace-switch) #'ignore)
                ((symbol-function '+workspace-list-names) (lambda () '("main" "xyz123 t")))
                ((symbol-function '+workspace-kill) (lambda (&rest args) (setq killed args))))
        (org-varde--workspace-close org-varde-test--id))
      (should (equal killed '("xyz123 t" t))))))

(ert-deftest org-varde-varde-links-open-the-task ()
  (org-varde-test-with-varde
    (let (shown)
      (cl-letf (((symbol-function 'org-varde-show) (lambda (task) (setq shown task))))
        (org-link-open-from-string "[[varde:xyz123]]"))
      (should (equal shown "xyz123")))))

;;; --- Round 3 --------------------------------------------------------------

(ert-deftest org-varde-a-busy-session-is-said-as-it-is-busy ()
  (org-varde-test-with-varde
    (should (equal (org-varde--busy-words '(:status "blocked")) "is waiting for your answer"))
    (should (equal (org-varde--busy-words '(:status "working" :agents 2)) "is working"))
    ;; A turn that ended with work left running.
    (should (equal (org-varde--busy-words '(:status "waiting" :agents 3 :shells 1))
                   "has 3 agents and a shell running"))
    (should (equal (org-varde--busy-words '(:status "waiting" :agents 1)) "has an agent running"))))

(ert-deftest org-varde-varde-links-are-stored-and-completed ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (let ((org-store-link-plist nil))
        (should (org-varde--store-link))
        (should (equal (plist-get org-store-link-plist :link) "varde:xyz123"))
        (should (equal (plist-get org-store-link-plist :description) "Fix the login bug"))))
    ;; On the board, and in a task's terminal.
    (setq org-varde-board-data (org-varde-test--board '("xyz123" "waiting" "open")))
    (with-temp-buffer
      (org-varde-board-mode)
      (let ((inhibit-read-only t))
        (insert (propertize "row" 'org-varde-row (org-varde--row "T00000000000000000000XYZ123"))))
      (goto-char (point-min))
      (let ((org-store-link-plist nil))
        (should (org-varde--store-link))
        (should (equal (plist-get org-store-link-plist :link) "varde:xyz123"))))
    (org-varde-test--with-terminal
      (with-current-buffer terminal
        (setq org-varde--task "T00000000000000000000XYZ123")
        (let ((org-store-link-plist nil))
          (should (org-varde--store-link))
          (should (equal (plist-get org-store-link-plist :description) "Task xyz123")))))
    ;; Elsewhere, org stores its own links.
    (with-temp-buffer (should-not (org-varde--store-link)))
    ;; Nothing fetched yet: C-c C-l varde: TAB asks varde, once.
    (setq org-varde-board-data nil)
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"A1\",\"short\":\"aaa111\",\"title\":\"One\",\"state\":\"open\"}]}]}")
    (cl-letf (((symbol-function 'completing-read) (lambda (_prompt table &rest _) (car (all-completions "" table)))))
      (should (equal (org-varde--complete-link) "varde:aaa111")))
    (should (equal (car (last (org-varde-test-calls))) "list --json --all"))))

(ert-deftest org-varde-a-link-is-put-in-by-picking-a-title ()
  (org-varde-test-with-varde
    (setq org-varde-board-data
          (org-varde-test--board '("aaa111" "waiting" "open" ",\"project\":\"/src/app\",\"last\":1")
                                 '("ddd444" "done" "done" ",\"project\":\"/src/web\",\"last\":1"))
          ;; Every task's rows fetched too: nothing is run to offer them.
          org-varde--all (make-hash-table :test #'equal))
    (let* (seen meta
           (pick (lambda (want)
                   (lambda (_prompt table &rest _)
                     (setq seen (all-completions "" table) meta (cdr (funcall table "" nil 'metadata)))
                     (seq-find (lambda (c) (string-prefix-p want c)) seen)))))
      (with-temp-buffer
        (org-mode)
        ;; Nothing waited for: every task's rows are fetched again for
        ;; next time, in the background.
        (cl-letf (((symbol-function 'completing-read) (funcall pick "Task ddd444")))
          (should (= 1 (org-varde-test--spawning (org-varde-insert-link)))))
        (should (equal (buffer-string) "[[varde:ddd444][Task ddd444]]"))
        ;; Open tasks first, then closed ones, then the projects' cairns.
        (should (equal (mapcar #'substring-no-properties seen)
                       '("Task aaa111  aaa111" "Task ddd444  ddd444"
                         "app · project cairn  /src/app" "web · project cairn  /src/web")))
        (should (equal (mapcar (lambda (c) (funcall (alist-get 'group-function meta) c nil)) seen)
                       '("Open tasks" "Closed tasks" "Project cairns" "Project cairns")))
        ;; Each says where it stands, its project and how long ago.
        (let ((first (car (funcall (alist-get 'affixation-function meta) seen))))
          (should (equal (substring-no-properties (nth 1 first)) "○ "))
          (should (string-match-p "your turn · app · [0-9]+d\\'" (substring-no-properties (nth 2 first)))))
        ;; A project's cairn, which its link opens on the view's Project tab.
        (erase-buffer)
        (cl-letf (((symbol-function 'completing-read) (funcall pick "app · project")))
          (org-varde-insert-link))
        (should (equal (buffer-string) "[[varde:project:/src/app][app · project cairn]]"))
        (let (shown)
          (cl-letf (((symbol-function 'org-varde-show-project) (lambda (dir) (setq shown dir))))
            (goto-char 3)
            (org-open-at-point))
          (should (equal shown "/src/app")))
        ;; It has no status, and is no task of the heading's.
        (should-not (org-varde-test--statuses))
        (should-not (org-varde--link-at-point)))
      ;; Outside org: Markdown's form, else the bare link.
      (with-temp-buffer
        (cl-letf (((symbol-function 'completing-read) (funcall pick "Task aaa111")))
          (org-varde-insert-link)
          (insert " ")
          (cl-letf (((symbol-function 'derived-mode-p) (lambda (&rest modes) (memq 'markdown-mode modes))))
            (org-varde-insert-link)))
        (should (equal (buffer-string) "varde:aaa111 [Task aaa111](varde:aaa111)"))))
    (should (equal (delete-dups (org-varde-test-calls)) '("list --json --all")))))

;; Typed at the prompt: in any case, and SPC is a space, as in the
;; title, not a completion that rewrites what was typed.
(ert-deftest org-varde-a-title-is-typed-as-it-reads ()
  (org-varde-test-with-varde
    (setq org-varde-board-data
          (org-varde-test--board '("aaa111" "waiting" "open") '("ddd444" "waiting" "open"))
          org-varde--all (make-hash-table :test #'equal))
    (save-window-excursion
      (switch-to-buffer (get-buffer-create "*varde typed*"))
      (org-mode)
      (execute-kbd-macro (kbd "M-x org-varde-insert-link RET task SPC ddd TAB RET"))
      (should (equal (buffer-string) "[[varde:ddd444][Task ddd444]]")))))

(ert-deftest org-varde-y-copies-a-link-on-the-board-and-in-the-view ()
  (org-varde-test-with-varde
    (setq org-varde-board-data (org-varde-test--board '("xyz123" "waiting" "open")))
    (with-temp-buffer
      (org-varde-board-mode)
      (let ((inhibit-read-only t))
        (insert (propertize "row" 'org-varde-row (org-varde--row "T00000000000000000000XYZ123"))))
      (goto-char (point-min))
      (should (eq (key-binding "y") #'org-varde-copy-link))
      (let ((org-stored-links nil))
        (org-varde-copy-link)
        (should (equal (current-kill 0) "[[varde:xyz123][Task xyz123]]"))
        ;; C-c C-l offers it too.
        (should (equal org-stored-links '(("varde:xyz123" "Task xyz123"))))))
    (org-varde-test--viewing
      (should (eq (key-binding "y") #'org-varde-copy-link))
      (org-varde-copy-link)
      (should (equal (current-kill 0) "[[varde:xyz123][Fix the login bug]]"))
      ;; On the Project tab, its project's cairn.
      (org-varde-test-reply "show" "{\"v\":1,\"project\":\"/p/app\",\"cairn\":{}}")
      (org-varde-show-next-tab)
      (org-varde-copy-link)
      (should (equal (current-kill 0) "[[varde:project:/p/app][app · project cairn]]")))))

(ert-deftest org-varde-the-cairn-as-text-starts-on-its-status ()
  (org-varde-test-with-varde
    (org-varde-test-reply "edit" "#: version 3\n#: what this is\nStatus: Cause found.\nFindings:\n")
    (save-window-excursion (org-varde-edit-cairn org-varde-test--id))
    (with-current-buffer "*cairn xyz123*"
      (unwind-protect
          (progn
            (should (equal (buffer-substring (line-beginning-position) (point)) "Status: Cause found."))
            ;; varde's notes cannot be changed; the lines below them can.
            (goto-char (+ (point-min) 3))
            (should-error (insert "x") :type 'text-read-only)
            (forward-line 2)
            (beginning-of-line)
            (insert "x")
            (delete-char -1)
            ;; A refusal reads as varde wrote it.
            (org-varde-test-fail-once "edit" "varde: not saved: Findings has 8 lines; at most 7")
            (should (equal (cadr (should-error (org-varde-cairn-save) :type 'user-error))
                           "varde: not saved: Findings has 8 lines; at most 7")))
        (set-buffer-modified-p nil)
        (kill-buffer)))))

(ert-deftest org-varde-an-edit-you-keep-stays-in-view ()
  (org-varde-test-with-varde
    (org-varde-test-reply "edit" "Status: \n")
    (save-window-excursion
      (org-varde-edit-cairn org-varde-test--id)
      (with-current-buffer "*cairn xyz123*"
        (unwind-protect
            (progn
              (insert "x")
              (cl-letf (((symbol-function 'yes-or-no-p) #'ignore))
                (org-varde-cairn-cancel))
              (should (eq (window-buffer) (current-buffer)))
              (should (buffer-modified-p)))
          (set-buffer-modified-p nil)
          (kill-buffer))))))

(ert-deftest org-varde-done-says-what-it-did ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
    (org-varde-test-reply "done" "{\"v\":1,\"worktree\":{\"action\":\"kept\",\"branch\":\"varde/x\"}}")
    (cl-letf (((symbol-function 'org-varde-refresh) #'ignore))
      (should (member "Done \"Finish it\"; varde/x is kept until its work lands, else put away a day from now"
                      (org-varde-test--saying (org-varde--finish "AAA111" "Finish it") (org-varde-test-wait))))
      ;; From the board, where u reopens it.
      (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
      (let ((org-varde--on-board t))
        (should (member "Done \"Finish it\"; u reopens"
                        (org-varde-test--saying (org-varde--finish "AAA111" "Finish it") (org-varde-test-wait))))))))

(ert-deftest org-varde-the-view-opens-below-and-says-where-there-is-no-task ()
  (org-varde-test-with-varde
    (org-varde-test-reply "show" org-varde-test--shown)
    (save-window-excursion
      (delete-other-windows)
      (let ((board (selected-window)))
        (org-varde-show org-varde-test--id)
        (should (eq (window-buffer) (get-buffer "*varde show*")))
        ;; Below the window it was opened from, most of the frame.
        (should (eq (window-in-direction 'above) board))
        (should (> (window-total-height) (window-total-height board)))))
    (with-temp-buffer
      (org-mode)
      (insert "* A heading\n")
      (goto-char (point-min))
      (should (equal (cadr (should-error (org-varde-show) :type 'user-error))
                     "No varde task here; org-varde-new-here makes one")))))

(ert-deftest org-varde-tab-in-a-view-says-where-the-rest-are ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (setq org-varde-board-data
            (org-varde--json "{\"v\":2,\"groups\":[{\"key\":\"needs_you\",\"rows\":[{\"task\":\"A1\",\"project\":\"/p/other\"}]}]}"))
      (let ((org-varde--kept "/p/app") said)
        (cl-letf (((symbol-function 'message) (lambda (f &rest a) (setq said (apply #'format f a)))))
          (org-varde-show-next-needs-you 1))
        (should (equal said "Nothing here needs you; 1 in other projects: . shows them"))))))

(ert-deftest org-varde-a-term-keeps-your-keys-for-org-varde ()
  "In term's char mode, C-c is its escape, where C-c v is C-x v: the keys
under C-c you bound org-varde's commands to are kept, at any depth and
with a label, and a prefix that holds one is kept whole."
  (require 'term)
  (org-varde-test-with-varde
    (let* ((org-varde-terminals '(term))
           (keys '("C-c a" "C-c b" "C-c d" "C-c o"))
           (was (mapcar (lambda (k) (keymap-lookup global-map k)) keys)))
      (keymap-set global-map "C-c a" (define-keymap "n" #'org-varde-next-needs-you "m" #'ignore))
      (keymap-set global-map "C-c b" #'org-varde-board)
      (keymap-set global-map "C-c d" (define-keymap "s" (define-keymap "v" '(menu-item "View" org-varde-show))))
      (keymap-set global-map "C-c o" #'ignore)
      (unwind-protect
          (cl-letf (((symbol-function 'make-term) (lambda (name _program) (get-buffer-create (concat "*" name "*"))))
                    ((symbol-function 'term-char-mode) (lambda () (use-local-map term-raw-map))))
            (with-current-buffer (org-varde--open-shell "/tmp" "*varde t*")
              (should (eq (key-binding (kbd "C-c a n")) #'org-varde-next-needs-you))
              (should (eq (key-binding (kbd "C-c a m")) #'ignore))
              (should (eq (key-binding (kbd "C-c b")) #'org-varde-board))
              (should (eq (key-binding (kbd "C-c d s v")) #'org-varde-show))
              ;; Your other keys there are term's.
              (should-not (eq (key-binding (kbd "C-c o")) #'ignore))
              (should (eq (key-binding (kbd "C-c C-j")) #'term-line-mode))))
        (cl-mapc (lambda (k w) (keymap-set global-map k w)) keys was)))))

;;; --- Org through links -------------------------------------------------------

(defun org-varde-test--board (&rest rows)
  "A board of ROWS, each (SHORT STATUS STATE &rest MORE), in one group."
  (org-varde--json
   (format "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[%s]}]}"
           (mapconcat (lambda (r)
                        (format "{\"task\":\"T00000000000000000000%s\",\"short\":\"%s\",\"title\":\"Task %s\",\"status\":\"%s\",\"state\":\"%s\"%s}"
                                (upcase (car r)) (car r) (car r) (nth 1 r) (nth 2 r) (or (nth 3 r) "")))
                      rows ","))))

(defmacro org-varde-test--spawning (&rest body)
  "Run BODY, and return how many processes it started."
  (declare (indent 0))
  `(let ((spawned 0))
     (cl-letf* ((make (symbol-function 'make-process))
                (call (symbol-function 'call-process))
                ((symbol-function 'make-process) (lambda (&rest a) (cl-incf spawned) (apply make a)))
                ((symbol-function 'call-process) (lambda (&rest a) (cl-incf spawned) (apply call a))))
       ,@body)
     spawned))

(defun org-varde-test--statuses ()
  "What the links of this buffer say after themselves, in order."
  (font-lock-ensure)
  (mapcar (lambda (o) (substring-no-properties (overlay-get o 'after-string)))
          (sort (seq-filter (lambda (o) (overlay-get o 'org-varde-link)) (overlays-in (point-min) (point-max)))
                (lambda (a b) (< (overlay-start a) (overlay-start b))))))

(ert-deftest org-varde-a-link-says-where-its-task-stands-and-runs-nothing ()
  (org-varde-test-with-varde
    (setq org-varde-board-data
          (org-varde-test--board '("aaa111" "working" "open") '("bbb222" "waiting" "open")
                                 '("ccc333" "blocked" "open") '("ddd444" "done" "done")
                                 '("eee555" "open" "open")))
    (with-temp-buffer
      (org-mode)
      (insert "* Notes\n[[varde:aaa111][One]] and varde:bbb222\n- [[varde:ccc333]]\n- [[varde:ddd444][Four]]\n"
              "- [[varde:T00000000000000000000EEE555][Five]]\n"
              ;; In a table, as a review's, no status: its glyph has a column.
              "| ● | [[varde:aaa111][One]] |\n")
      (should (= 0 (org-varde-test--spawning
                     (should (equal (org-varde-test--statuses)
                                    '(" ● working" " ○ your turn" " ● needs input" " ✓ done" " ○ open"))))))
      ;; The board moves on: only a buffer whose links changed is drawn again.
      (let ((flushed 0))
        (cl-letf (((symbol-function 'font-lock-flush) (lambda (&rest _) (cl-incf flushed))))
          (setq org-varde-board-data (copy-tree org-varde-board-data))
          (org-varde--links-refresh t)
          (should (= flushed 0))
          (setq org-varde-board-data
                (org-varde-test--board '("aaa111" "waiting" "open") '("bbb222" "waiting" "open")
                                       '("ccc333" "blocked" "open") '("ddd444" "done" "done")
                                       '("eee555" "open" "open")))
          (org-varde--links-refresh t)
          (should (= flushed 1))))
      (font-lock-flush)
      (should (equal (car (org-varde-test--statuses)) " ○ your turn"))
      ;; Edited, a link's status goes, until it is drawn again.
      (goto-char (point-min))
      (search-forward "One")
      (insert "s")
      (should-not (seq-some (lambda (o) (overlay-get o 'org-varde-link)) (overlays-at (1- (point))))))))

(ert-deftest org-varde-a-link-to-a-task-off-the-board-is-fetched-once-from-an-idle-timer ()
  (org-varde-test-with-varde
    (setq org-varde-board-data (org-varde-test--board '("aaa111" "working" "open")))
    (org-varde-test-reply "list" (concat "{\"v\":2,\"groups\":[{\"key\":\"dropped\",\"rows\":"
                                         "[{\"task\":\"T1\",\"short\":\"old999\",\"status\":\"dropped\",\"state\":\"dropped\"}]}]}"))
    (with-temp-buffer
      (org-mode)
      (insert "[[varde:old999][Gone]] [[varde:aaa111][Here]] [[varde:zzz000][Never]]\n")
      (should (= 0 (org-varde-test--spawning
                     (should (equal (org-varde-test--statuses) '(" ● working"))))))
      ;; Asked for once, from an idle timer, not from font-lock.
      (should (timerp org-varde--all-due))
      (should-not (org-varde-test-calls))
      (org-varde--list-all)
      (org-varde-test-wait)
      (should (equal (org-varde-test-calls) '("list --json --all")))
      (should (equal (org-varde-test--statuses) '(" ✕ dropped" " ● working")))
      ;; A task unknown everywhere says nothing, and asks no more.
      (should-not org-varde--all-due))))

(ert-deftest org-varde-a-view-says-where-its-task-came-from ()
  (org-varde-test-with-varde
    (let ((org-varde-test--shown
           (string-replace "\"body\":\"It fails on CI only.\""
                           "\"body\":\"It fails on CI only.\\n\\nFrom: [[file:/n/auth.org::*SSO][auth.org › Auth › SSO]]\""
                           org-varde-test--shown)))
      (org-varde-test--viewing
        (goto-char (point-min))
        (search-forward "From  auth.org › Auth › SSO")
        (should-not (string-match-p "\\[\\[file:" (buffer-string)))
        (beginning-of-line)
        ;; A stop for the moves, and RET goes there.
        (should (org-varde--view-stop-p))
        (let (opened)
          (cl-letf (((symbol-function 'org-link-open-from-string) (lambda (link) (setq opened link))))
            (org-varde-show-go))
          (should (equal opened "[[file:/n/auth.org::*SSO][auth.org › Auth › SSO]]")))
        (should-not (seq-find (lambda (c) (string-prefix-p "start" c)) (org-varde-test-calls)))))))

(defmacro org-varde-test--marking-done (rows text answer &rest body)
  "With the board's ROWS, mark the first heading of org TEXT done,
answering the question with ANSWER (y or n), then run BODY.  ASKED is
the question, or nil."
  (declare (indent 3))
  `(progn
     (setq org-varde-board-data (apply #'org-varde-test--board ,rows))
     (org-varde-test-reply "show" "{\"v\":1,\"state\":\"open\"}")
     (org-varde-test-reply "done" "{\"v\":1,\"worktree\":null}")
     (org-varde-test-with-org ,text
       (add-hook 'org-after-todo-state-change-hook #'org-varde--on-done nil t)
       ;; No note logged: org would wait for it on the global
       ;; post-command-hook, and the next test's first command would
       ;; take it.
       (let (asked org-log-done org-log-repeat)
         (cl-letf (((symbol-function 'y-or-n-p) (lambda (prompt) (setq asked prompt) (eq ,answer ?y))))
           (org-todo "DONE")
           (org-varde-test-wait))
         ,@body))))

(defconst org-varde-test--shipping
  "* TODO Ship SSO\n[[varde:aaa111][Fix login]] and [[varde:bbb222][Docs]], [[varde:ddd444][Old]]\n** TODO Later\n[[varde:ccc333]]\n"
  "A heading linking two open tasks and a done one, with a child linking another.")

(defconst org-varde-test--shipping-rows
  '(("aaa111" "open" "open") ("bbb222" "waiting" "open") ("ccc333" "open" "open") ("ddd444" "done" "done")))

(ert-deftest org-varde-done-on-a-heading-offers-to-close-its-tasks ()
  (org-varde-test-with-varde
    (org-varde-test--marking-done org-varde-test--shipping-rows org-varde-test--shipping ?y
      (should (equal asked "Close \"Task aaa111\" (and 1 more) as done? "))
      ;; Its own open tasks: not the done one, nor its child's.
      (should (equal (seq-filter (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls))
                     '("done T00000000000000000000AAA111 --json" "done T00000000000000000000BBB222 --json")))
      ;; Nothing is written back to org.
      (should (equal (buffer-string) (replace-regexp-in-string "TODO Ship" "DONE Ship" org-varde-test--shipping))))))

(ert-deftest org-varde-done-on-a-heading-can-leave-its-tasks-open ()
  (org-varde-test-with-varde
    (org-varde-test--marking-done org-varde-test--shipping-rows org-varde-test--shipping ?n
      (should asked)
      (should-not (seq-find (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls)))))
  (org-varde-test-with-varde
    (let ((org-varde-close-with-heading 'never))
      (org-varde-test--marking-done org-varde-test--shipping-rows org-varde-test--shipping ?y
        (should-not asked)
        (should-not (seq-find (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls))))))
  (org-varde-test-with-varde
    (let ((org-varde-close-with-heading 'always))
      (org-varde-test--marking-done org-varde-test--shipping-rows org-varde-test--shipping ?n
        (should-not asked)
        (should (= 2 (seq-count (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls)))))))
  ;; A repeating heading comes round again: nothing is asked.
  (org-varde-test-with-varde
    (org-varde-test--marking-done org-varde-test--shipping-rows
        "* TODO Weekly review\nSCHEDULED: <2026-10-12 Mon +1w>\n[[varde:aaa111]]\n" ?y
      (should-not asked)
      (should-not (seq-find (lambda (c) (string-prefix-p "done" c)) (org-varde-test-calls))))))

(ert-deftest org-varde-a-review-block-lists-tasks-with-their-status ()
  (org-varde-test-with-varde
    (let ((now (truncate (* 1000 (float-time)))))
      (org-varde-test-reply
       "list"
       (format (concat "{\"v\":2,\"groups\":["
                       "{\"key\":\"open\",\"rows\":[{\"task\":\"T1\",\"short\":\"aaa111\",\"title\":\"Fix | login\","
                       "\"project\":\"/src/app\",\"status\":\"working\",\"state\":\"open\",\"cairn_status\":\"Cause found.\"}]},"
                       "{\"key\":\"done\",\"rows\":[{\"task\":\"T2\",\"short\":\"bbb222\",\"title\":\"Ship docs\","
                       "\"project\":\"/src/web\",\"status\":\"done\",\"state\":\"done\",\"closed\":%d,"
                       "\"cairn_status\":\"%s\"},"
                       "{\"task\":\"T3\",\"short\":\"ccc333\",\"title\":\"Old\",\"project\":\"/src/app\","
                       "\"status\":\"done\",\"state\":\"done\",\"closed\":1000}]}]}")
               now "Shipped the docs for the new API and the SDK, then fixed every broken link"))
      (org-varde-test-with-org "#+BEGIN: varde :closed 7d\n#+END:\n"
        (org-update-dblock)
        (should (equal (org-varde-test-calls) '("list --json --all")))
        (let ((rows (split-string (buffer-string) "\n" t)))
          (should (string-match-p "^| *| Task *| Project *| Status *| *Closed |$" (nth 1 rows)))
          (should (string-match-p "^| ● *| \\[\\[varde:aaa111\\]\\[Fix \\\\vert{} login\\]\\] *| app *| Cause found\\. *| *|$"
                                  (nth 3 rows)))
          ;; A long Status is cut.
          (should (string-match-p (concat "^| ✓ *| \\[\\[varde:bbb222\\]\\[Ship docs\\]\\] *| web *| Shipped the docs for the new API and the SDK, the… *| "
                                          (format-time-string "%F") " |$")
                                  (nth 4 rows)))
          ;; Closed more than 7 days ago: left out.
          (should-not (string-match-p "ccc333" (buffer-string)))
          (should (equal (car (last rows)) "#+END:"))))
      ;; Kept to a project and a state.
      (org-varde-test-with-org "#+BEGIN: varde :project /src/app :state done\n#+END:\n"
        (org-update-dblock)
        (should (string-match-p "ccc333" (buffer-string)))
        (should-not (string-match-p "aaa111\\|bbb222" (buffer-string)))))))

(ert-deftest org-varde-headings-of-an-earlier-org-varde-become-links ()
  (org-varde-test-with-varde
    (org-varde-test-fail-once "unlink" "varde: no task matches")
    (org-varde-test-with-org (concat "* TODO Fix the login bug :work:\n:PROPERTIES:\n:VARDE_TASK: " org-varde-test--id
                                     "\n:CUSTOM: x\n:END:\nIt fails on CI only.\n"
                                     "* Plain\n** DONE Ship it\nCLOSED: [2026-10-01 Wed 10:00]\n"
                                     ":PROPERTIES:\n:VARDE_TASK: 01K5ZZZZZZZZZZZZZZZZABC456\n:END:\n")
      ;; Until converted, a heading's VARDE_TASK still names its task.
      (re-search-forward "^It fails")
      (should (equal (org-varde--task-here) org-varde-test--id))
      (let (said)
        (cl-letf (((symbol-function 'message) (lambda (f &rest a) (setq said (apply #'format f a)))))
          (org-varde-convert-links))
        (should (equal said "Converted 2 headings in 1 file; nothing is saved: C-x s saves them")))
      (should (equal (buffer-string)
                     (concat "* TODO Fix the login bug :work:\n:PROPERTIES:\n:CUSTOM: x\n:END:\n"
                             "[[varde:xyz123][Fix the login bug]]\nIt fails on CI only.\n"
                             "* Plain\n** DONE Ship it\nCLOSED: [2026-10-01 Wed 10:00]\n"
                             "[[varde:abc456][Ship it]]\n")))
      ;; varde forgets the links; one it does not know is converted all the same.
      (should (equal (seq-filter (lambda (c) (string-prefix-p "unlink" c)) (org-varde-test-calls))
                     (list "unlink 01K5ZZZZZZZZZZZZZZZZABC456" (concat "unlink " org-varde-test--id))))
      (should (buffer-modified-p))
      (should-not (string-match-p "varde:" (with-temp-buffer (insert-file-contents file) (buffer-string))))
      ;; A link at point names its task too.
      (goto-char (point-min))
      (search-forward "[[varde:xy")
      (should (equal (org-varde--task-here) "xyz123")))))

;; Everywhere: a file it opened and did not change is closed again.
(ert-deftest org-varde-converting-everywhere-closes-the-files-it-left-alone ()
  (org-varde-test-with-varde
    (let ((a (expand-file-name "a.org" org-directory))
          (b (expand-file-name "b.org" org-directory)))
      (with-temp-file a (insert "* Fix\n:PROPERTIES:\n:VARDE_TASK: " org-varde-test--id "\n:END:\n"))
      (with-temp-file b (insert "* Plain\n"))
      (unwind-protect
          (progn
            (org-varde-convert-links t)
            (should (buffer-modified-p (find-buffer-visiting a)))
            (should-not (find-buffer-visiting b)))
        (when-let* ((buffer (find-buffer-visiting a)))
          (with-current-buffer buffer (set-buffer-modified-p nil))
          (kill-buffer buffer))))))

;;; --- Round 6 review -------------------------------------------------------

(ert-deftest org-varde-links-and-the-board-mark-a-task-alike ()
  (org-varde-test-with-varde
    (dolist (status '("working" "blocked" "waiting" "done" "parked" "dropped" "open"))
      (dolist (unread '(nil t))
        (let ((row (list :status status :unread unread :state "open" :title "T" :short "aaa111" :last 0)))
          (with-temp-buffer
            (org-varde-board--row row 0 "open")
            ;; The link's mark starts with the board's glyph, in its face.
            (let ((mark (org-varde--status-mark row t)))
              (should (equal (buffer-substring-no-properties 2 3) (substring mark 0 1)))
              (should (eq (get-text-property 2 'face) (get-text-property 0 'face mark))))))))
    ;; A session asking for input is the board's red dot; your turn is
    ;; full until you have looked.
    (should (equal (org-varde--status-mark '(:status "blocked") t) "● needs input"))
    (should (eq (get-text-property 0 'face (org-varde--status-mark '(:status "blocked"))) 'org-varde-board-blocked))
    (should (equal (org-varde--status-mark '(:status "waiting" :unread t) t) "● your turn"))
    (should (equal (org-varde--status-mark '(:status "waiting") t) "○ your turn"))))

(ert-deftest org-varde-links-are-matched-anywhere-and-by-short-id ()
  (org-varde-test-with-varde
    (setq org-varde-board-data (org-varde-test--board '("aaa111" "waiting" "open") '("ddd444" "done" "done"))
          org-varde--all (make-hash-table :test #'equal))
    (let (table meta)
      (cl-letf (((symbol-function 'completing-read)
                 (lambda (_prompt tbl &rest _)
                   (setq table tbl meta (funcall tbl "" nil 'metadata))
                   (car (all-completions "" tbl)))))
        (with-temp-buffer (org-varde-insert-link)))
      (let ((match (lambda (string)
                     (let ((all (completion-all-completions string table nil (length string) meta)))
                       (when (consp all) (setcdr (last all) nil))
                       (mapcar #'substring-no-properties all)))))
        ;; Inside a title, and by a task's short id.
        (should (equal (funcall match "ask ddd") '("Task ddd444  ddd444")))
        (should (equal (funcall match "a111") '("Task aaa111  aaa111")))))))

(ert-deftest org-varde-links-export-as-their-description-or-short-id ()
  (org-varde-test-with-varde
    (require 'ox-html)
    (require 'ox-ascii)
    (dolist (backend '(html ascii latex))
      (let ((out (org-export-string-as "[[varde:aaa111][Fix login]] and [[varde:T00000000000000000000BBB222]]\n"
                                       backend t)))
        (should (string-match-p "Fix login" out))
        (should (string-match-p "bbb222" out))
        (should-not (string-match-p "varde:\\|href" out))))))

(ert-deftest org-varde-y-copies-once-says-how-to-paste-and-c-c-l-stores-the-project ()
  (org-varde-test-with-varde
    (org-varde-test--viewing
      (let ((org-stored-links nil)
            said)
        (cl-letf (((symbol-function 'message) (lambda (f &rest a) (setq said (apply #'format f a)))))
          (org-varde-copy-link)
          (org-varde-copy-link))
        (should (equal said (format "Copied a link to \"Fix the login bug\"; %s puts it in"
                                    (substitute-command-keys "\\[yank]"))))
        ;; Under evil, p puts it in.
        (cl-letf (((symbol-function 'message) (lambda (f &rest a) (setq said (apply #'format f a))))
                  ((symbol-function 'org-varde--evil-p) (lambda () t)))
          (org-varde-copy-link))
        (should (equal said "Copied a link to \"Fix the login bug\"; p puts it in"))
        ;; Once in C-c C-l's list, however often copied.
        (should (equal org-stored-links '(("varde:xyz123" "Fix the login bug")))))
      ;; On the Project tab, C-c l stores the project's cairn, as y copies it.
      (org-varde-test-reply "show" "{\"v\":1,\"project\":\"/p/app\",\"cairn\":{}}")
      (org-varde-show-next-tab)
      (let ((org-store-link-plist nil))
        (should (org-varde--store-link))
        (should (equal (plist-get org-store-link-plist :link) "varde:project:/p/app"))))))

(ert-deftest org-varde-done-on-a-heading-fetches-a-task-it-does-not-know ()
  (org-varde-test-with-varde
    ;; The board does not show aaa111: every task's rows are fetched, once.
    (org-varde-test-reply "list" "{\"v\":2,\"groups\":[{\"key\":\"open\",\"rows\":[{\"task\":\"T00000000000000000000AAA111\",\"short\":\"aaa111\",\"title\":\"Old one\",\"state\":\"open\"}]}]}")
    (org-varde-test--marking-done '(("zzz999" "open" "open")) "* TODO Ship\n[[varde:aaa111]]\n" ?y
      (should (equal asked "Close \"Old one\" as done? "))
      (should (equal (seq-filter (lambda (c) (string-prefix-p "list" c)) (org-varde-test-calls))
                     '("list --json --all")))
      (should (member "done T00000000000000000000AAA111 --json" (org-varde-test-calls))))))

(ert-deftest org-varde-a-review-block-checks-what-it-is-asked ()
  (org-varde-test-with-varde
    (let* ((now (truncate (* 1000 (float-time))))
           (real (expand-file-name "real" org-varde-test-dir))
           (alias (expand-file-name "alias" org-varde-test-dir)))
      (make-directory real)
      (make-symbolic-link real alias)
      (org-varde-test-reply
       "list"
       (format (concat "{\"v\":2,\"groups\":[{\"key\":\"done\",\"rows\":["
                       "{\"task\":\"T1\",\"short\":\"aaa111\",\"title\":\"Hour ago\",\"project\":%S,\"status\":\"done\",\"state\":\"done\",\"closed\":%d},"
                       "{\"task\":\"T2\",\"short\":\"bbb222\",\"title\":\"Two days\",\"project\":%S,\"status\":\"done\",\"state\":\"done\",\"closed\":%d}]}]}")
               (file-truename real) (- now 3600000) (file-truename real) (- now (* 2 86400000))))
      (dolist (case '(("24h" "aaa111" "bbb222") ("3" "bbb222" nil) ("1w" "bbb222" nil)))
        (org-varde-test-with-org (format "#+BEGIN: varde :closed %s\n#+END:\n" (car case))
          (org-update-dblock)
          (should (string-match-p (nth 1 case) (buffer-string)))
          (when (nth 2 case) (should-not (string-match-p (nth 2 case) (buffer-string))))))
      ;; A project named through a symlink is the same.
      (org-varde-test-with-org (format "#+BEGIN: varde :project %s\n#+END:\n" alias)
        (org-update-dblock)
        (should (string-match-p "bbb222" (buffer-string))))
      ;; A project with no task says so, in one line.
      (org-varde-test-with-org "#+BEGIN: varde :project /nonexistent/x\n#+END:\n"
        (org-update-dblock)
        (should (equal (buffer-string) "#+BEGIN: varde :project /nonexistent/x\nNo tasks in /nonexistent/x\n#+END:\n")))
      ;; What it cannot read, it names the choices of.
      (org-varde-test-with-org "#+BEGIN: varde :closed 7x\n#+END:\n"
        (should (string-match-p "24h, 7d or 2w" (cadr (should-error (org-update-dblock) :type 'user-error)))))
      (org-varde-test-with-org "#+BEGIN: varde :state closed\n#+END:\n"
        (should (string-match-p "open, done or all" (cadr (should-error (org-update-dblock) :type 'user-error))))))))

(provide 'org-varde-test)
;;; org-varde-test.el ends here
