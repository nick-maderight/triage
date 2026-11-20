;;; triage-render-test.el --- ERT tests for the triage card layout -*- lexical-binding: t; -*-

;;; Commentary:

;; Tests for `triage-render.el'.  They draw a card into a buffer that no
;; window shows, which makes the text width `fill-column'.

;;; Code:

(require 'cl-lib)
(require 'ert)
(require 'triage-render)

(ert-deftest triage-render-test-lines-break-between-items ()
  "Headings, the facts line and the dock stay inside the text width."
  (with-temp-buffer
    (magit-section-mode)
    (setq fill-column 40)
    (let ((inhibit-read-only t))
      (triage-render-card
       '((id . "c1") (kind . "finding") (title . "Short title") (priority . 2)
         (source . ((uri . "tmux://work") (pane . "%1") (target . "work:1.0")
                    (cwd . "/srv/projects/long-name")))
         (star . ((situation . "The key is old.")))
         (options . (((label . "Go") (recommended . t)) ((label . "Stop")))))
       (triage-draft--make)
       (lambda (_id) #'ignore)))
    (let ((lines (split-string (buffer-string) "\n")))
      (dolist (line lines)
        (should (<= (string-width line) 40)))
      (dolist (item '("Kind finding" "Directory /srv/projects/long-name"
                      "C Acknowledge & close" "v Peek" "J Jump" "? more"))
        (should (cl-some (lambda (line) (string-search item line)) lines)))
      (dolist (heading '("Situation ─" "Action ─"))
        (should (cl-some (lambda (line) (string-prefix-p heading line)) lines))))))

(ert-deftest triage-render-test-side-margins ()
  "Margins take a fifth of each side until the text would drop under 60."
  (let ((triage-side-margin 0.2))
    (should (= (triage-render--margin 270) 54))
    (should (= (triage-render--margin 100) 20))
    (should (= (triage-render--margin 80) 10))
    (should (= (triage-render--margin 40) 2))))

(ert-deftest triage-render-test-markdown-heading-marks-hidden ()
  "A Markdown heading shows its title without the # marks."
  (skip-unless (require 'markdown-mode nil t))
  (let ((text (triage-render-markdown "## Summary\n\nAll hosts finished.")))
    (should (get-text-property 0 'invisible text))
    (should-not (get-text-property (string-search "Summary" text) 'invisible text))
    (should-not (get-text-property (string-search "All" text) 'invisible text))))

(ert-deftest triage-render-test-info-body-drops-repeated-title ()
  "An info body that opens with the card title as a heading starts below it."
  (should (equal (triage-render--untitled "# Backup report\n\n## Summary\n" "Backup report")
                 "## Summary\n"))
  (should (equal (triage-render--untitled "# Other\nText" "Backup report")
                 "# Other\nText")))

(ert-deftest triage-render-test-markdown-reuses-fontification ()
  "Equal Markdown text is fontified once and different text stays distinct."
  (skip-unless (require 'markdown-mode nil t))
  (when (boundp 'triage-render--markdown-cache)
    (clrhash (symbol-value 'triage-render--markdown-cache)))
  (let ((calls 0)
        (original (symbol-function 'triage-render--gfm)))
    (cl-letf (((symbol-function 'triage-render--gfm)
               (lambda (text)
                 (setq calls (1+ calls))
                 (funcall original text))))
      (let ((first (triage-render-markdown "## Summary\n\nAll hosts finished."))
            (second (triage-render-markdown "## Summary\n\nAll hosts finished.")))
        (should (equal-including-properties first second))
        (should (= calls 1)))
      (triage-render-markdown "## Different\n\nText.")
      (should (= calls 2)))))

(provide 'triage-render-test)
;;; triage-render-test.el ends here
