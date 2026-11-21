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

(ert-deftest triage-context-test-org-narrowing-keeps-context ()
  "Narrowing does not drop ancestor context or change the selected heading."
  (with-temp-buffer
    (org-mode)
    (setq buffer-file-name "/tmp/triage-context-narrow.org")
    (insert "* Parent\nParent body\n** Child\nChild body\n** Sibling\nSibling body\n")
    (goto-char (point-min))
    (re-search-forward "^\\*\\* Child")
    (let ((start (line-beginning-position))
          (end (save-excursion (org-end-of-subtree t t) (point))))
      (narrow-to-region start end)
      (goto-char start)
      (set-buffer-modified-p nil)
      (cl-letf (((symbol-function 'org-attach-dir) (lambda (&rest _) "/tmp/"))
                ((symbol-function 'read-string)
                 (lambda (&rest _)
                   (should (= (point-min) start))
                   (should (= (point-max) end))
                   "")))
        (let ((context (triage-context--org-session)))
          (should (string-prefix-p "Org: Child\n" (car context)))
          (should (string-match-p "Parent body" (car context)))
          (should-not (string-match-p "Sibling body" (car context)))
          (should (= (point-min) start))
          (should (= (point-max) end)))))))

(ert-deftest triage-context-test-agenda-widens-before-using-marker ()
  "An agenda marker outside the narrowing still selects its own heading."
  (with-temp-buffer
    (org-mode)
    (setq buffer-file-name "/tmp/triage-context-agenda.org")
    (insert "* Parent\nParent body\n** Child\nChild body\n** Sibling\nSibling body\n")
    (goto-char (point-min))
    (re-search-forward "^\\*\\* Child")
    (let ((start (line-beginning-position))
          (marker (copy-marker (+ (line-beginning-position) 2))))
      (narrow-to-region (point-min) start)
      (set-buffer-modified-p nil)
      (cl-letf (((symbol-function 'org-get-at-bol) (lambda (&rest _) marker))
                ((symbol-function 'org-attach-dir) (lambda (&rest _) "/tmp/"))
                ((symbol-function 'read-string)
                 (lambda (&rest _)
                   (should (= (point-max) start))
                   "")))
        (let ((context (triage-context--agenda-session)))
          (should (string-prefix-p "Org: Child\n" (car context)))
          (should (string-match-p "Parent body" (car context)))
          (should (= (point-max) start)))))))

(ert-deftest triage-context-test-region-uses-saved-text-and-lines ()
  "Region prompts use saved text and exclude the next line at column zero."
  (let ((file (make-temp-name "/tmp/triage-context-region-"))
        (transient-mark-mode t))
    (unwind-protect
        (with-temp-buffer
          (setq buffer-file-name file)
          (insert "one\ntwo\nthree\nfour\nfive  \nsix\nseven\neight\n")
          (setq-local before-save-hook '(delete-trailing-whitespace))
          (goto-char (point-min))
          (forward-line 4)
          (set-mark (point))
          (forward-line 3)
          (setq mark-active t)
          (cl-letf (((symbol-function 'y-or-n-p) (lambda (&rest _) t))
                    ((symbol-function 'read-string) (lambda (&rest _) "Fix it")))
            (let ((context (triage-context--region-session)))
              (should (string-prefix-p (format "Rewrite: %s:5-7" file) (car context)))
              (should (string-match-p "five\nsix\nseven" (car context)))
              (should-not (string-match-p "five  \n" (car context))))))
      (when (file-exists-p file)
        (delete-file file)))))

(ert-deftest triage-context-test-starting-launch-stops-context-capture ()
  "An in-flight launch stops delegation before context capture."
  (let ((buffer (get-buffer-create triage-launch--compose-buffer)))
    (unwind-protect
        (progn
          (with-current-buffer buffer
            (triage-launch-mode)
            (setq triage-launch--starting t))
          (with-temp-buffer
            (org-mode)
            (let (called)
              (cl-letf (((symbol-function 'triage-context--org-session)
                         (lambda ()
                           (setq called t)
                           '("Prompt" . "/tmp/")))
                        ((symbol-function 'triage-new-session) #'ignore))
                (should-error (triage-delegate) :type 'user-error)
                (should-not called)))))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))))

(provide 'triage-context-test)
;;; triage-context-test.el ends here
