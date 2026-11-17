;;; triage-render-test.el --- ERT tests for the triage card layout -*- lexical-binding: t; -*-

;;; Commentary:

;; Tests for `triage-render.el'.  They draw a card into a buffer that no
;; window shows, which makes the text width `fill-column'.

;;; Code:

(require 'cl-lib)
(require 'ert)
(require 'triage-render)

(ert-deftest triage-render-test-lines-break-between-items ()
  "The facts line and the dock break between whole items at the text width."
  (with-temp-buffer
    (magit-section-mode)
    (setq fill-column 40)
    (let ((inhibit-read-only t))
      (triage-render-card
       '((id . "c1") (kind . "finding") (title . "Short title") (priority . 2)
         (source . ((uri . "tmux://work") (pane . "%1") (target . "work:1.0")
                    (cwd . "/srv/projects/long-name")))
         (options . (((label . "Go") (recommended . t)) ((label . "Stop")))))
       (triage-draft--make)
       (lambda (_id) #'ignore)))
    (let ((lines (split-string (buffer-string) "\n")))
      (dolist (line lines)
        (should (<= (string-width line) 40)))
      (dolist (item '("Kind finding" "Directory /srv/projects/long-name"
                      "x Acknowledge & close" "v Peek" "J Jump" "? more"))
        (should (cl-some (lambda (line) (string-search item line)) lines))))))

(provide 'triage-render-test)
;;; triage-render-test.el ends here
