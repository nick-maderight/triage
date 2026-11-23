;;; triage-inbox-test.el --- ERT tests for deck navigation -*- lexical-binding: t; -*-

;;; Commentary:
;; Tests for the Emacs inbox's position after local card actions.

;;; Code:
(require 'ert)
(require 'triage-inbox)

(ert-deftest triage-inbox-test-own-action-keeps-the-next-card-in-place ()
  "Removing the shown card keeps the next card at the same deck position."
  (with-temp-buffer
    (triage-inbox-mode)
    (let* ((first '((id . "c1") (kind . "info") (title . "First")))
           (second '((id . "c2") (kind . "info") (title . "Second")))
           (third '((id . "c3") (kind . "info") (title . "Third")))
           (triage-queue-cards (list first second third))
           (triage-queue--inflight (make-hash-table :test #'equal)))
      (setq triage-inbox--idx 1
            triage-inbox--shown-id "c2"
            triage-inbox--shown-card second
            triage-inbox--top first)
      (puthash "c2" t triage-queue--inflight)
      (setq triage-queue-cards (list first third))
      (cl-letf (((symbol-function 'triage-inbox--render) #'ignore))
        (triage-inbox--apply))
      (should (= triage-inbox--idx 1)))))

(ert-deftest triage-inbox-test-later-defers-an-hour-without-asking ()
  "Later defers the shown card for 60 minutes and asks nothing."
  (with-temp-buffer
    (triage-inbox-mode)
    (let ((card '((id . "c1") (kind . "info") (title . "First")))
          sent)
      (setq triage-inbox--shown-card card)
      (cl-letf (((symbol-function 'read-multiple-choice)
                 (lambda (&rest _) (error "Later asked for a choice")))
                ((symbol-function 'triage-queue-act)
                 (lambda (shown verb body)
                   (setq sent (list (alist-get 'id shown) verb body))
                   t)))
        (triage-inbox-later))
      (should (equal sent '("c1" "later" ((minutes . 60))))))))

(provide 'triage-inbox-test)
;;; triage-inbox-test.el ends here
