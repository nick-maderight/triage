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

(provide 'triage-launch-test)
;;; triage-launch-test.el ends here
