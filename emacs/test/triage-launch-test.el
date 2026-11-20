;;; triage-launch-test.el --- ERT tests for launch options -*- lexical-binding: t; -*-

;;; Commentary:
;; Tests for choosing launch settings without leaving the keyboard.

;;; Code:
(require 'ert)
(require 'triage-launch)

(ert-deftest triage-launch-test-engine-takes-one-key ()
  "The fixed Engine choices accept one digit instead of a completion prompt."
  (with-temp-buffer
    (triage-launch-mode)
    (setq triage-launch--active (current-buffer))
    (let (used-multiple-choice)
      (cl-letf (((symbol-function 'triage-launch--draw) #'ignore)
                ((symbol-function 'read-multiple-choice)
                 (lambda (_prompt choices)
                   (setq used-multiple-choice t)
                   (should (equal (mapcar #'car choices) '(?1 ?2 ?3)))
                   (nth 1 choices)))
                ((symbol-function 'completing-read)
                 (lambda (&rest _args) "omp")))
        (triage-launch-set-engine))
      (should used-multiple-choice)
      (should (equal triage-launch--engine "omp")))))

(provide 'triage-launch-test)
;;; triage-launch-test.el ends here
