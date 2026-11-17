;;; triage-card-test.el --- Pure ERT tests for the triage card model -*- lexical-binding: t; -*-

;;; Commentary:

;; Tests for `triage-card.el'.  Each fixture is a literal card alist shaped like
;; decoded daemon JSON.  JSON arrays decode to lists and false decodes to nil.
;; Fixtures use lists, and nil when the daemon sends false.

;;; Code:

(require 'ert)
(require 'triage-card)

(defun triage-card-test--status (card)
  "Return the label, tone and title of the status of CARD as a list."
  (let ((status (triage-card-status card)))
    (list (plist-get status :label)
          (plist-get status :tone)
          (plist-get status :title))))

(defun triage-card-test--ids (cards)
  "Return the ids of CARDS in order."
  (mapcar (lambda (card) (alist-get 'id card)) cards))

(defun triage-card-test--local (year month day hour minute)
  "Return the local time YEAR MONTH DAY HOUR MINUTE as a float time."
  (float-time (encode-time (list 0 minute hour day month year nil nil nil))))

(defconst triage-card-test--catalog
  '((engine . "dsh")
    (thinking . ("low" "high"))
    (default . "n")
    (models . (((selector . "m") (name . "M") (efforts))
               ((selector . "n") (name . "N"))
               ((selector . "p") (name . "P") (efforts . ("max")))))))

(ert-deftest triage-card-test-status-prefix-reports ()
  "Report prefixes in info and plan titles set the status."
  (should (equal (triage-card-test--status
                  '((id . "i1") (kind . "info") (title . "Done: Ship it")))
                 '("Complete" positive "Ship it")))
  (should (equal (triage-card-test--status
                  '((id . "i2") (kind . "info") (title . "Agent done: X")
                    (body . "# Real title\nmore")))
                 '("Step left open" warning "Real title")))
  (should (equal (triage-card-test--status
                  '((id . "p1") (kind . "plan") (title . "Plan ready: P")))
                 '("Plan ready" nil "P"))))

(ert-deftest triage-card-test-status-legacy ()
  "Cards without a status fall back to their kind and refs."
  (should (equal (triage-card-test--status
                  '((id . "i3") (kind . "info") (title . "Plain") (refs . nil)))
                 '("Notice" nil "Plain")))
  (should (equal (triage-card-test--status
                  '((id . "i4") (kind . "info") (title . "Plain") (refs . ("x"))))
                 '("No final report" warning "Plain")))
  (should (equal (triage-card-test--status
                  '((id . "a1") (kind . "ask") (title . "Q")))
                 '("Waiting on you" nil "Q"))))

(ert-deftest triage-card-test-status-unknown-stated-value ()
  "An unknown stated status falls back to the legacy status."
  (should (equal (triage-card-test--status
                  '((id . "f1") (kind . "finding") (title . "F") (status . "bogus")))
                 '("Waiting on you" nil "F"))))

(ert-deftest triage-card-test-status-stated-value-wins ()
  "A known stated status beats the prefix and legacy status."
  (should (equal (triage-card-test--status
                  '((id . "i5") (kind . "info") (title . "Plain") (status . "complete")))
                 '("Complete" positive "Plain")))
  (should (equal (triage-card-test--status
                  '((id . "f2") (kind . "finding") (title . "Done: X")
                    (status . "incomplete")))
                 '("Incomplete" negative "Done: X"))))

(ert-deftest triage-card-test-heading ()
  "The heading is the first non-blank line when it is a Markdown heading."
  (should (equal (triage-card-heading "  \n\n  ## Title here  \nbody") "Title here"))
  (should (equal (triage-card-heading "plain first\n# Later") ""))
  (should (equal (triage-card-heading "####### seven") "")))

(ert-deftest triage-card-test-title-of ()
  "An info card shows its body heading, and other cards show their title."
  (should (equal (triage-card-title-of
                  '((id . "i") (kind . "info") (title . "T") (body . "# Head\nx")))
                 "Head"))
  (should (equal (triage-card-title-of
                  '((id . "i") (kind . "info") (title . "T") (body . "no heading")))
                 "T"))
  (should (equal (triage-card-title-of
                  '((id . "p") (kind . "plan") (title . "T") (body . "# Head")))
                 "T")))

(ert-deftest triage-card-test-info-with-step-and-pane ()
  "An info card with a step leads with plan and offers the pane actions."
  (let ((card '((id . "i1") (kind . "info") (title . "Plain")
                (star . ((action . "Do X")))
                (source . ((uri . "u") (label . "L") (pane . "%5")))
                (options . (((label . "Acknowledge") (recommended . t)))))))
    (should (eq (triage-card-lead card 0) 'plan))
    (should (triage-card-offers-p card 'do-next 0))
    (should (triage-card-offers-p card 'close 0))
    (should (triage-card-offers-p card 'peek 0))
    (should (triage-card-offers-p card 'jump 0))
    (should (equal (triage-card-label card 'jump 0) "Jump"))))

(ert-deftest triage-card-test-info-without-pane-leads-with-ack ()
  "An info card without a pane or step leads with ack and offers no close."
  (let ((card '((id . "i2") (kind . "info") (title . "Plain") (report_due . t)
                (options . (((label . "Acknowledge") (recommended . t)))))))
    (should (eq (triage-card-lead card 0) 'ack))
    (should (triage-card-offers-p card 'ack 0))
    (should-not (triage-card-offers-p card 'close 0))
    (should-not (triage-card-offers-p card 'request 0))))

(ert-deftest triage-card-test-request-report-needs-due-and-pane ()
  "Request final report needs report_due and a pane on an info card."
  (should (triage-card-offers-p
           '((id . "i3") (kind . "info") (title . "Plain") (report_due . t)
             (source . ((uri . "u") (label . "L") (pane . "%5"))))
           'request 0))
  (should-not (triage-card-offers-p
               '((id . "i4") (kind . "info") (title . "Plain")
                 (source . ((uri . "u") (label . "L") (pane . "%5"))))
               'request 0)))

(ert-deftest triage-card-test-unanswerable-ask-leads-with-peek ()
  "An ask that is not answerable and has a pane leads with peek."
  (let ((card '((id . "a1") (kind . "ask") (title . "Q") (answerable . nil)
                (source . ((uri . "u") (label . "L") (pane . "%5")))
                (options . (((label . "Yes")))))))
    (should (eq (triage-card-lead card 0) 'peek))))

(ert-deftest triage-card-test-dsh-session-pane ()
  "A dsh card takes its pane from the session uri and jumps with Open."
  (let ((card '((id . "d1") (kind . "info") (title . "Plain") (engine . "dsh")
                (source . ((uri . "dsh://session/abc") (label . "L"))))))
    (should (equal (triage-card-dsh-id card) "abc"))
    (should (equal (triage-card-pane card) "dsh:abc"))
    (should (equal (triage-card-label card 'jump 0) "Open")))
  (should-not (triage-card-dsh-id
               '((id . "d2") (kind . "info") (engine . "omp")
                 (source . ((uri . "dsh://session/abc")))))))

(ert-deftest triage-card-test-restorable ()
  "Only exited info cards are restorable."
  (should (triage-card-restorable-p '((id . "exited:%3:x") (kind . "info"))))
  (should (triage-card-restorable-p '((id . "reload:exited:%3:x") (kind . "info"))))
  (should-not (triage-card-restorable-p '((id . "exited:%3:x") (kind . "ask")))))

(ert-deftest triage-card-test-answerable ()
  "Ask and plan cards are answerable only when the daemon says so."
  (should-not (triage-card-answerable-p '((kind . "ask") (answerable . nil))))
  (should (triage-card-answerable-p '((kind . "ask") (answerable . t))))
  (should (triage-card-answerable-p '((kind . "finding"))))
  (should (triage-card-answerable-p '((kind . "info")))))

(ert-deftest triage-card-test-has-step ()
  "A step needs an info card with a non-empty action."
  (should (triage-card-has-step-p '((kind . "info") (star . ((action . "Do X"))))))
  (should-not (triage-card-has-step-p '((kind . "info") (star . ((action . ""))))))
  (should-not (triage-card-has-step-p '((kind . "ask") (star . ((action . "Do X")))))))

(ert-deftest triage-card-test-finding-other-answer ()
  "A finding allows Other only through its question and only at that step."
  (let ((bare '((id . "f1") (kind . "finding") (title . "F") (allow_other . t)
                (options . (((label . "A")))))))
    (should-not (triage-card-other-allowed-p bare 0)))
  (let ((card '((id . "f2") (kind . "finding") (title . "F") (allow_other . t)
                (questions . (((label . "Q1") (question . "?") (allow_other . t)))))))
    (should (triage-card-other-allowed-p card 0))
    (should-not (triage-card-other-allowed-p card 1))))

(ert-deftest triage-card-test-other-question-opt-out ()
  "A question with allow_other nil, or an unanswerable card, refuses Other."
  (should-not (triage-card-other-allowed-p
               '((id . "a2") (kind . "ask") (allow_other . t) (answerable . t)
                 (questions . (((label . "Q1") (question . "?") (allow_other . nil)))))
               0))
  (should-not (triage-card-other-allowed-p
               '((id . "a3") (kind . "ask") (allow_other . t) (answerable . nil)
                 (questions . (((label . "Q1") (question . "?") (allow_other . t)))))
               0)))

(ert-deftest triage-card-test-option-mode ()
  "The option label picks the dispatch mode."
  (should (equal (triage-card-option-mode "Investigate + fix it") "investigate-fix"))
  (should (equal (triage-card-option-mode " investigate") "investigate"))
  (should (equal (triage-card-option-mode "Do as recommended") "act")))

(ert-deftest triage-card-test-rec-index-and-sorted-options ()
  "Recommended options sort first and keep their original index."
  (let ((options '(((label . "A")) ((label . "B") (recommended . t)) ((label . "C")))))
    (should (= (triage-card-rec-index options) 1))
    (let ((sorted (triage-card-sorted-options options)))
      (should (equal (mapcar #'car sorted) '(1 0 2)))
      (should (equal (alist-get 'label (cdr (car sorted))) "B"))))
  (should (= (triage-card-rec-index '(((label . "A")) ((label . "B")))) 0)))

(ert-deftest triage-card-test-later-choices-tomorrow ()
  "Later offers 1 hour, 4 hours and Tomorrow, which runs to 09:00."
  (let* ((now (triage-card-test--local 2026 11 17 23 30))
         (choices (triage-card-later-choices now)))
    (should (equal (mapcar (lambda (choice) (plist-get choice :label)) choices)
                   '("1 hour" "4 hours" "Tomorrow")))
    (should (equal (mapcar (lambda (choice) (plist-get choice :minutes)) choices)
                   '(60 240 570)))))

(ert-deftest triage-card-test-later-tomorrow-crosses-month ()
  "Tomorrow from the 30th lands on 09:00 on the first of next month."
  (let* ((now (triage-card-test--local 2026 11 30 10 0))
         (tomorrow (nth 2 (triage-card-later-choices now))))
    (should (equal (format-time-string "%F %R" (plist-get tomorrow :at))
                   "2026-12-01 09:00"))
    (should (= (plist-get tomorrow :minutes) 1380))))

(ert-deftest triage-card-test-source-name ()
  "The source name drops the window prefix and the leading emoji."
  (should (equal (triage-card-source-name
                  '((label . "bill:1 🦀 fix login · omp") (target . "bill:1.0")))
                 "fix login"))
  (should (equal (triage-card-source-name '((label . "") (cwd . "/home/u/proj/")))
                 "proj"))
  (should (equal (triage-card-source-name '((label . "") (uri . "dsh://session/x")))
                 "dsh://session/x"))
  (should (equal (triage-card-source-name '()) "unknown")))

(ert-deftest triage-card-test-directory ()
  "The home directory prefix becomes a tilde."
  (should (equal (triage-card-directory "/home/nick/proj") "~/proj"))
  (should (equal (triage-card-directory "/Users/nick") "~"))
  (should (equal (triage-card-directory "/opt/data") "/opt/data")))

(ert-deftest triage-card-test-age ()
  "Age reads as now, minutes, hours or days with floor division."
  (let ((now 10000000000))
    (should (equal (triage-card-age (- now 30000) now) "now"))
    (should (equal (triage-card-age (- now 60000) now) "1m"))
    (should (equal (triage-card-age (- now (* 5 60000)) now) "5m"))
    (should (equal (triage-card-age (- now (* 3 3600000)) now) "3h"))
    (should (equal (triage-card-age (- now (* 2 86400000)) now) "2d"))))

(ert-deftest triage-card-test-effort-levels ()
  "Effort levels come from the model, or from the catalog when it has none."
  (should-not (triage-card-effort-levels triage-card-test--catalog "m"))
  (should (equal (triage-card-effort-levels triage-card-test--catalog "n")
                 '("low" "high")))
  (should (equal (triage-card-effort-levels triage-card-test--catalog "p")
                 '("max")))
  (should (equal (triage-card-effort-levels triage-card-test--catalog "gone")
                 '("low" "high"))))

(ert-deftest triage-card-test-swipe-status ()
  "A done swipe shows its reason, and other swipes show their action."
  (should (equal (triage-card-swipe-status '((action . "done") (reason . "launched %5"))
                                           "queued")
                 "done (launched %5)"))
  (should (equal (triage-card-swipe-status '((action . "done")) "queued") "done"))
  (should (equal (triage-card-swipe-status '((action . "skipped")) "queued") "skipped"))
  (should (equal (triage-card-swipe-status nil "queued") "queued")))

(ert-deftest triage-card-test-swipe-details ()
  "Swipe details join the option label, the text and the note."
  (let ((card '((id . "f") (kind . "finding") (title . "T")
                (options . (((label . "Do as recommended"))
                            ((label . "Investigate first")))))))
    (should (equal (triage-card-swipe-details
                    card '((option . 1) (text . "Investigate first")))
                   "Investigate first"))
    (should (equal (triage-card-swipe-details
                    card '((option . 0) (text . "Custom") (note . "ok")))
                   "Do as recommended · Custom · Note: ok"))))

(ert-deftest triage-card-test-result-message ()
  "Result messages name the action and end with the card title."
  (let ((finding '((id . "f") (kind . "finding") (title . "T")))
        (info '((id . "i") (kind . "info") (title . "T"))))
    (should (equal (triage-card-result-message
                    finding "dispatch" '((mode . "investigate-fix")) nil)
                   "Investigating and fixing · T"))
    (should (equal (triage-card-result-message finding "dispatch" '((mode . "act")) nil)
                   "Acting · T"))
    (should (equal (triage-card-result-message finding "dispatch" '((mode . "plan")) nil)
                   "Planning · T"))
    (should (equal (triage-card-result-message finding "skip" nil nil) "Skipped · T"))
    (should (equal (triage-card-result-message finding "close" '((note . :null)) nil)
                   "Closed · T"))
    (should (equal (triage-card-result-message finding "report" nil nil)
                   "Requested report · T"))
    (should (equal (triage-card-result-message finding "restore" nil nil)
                   "Restoring run · T"))
    (should (equal (triage-card-result-message
                    finding "later" '((minutes . 60))
                    '(:key ?1 :label "1 hour" :minutes 60 :at nil))
                   "Later 1 hour · T"))
    (should (equal (triage-card-result-message
                    info "answer" '((option . :null) (text . :null) (note . :null)) nil)
                   "Acknowledged · T"))))

(ert-deftest triage-card-test-sort-priority-then-created ()
  "Cards sort by priority, then by created time."
  (let* ((low '((id . "b") (priority . 3) (created_ms . 100)))
         (new '((id . "a") (priority . 1) (created_ms . 900)))
         (old '((id . "c") (priority . 1) (created_ms . 50)))
         (input (list low new old)))
    (should (equal (triage-card-test--ids (triage-card-sort input)) '("c" "a" "b")))
    (should (equal (triage-card-test--ids input) '("b" "a" "c")))))

(ert-deftest triage-card-test-less-p-defaults-and-id-tie ()
  "Missing priority counts as 5 and missing created time as 0."
  (should (triage-card-less-p '((id . "a") (priority . 4)) '((id . "b"))))
  (should-not (triage-card-less-p '((id . "b")) '((id . "a") (priority . 4))))
  (should (triage-card-less-p '((id . "a")) '((id . "b") (priority . 5))))
  (should (equal (triage-card-test--ids
                  (triage-card-sort '(((id . "y")) ((id . "x")))))
                 '("x" "y"))))

(ert-deftest triage-card-test-counts ()
  "Counts tally cards by kind."
  (let ((counts (triage-card-counts '(((kind . "ask")) ((kind . "finding"))
                                      ((kind . "finding")) ((kind . "info"))))))
    (should (= (alist-get 'ask counts) 1))
    (should (= (alist-get 'plan counts) 0))
    (should (= (alist-get 'finding counts) 2))
    (should (= (alist-get 'info counts) 1))))

(ert-deftest triage-card-test-action-keys-are-unique ()
  "Every action row has its own key, so the keymap cannot collide."
  (let ((keys (mapcar (lambda (row) (plist-get (cdr row) :key)) triage-card-actions)))
    (should (= (length keys) (length (delete-dups (copy-sequence keys)))))))

(provide 'triage-card-test)
;;; triage-card-test.el ends here
