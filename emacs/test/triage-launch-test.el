;;; triage-launch-test.el --- ERT tests for launch options -*- lexical-binding: t; -*-

;;; Commentary:
;; Tests for choosing launch settings without leaving the keyboard.

;;; Code:
(require 'ert)
(require 'triage-launch)

(defun triage-launch-test--sheet (kind mode)
  "Turn the current buffer into a drawn launch sheet of KIND in MODE."
  (triage-launch-mode)
  (setq triage-launch--kind kind
        triage-launch--context (and (eq kind 'context) "Org: Child\n")
        triage-launch--catalogs nil)
  (triage-launch--set-mode mode)
  (triage-launch--draw))

;; Options stay live the way Magit's menus do, so one key changes, launches or cancels.
(ert-deftest triage-launch-test-option-rows-take-one-key ()
  "One key on the option rows acts at once, while the body still takes text."
  (with-temp-buffer
    (triage-launch-test--sheet 'dispatch "investigate")
    (triage-launch-edit)
    (insert "typed")
    (dolist (position (list (point-min) (1- triage-launch--body-start)))
      (goto-char position)
      (should (eq (key-binding "e") #'triage-launch-set-engine))
      (should (eq (key-binding "m") #'triage-launch-set-mode))
      (should (eq (key-binding (kbd "RET")) #'triage-launch-submit))
      (should (eq (key-binding "q") #'triage-launch-cancel))
      (should (eq (key-binding (kbd "TAB")) #'triage-launch-edit))
      (should (eq (key-binding "f") #'undefined)))
    (dolist (position (list triage-launch--body-start (point-max)))
      (goto-char position)
      (should (eq (key-binding "e") #'self-insert-command))
      (should (eq (key-binding "q") #'self-insert-command))
      (should (eq (key-binding (kbd "C-c C-c")) #'triage-launch-submit))
      (should (eq (key-binding (kbd "<backtab>")) #'triage-launch-options)))
    (goto-char (+ triage-launch--body-start 2))
    (triage-launch-options)
    (should (= (point) (point-min)))
    (triage-launch-edit)
    (should (= (point) (+ triage-launch--body-start 2)))))

(ert-deftest triage-launch-test-a-redraw-keeps-point-in-its-zone ()
  "A redraw, such as a model catalog arriving, leaves point in the text or on its row."
  (with-temp-buffer
    (triage-launch-test--sheet 'compose "goal")
    (triage-launch-edit)
    (triage-launch--draw)
    (should (= (point) triage-launch--body-start))
    (should (eq (key-binding "e") #'self-insert-command))
    (insert "abc")
    (goto-char triage-launch--body-start)
    (triage-launch--draw)
    (should (= (point) triage-launch--body-start))
    (goto-char (point-min))
    (forward-line 5)
    (let ((row (point)))
      (triage-launch--draw)
      (should (= (point) row)))))

(ert-deftest triage-launch-test-a-mode-change-keeps-point-on-the-options ()
  "Filling a mode's default instruction leaves point where the next key acts."
  (with-temp-buffer
    (triage-launch-test--sheet 'context "investigate")
    (goto-char (point-min))
    (cl-letf (((symbol-function 'read-multiple-choice)
               (lambda (_prompt choices)
                 (seq-find (lambda (choice) (equal (nth 1 choice) "Act")) choices))))
      (triage-launch-set-mode))
    (should (equal triage-launch--mode "act"))
    (should (equal (triage-launch--body) triage-card-act-default))
    (should (< (point) triage-launch--body-start))
    (should (string-match-p "Dispatch · Org: Child" (buffer-string)))
    (should (string-match-p "^d  Directory" (buffer-string)))
    (should-not (string-match-p "^x  Exec model" (buffer-string)))
    (should-error (triage-launch-set-executor) :type 'user-error)))

(ert-deftest triage-launch-test-engine-takes-one-key ()
  "The fixed Engine choices accept one mnemonic key instead of a completion prompt."
  (with-temp-buffer
    (triage-launch-mode)
    (let (used-multiple-choice)
      (cl-letf (((symbol-function 'triage-launch--draw) #'ignore)
                ((symbol-function 'read-multiple-choice)
                 (lambda (_prompt choices)
                   (setq used-multiple-choice t)
                   (should (equal (mapcar #'car choices) '(?d ?o ?D)))
                   (nth 1 choices)))
                ((symbol-function 'completing-read)
                 (lambda (&rest _args) "omp")))
        (triage-launch-set-engine))
      (should used-multiple-choice)
      (should (equal triage-launch--engine "omp")))))

(ert-deftest triage-launch-test-draw-does-not-load-a-catalog ()
  "Drawing launch options never waits for a model catalog."
  (with-temp-buffer
    (triage-launch-mode)
    (setq triage-launch--kind 'compose
          triage-launch--mode "goal"
          triage-launch--catalogs nil)
    (let (sync-called)
      (cl-letf (((symbol-function 'triage-api-request-sync)
                 (lambda (&rest _args)
                   (setq sync-called t)
                   nil)))
        (triage-launch--draw))
      (should-not sync-called))))

(ert-deftest triage-launch-test-model-catalog-loads-before-the-prompt ()
  "The model prompt waits for an asynchronous catalog without blocking Emacs."
  (with-temp-buffer
    (triage-launch-mode)
    (setq triage-launch--kind 'compose
          triage-launch--mode "goal"
          triage-launch--engine "omp"
          triage-launch--catalogs nil)
    (let (sync-called async-then prompt-called)
      (cl-letf (((symbol-function 'triage-api-request-sync)
                 (lambda (&rest _args)
                   (setq sync-called t)
                   '((engine . "omp")
                     (default . "provider/default")
                     (models . (((selector . "provider/default")
                                 (name . "Default model")))))))
                ((symbol-function 'triage-api-request)
                 (lambda (&rest args)
                   (setq async-then (plist-get (cddr args) :then))))
                ((symbol-function 'completing-read)
                 (lambda (&rest _args)
                   (setq prompt-called t)
                   "Default (Default model)"))
                ((symbol-function 'triage-launch--draw) #'ignore))
        (triage-launch-set-model)
        (should-not sync-called)
        (should async-then)
        (should-not prompt-called)
        (funcall async-then
                 '((engine . "omp")
                   (default . "provider/default")
                   (models . (((selector . "provider/default")
                               (name . "Default model"))))))
        (should prompt-called)))))

(ert-deftest triage-launch-test-context-opens-a-dispatch-sheet ()
  "A context launch opens a Dispatch sheet on its options with the fix default."
  (let ((buffer (get-buffer-create triage-launch--context-buffer))
        catalog-engine)
    (with-current-buffer buffer
      (triage-launch-mode)
      (setq triage-launch--executor "old-model"
            triage-launch--executor-thinking "high"))
    (unwind-protect
        (cl-letf (((symbol-function 'triage-launch--load-tmux-sessions) #'ignore)
                  ((symbol-function 'triage-launch--with-catalog)
                   (lambda (engine _fn) (setq catalog-engine engine)))
                  ((symbol-function 'pop-to-buffer) #'ignore))
          (triage-launch-context "Org: Child\n\nChild body\n" "/tmp/" "dsh")
          (with-current-buffer buffer
            (should (eq triage-launch--kind 'context))
            (should (equal triage-launch--mode "investigate-fix"))
            (should (equal (triage-launch--body) triage-card-fix-default))
            (should (equal triage-launch--cwd "/tmp/"))
            (should (equal triage-launch--engine "dsh"))
            (should-not triage-launch--executor)
            (should-not triage-launch--executor-thinking)
            (should (= (point) (point-min))))
          (should (equal catalog-engine "dsh")))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))))

(ert-deftest triage-launch-test-context-keeps-an-unsent-direction ()
  "Another context replaces a typed direction only after confirmation."
  (let ((buffer (get-buffer-create triage-launch--context-buffer)))
    (unwind-protect
        (cl-letf (((symbol-function 'triage-launch--load-tmux-sessions) #'ignore)
                  ((symbol-function 'triage-launch--with-catalog) #'ignore)
                  ((symbol-function 'pop-to-buffer) #'ignore))
          (triage-launch-context "Org: First\n" "/tmp/")
          (with-current-buffer buffer
            (triage-launch--set-body "Unsent direction"))
          (triage-launch-context "Org: First\n" "/tmp/")
          (with-current-buffer buffer
            (should (equal (triage-launch--body) "Unsent direction")))
          (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) nil)))
            (should-error (triage-launch-context "Org: Second\n" "/tmp/")
                          :type 'user-error))
          (with-current-buffer buffer
            (should (equal triage-launch--context "Org: First\n"))
            (should (equal (triage-launch--body) "Unsent direction")))
          (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t)))
            (triage-launch-context "Org: Second\n" "/tmp/"))
          (with-current-buffer buffer
            (should (equal triage-launch--context "Org: Second\n"))
            (should (equal (triage-launch--body) triage-card-fix-default))))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))))

(ert-deftest triage-launch-test-context-submit-sends-note-and-dispatch-mode ()
  "A context Dispatch posts the context as the prompt and the body as its note."
  (with-temp-buffer
    (triage-launch-test--sheet 'context "act")
    (triage-launch--set-body "  ")
    (let (body)
      (cl-letf (((symbol-function 'triage-api-request)
                 (lambda (_method _path &rest args)
                   (setq body (plist-get args :body))
                   'process)))
        (triage-launch-submit))
      (should (equal (alist-get 'prompt body) "Org: Child\n"))
      (should (equal (alist-get 'mode body) "act"))
      (should (equal (alist-get 'note body) "")))))

(ert-deftest triage-launch-test-cancel-keeps-a-new-session-prompt ()
  "Cancel on a New session hides the sheet and keeps its prompt."
  (let ((buffer (get-buffer-create triage-launch--compose-buffer)))
    (unwind-protect
        (progn
          (with-current-buffer buffer
            (triage-launch-test--sheet 'compose "goal")
            (triage-launch-edit)
            (insert "Unsent prompt"))
          (switch-to-buffer buffer)
          (triage-launch-cancel)
          (should (buffer-live-p buffer))
          (with-current-buffer buffer
            (should (equal (triage-launch--body) "Unsent prompt"))))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))))

(ert-deftest triage-launch-test-new-session-repairs-invalid-mode ()
  "A remembered non-string mode resets to goal when a sheet opens."
  (let ((buffer (get-buffer-create triage-launch--compose-buffer))
        (triage-launch--last-compose-mode '((mode . "plan"))))
    (unwind-protect
        (cl-letf (((symbol-function 'triage-launch--load-tmux-sessions) #'ignore)
                  ((symbol-function 'triage-launch--with-catalog) #'ignore)
                  ((symbol-function 'pop-to-buffer) #'ignore))
          (triage-new-session)
          (with-current-buffer buffer
            (should (equal triage-launch--mode "goal"))
            (should (equal triage-launch--last-compose-mode "goal"))
            (should (= (point) (point-max)))))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))))

(ert-deftest triage-launch-test-compose-submit-recovers-from-invalid-mode ()
  "Repair a bad sheet mode and keep the sheet ready after a failed launch."
  (with-temp-buffer
    (triage-launch-mode)
    (setq triage-launch--kind 'compose
          triage-launch--mode '((mode . "plan")))
    (insert "Do the task")
    (let ((triage-launch--last-compose-mode "goal")
          args calls failure sync-error quit-called pop-called)
      (cl-letf (((symbol-function 'triage-start-session)
                 (lambda (&rest new-args)
                   (setq calls (1+ (or calls 0)))
                   (if sync-error
                       (error "Bad request")
                     (setq args new-args
                           failure (plist-get (cdr new-args) :else))
                     'process)))
                ((symbol-function 'quit-window)
                 (lambda (&rest _) (setq quit-called t)))
                ((symbol-function 'pop-to-buffer)
                 (lambda (&rest _) (setq pop-called t))))
        (triage-launch--submit-session)
        (should (= calls 1))
        (should (equal (plist-get (cdr args) :mode) "goal"))
        (should-not (plist-get (cdr args) :note))
        (should triage-launch--starting)
        (should-error (triage-launch-cancel) :type 'user-error)
        (should (buffer-live-p (current-buffer)))
        (should-error (triage-launch--submit-session) :type 'user-error)
        (should (= calls 1))
        (funcall failure 422 "bad mode")
        (should-not triage-launch--starting)
        (should (equal (triage-launch--body) "Do the task"))
        (should-not quit-called)
        (should-not pop-called)
        (triage-launch--submit-session)
        (should (= calls 2))
        (funcall failure 500 "retry failed")
        (should-not triage-launch--starting)
        (setq sync-error t)
        (should-error (triage-launch--submit-session))
        (should-not triage-launch--starting)
        (should (= calls 3))))))

(ert-deftest triage-launch-test-start-session-rejects-structured-mode ()
  "The session API rejects a structured mode, or a Dispatch mode without a note."
  (let (called)
    (cl-letf (((symbol-function 'triage-api-request)
               (lambda (&rest _) (setq called t))))
      (should-error
       (triage-start-session "Task" :mode '((mode . "goal")))
       :type 'user-error)
      (should-error (triage-start-session "Task" :mode "act") :type 'user-error)
      (should-not called))))

(provide 'triage-launch-test)
;;; triage-launch-test.el ends here
