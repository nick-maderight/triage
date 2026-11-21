;;; triage-launch-test.el --- ERT tests for launch options -*- lexical-binding: t; -*-

;;; Commentary:
;; Tests for choosing launch settings without leaving the keyboard.

;;; Code:
(require 'ert)
(require 'triage-launch)

(ert-deftest triage-launch-test-engine-takes-one-key ()
  "The fixed Engine choices accept one mnemonic key instead of a completion prompt."
  (with-temp-buffer
    (triage-launch-mode)
    (setq triage-launch--active (current-buffer))
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
    (setq triage-launch--active (current-buffer)
          triage-launch--kind 'compose
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


(ert-deftest triage-launch-test-new-session-prefills-context ()
  "A contextual launch pre-fills the draft and loads the chosen engine's catalog."
  (let ((buffer (get-buffer-create triage-launch--compose-buffer))
        catalog-engine)
    (with-current-buffer buffer
      (triage-launch-mode)
      (setq triage-launch--executor "old-model"
            triage-launch--executor-thinking "high"))
    (unwind-protect
        (cl-letf (((symbol-function 'triage-launch--load-tmux-sessions) #'ignore)
                  ((symbol-function 'triage-launch--with-catalog)
                   (lambda (engine _fn) (setq catalog-engine engine)))
                  ((symbol-function 'pop-to-buffer)
                   (lambda (target) (set-buffer target) target)))
          (triage-new-session "Context prompt" "/tmp/" "dsh")
          (with-current-buffer buffer
            (should (equal (triage-launch--body) "Context prompt"))
            (should (equal triage-launch--cwd "/tmp/"))
            (should (equal triage-launch--engine "dsh"))
            (should-not triage-launch--executor)
            (should-not triage-launch--executor-thinking)
            (should (= (point) (point-max))))
          (should (equal catalog-engine "dsh")))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))))

(ert-deftest triage-launch-test-context-keeps-an-unsent-draft ()
  "A contextual launch replaces a different draft only after confirmation."
  (let ((buffer (get-buffer-create triage-launch--compose-buffer)))
    (unwind-protect
        (progn
          (with-current-buffer buffer
            (triage-launch-mode)
            (insert "Unsent draft"))
          (cl-letf (((symbol-function 'triage-launch--load-tmux-sessions) #'ignore)
                    ((symbol-function 'triage-launch--with-catalog) #'ignore)
                    ((symbol-function 'pop-to-buffer) #'ignore))
            (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) nil)))
              (should-error
               (triage-new-session "Replacement" "/tmp/" "omp")
               :type 'user-error))
            (with-current-buffer buffer
              (should (equal (triage-launch--body) "Unsent draft")))
            (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t)))
              (triage-new-session "Replacement" "/tmp/" "omp"))
            (with-current-buffer buffer
              (should (equal (triage-launch--body) "Replacement")))))
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
          (triage-new-session "Context prompt" "/tmp/" "omp")
          (with-current-buffer buffer
            (should (equal triage-launch--mode "goal"))
            (should (equal triage-launch--last-compose-mode "goal"))))
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
        (triage-launch--submit-compose)
        (should (= calls 1))
        (should (equal (plist-get (cdr args) :mode) "goal"))
        (should triage-launch--starting)
        (should-error (triage-launch-cancel) :type 'user-error)
        (should (buffer-live-p (current-buffer)))
        (should-error (triage-launch--submit-compose) :type 'user-error)
        (should (= calls 1))
        (funcall failure 422 "bad mode")
        (should-not triage-launch--starting)
        (should (equal (triage-launch--body) "Do the task"))
        (should-not quit-called)
        (should-not pop-called)
        (triage-launch--submit-compose)
        (should (= calls 2))
        (funcall failure 500 "retry failed")
        (should-not triage-launch--starting)
        (setq sync-error t)
        (should-error (triage-launch--submit-compose))
        (should-not triage-launch--starting)
        (should (= calls 3))))))

(ert-deftest triage-launch-test-start-session-rejects-structured-mode ()
  "The session API rejects a structured mode before opening a request."
  (let (called)
    (cl-letf (((symbol-function 'triage-api-request)
               (lambda (&rest _) (setq called t))))
      (should-error
       (triage-start-session "Task" :mode '((mode . "goal")))
       :type 'user-error)
      (should-not called))))

(provide 'triage-launch-test)
;;; triage-launch-test.el ends here
