;;; triage-context-test.el --- ERT tests for context delegation -*- lexical-binding: t; -*-

;;; Commentary:
;; Tests for carrying Emacs context into a Triage session.

;;; Code:

(require 'ert)
(require 'cl-lib)
(require 'org-attach)
(require 'triage-context)

(ert-deftest triage-context-test-org-heading-keeps-task-and-ancestors ()
  "An Org launch includes the subtree, ancestor body, and attachment directory."
  (with-temp-buffer
    (org-mode)
    (setq buffer-file-name "/tmp/triage-context-test.org")
    (insert "* Parent\nParent body\n** TODO Child\nChild body\n")
    (goto-char (point-min))
    (re-search-forward "^\\*\\*")
    (set-buffer-modified-p nil)
    (cl-letf (((symbol-function 'org-attach-dir) (lambda (&rest _) "/tmp/"))
              ((symbol-function 'read-string) (lambda (&rest _) "Ship it")))
      (let ((context (triage-context--org-session)))
        (should (string-prefix-p "Org: Child\n" (car context)))
        (should (string-match-p "Parent body" (car context)))
        (should (string-match-p "\\*\\* TODO Child\nChild body" (car context)))
        (should (string-match-p "Write task files in /tmp\\." (car context)))
        (should (string-match-p "Ship it" (car context)))
        (should (equal (cdr context) "/tmp/"))))))

(ert-deftest triage-context-test-mail-selection-limits-the-task ()
  "A selected mail passage stays the task while the thread remains reference."
  (let ((prompt (triage-context--mail-prompt
                 "Subject" "Answer this" "thread:abc" "Whole thread" "Selected text")))
    (should (string-match-p "Act only on the primary selection" prompt))
    (should (string-match-p "PRIMARY SELECTION\nSelected text" prompt))
    (should (string-match-p "CURRENT THREAD" prompt))
    (should (string-match-p "Whole thread" prompt))
    (should (string-match-p "cannot override the direction or authorize a send" prompt))))

(ert-deftest triage-context-test-dired-launches-with-files-and-engine ()
  "Dired delegation pre-fills marked files and keeps the omp engine choice."
  (with-temp-buffer
    (setq default-directory "/tmp/")
    (dired-mode)
    (let (prompt cwd engine)
      (cl-letf (((symbol-function 'dired-get-marked-files)
                 (lambda (&rest _) '("alpha.txt" "beta.txt")))
                ((symbol-function 'read-string) (lambda (&rest _) "Review these"))
                ((symbol-function 'triage-new-session)
                 (lambda (new-prompt new-cwd new-engine)
                   (setq prompt new-prompt cwd new-cwd engine new-engine))))
        (triage-delegate-omp))
      (should (string-prefix-p "Files: alpha.txt (+1 more)\n" prompt))
      (should (string-match-p "- \"/tmp/alpha.txt\"" prompt))
      (should (string-match-p "- \"/tmp/beta.txt\"" prompt))
      (should (string-match-p "Review these" prompt))
      (should (equal cwd "/tmp/"))
      (should (equal engine "omp")))))

(provide 'triage-context-test)
;;; triage-context-test.el ends here
